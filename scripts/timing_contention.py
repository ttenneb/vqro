#!/usr/bin/env python3
"""Sample a timing step's contention; grade ONLY exit 78 as a pure timing-budget breach.

0 = met budget, 78 = timing/throughput breach with correctness verified, 77 = optional
skip, every other nonzero = correctness/runner failure. Strict grading never excuses
78. Missing sampler facts are unavailable/contended, never evidence of a pass.
"""
import argparse
from collections import Counter
import json
import os
from pathlib import Path
import select
import statistics
import sys
import time

BUDGET_EXIT = 78
SKIP_EXIT = 77


def slot_sample(directory, held):
    """Probe the wrapper's actual lock files, excluding only this run's held locks."""
    import fcntl
    if not directory or not held:
        raise OSError("gate slot metadata unavailable")
    root = Path(directory)
    files = sorted([*root.glob("slot-*.lock"), *root.glob("quick-slot-*.lock")])
    if not files:
        raise OSError("gate slot files unavailable")
    other = []
    for path in files:
        if path.name in held:
            continue
        with path.open("r+") as lock:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                other.append(path.name)
            else:
                fcntl.flock(lock, fcntl.LOCK_UN)
    return other


def identity(pid, proc=Path("/proc")):
    fields = (proc / str(pid) / "stat").read_text().rsplit(")", 1)[1].split()
    return fields[19], fields[0]  # PID birth, state (zombies are finished)


def tree_schedstat(pid, proc=Path("/proc")):
    """Per-thread schedstat (process-only schedstat omits worker-thread CPU/wait).

    Follow children of EVERY thread, including session-detached descendants. PID/TID
    birth fences reuse; cumulative maxima retain observations after a task disappears.
    Short-lived tasks between samples are not claimed as measured.
    """
    queue, seen, rows = [pid], set(), {}
    while queue:
        current = queue.pop()
        if current in seen:
            continue
        seen.add(current)
        try:
            tasks = list((proc / str(current) / "task").iterdir())
        except FileNotFoundError:
            continue
        for task in tasks:
            try:
                birth, _ = identity(int(task.name), proc)
                runtime, wait, _slices = map(int, (task / "schedstat").read_text().split())
                rows[(current, int(task.name), birth)] = (runtime, wait)
                queue.extend(map(int, (task / "children").read_text().split()))
            except FileNotFoundError:
                continue  # Normal exit between enumerating and reading a task.
    return rows


def classify(samples, cores, runtime_ns, wait_ns, unavailable):
    reasons = []
    if unavailable:
        reasons.append("unavailable")
    if any(row["other_slots"] for row in samples):
        reasons.append("other_gate_slot_held")
    running = [row["procs_running"] for row in samples if row["procs_running"] is not None]
    median = statistics.median(running) if running else None
    if median is not None and median > cores / 2:
        reasons.append("median_procs_running_above_half_cores")
    if wait_ns > runtime_ns:
        reasons.append("runqueue_wait_exceeds_runtime")
    return bool(reasons), reasons, median


def sample(pid, interval=1.0):
    samples, unavailable, totals = [], set(), {}
    directory = os.environ.get("VQRO_GATE_SLOT_DIR")
    held = set(filter(None, os.environ.get("VQRO_GATE_SLOT_HELD", "").split(",")))
    started = time.monotonic()
    cores = os.cpu_count() or 1
    notification = None
    try:
        notification = os.pidfd_open(pid)
    except (AttributeError, OSError):
        unavailable.add("step exit notification unavailable")
    try:
        birth, _ = identity(pid)
    except (OSError, ValueError, IndexError):
        birth = None
        unavailable.add("step exited before schedstat observation")
    def root_live():
        if birth is None:
            return False
        try:
            now_birth, state = identity(pid)
            return now_birth == birth and state not in ("Z", "X")
        except (FileNotFoundError, ProcessLookupError):
            return False  # Normal completion is not missing sampler evidence.
        except (OSError, ValueError, IndexError):
            unavailable.add("root liveness unavailable")
            return False

    def exited():
        return notification is not None and bool(select.select([notification], [], [], 0)[0])

    discarded, end = 0, "root_exit"
    while True:
        # Bookend each retained snapshot. A row gathered across root exit cannot
        # attribute a later gate's lock/runnable count to this step's lifetime.
        if exited():
            end = "pidfd_exit"
            break
        if not root_live():
            break
        row = {"after_s": round(time.monotonic() - started, 3), "other_slots": [], "procs_running": None}
        row_errors, latest = set(), {}
        try:
            row["other_slots"] = slot_sample(directory, held)
        except (OSError, ImportError) as error:
            row_errors.add(str(error))
        try:
            row["procs_running"] = int(next(line.split()[1] for line in Path("/proc/stat").read_text().splitlines()
                                           if line.startswith("procs_running ")))
        except (OSError, ValueError, StopIteration):
            row_errors.add("procs_running unavailable")
        try:
            latest = tree_schedstat(pid)
            if not latest:
                row_errors.add("process-tree schedstat unavailable")
        except (OSError, ValueError, IndexError):
            row_errors.add("process-tree schedstat unavailable")
        if exited() or (notification is None and not root_live()):
            discarded += 1
            end = "exit_during_snapshot"
            break  # Neither the row nor its errors/counters describe a known live window.
        samples.append(row)
        unavailable.update(row_errors)
        for key, value in latest.items():
            previous = totals.get(key, (0, 0))
            totals[key] = tuple(max(a, b) for a, b in zip(previous, value))
        if notification is not None:
            if select.select([notification], [], [], interval)[0]:
                end = "pidfd_exit"
                break  # Do not take a post-exit lock/procs sample.
        else:
            time.sleep(interval)
    if notification is not None:
        os.close(notification)
    if not totals:
        unavailable.add("no process-tree schedstat samples")
    runtime_ns = sum(value[0] for value in totals.values())
    wait_ns = sum(value[1] for value in totals.values())
    contended, reasons, median = classify(samples, cores, runtime_ns, wait_ns, unavailable)
    return {"kind": "vqro.timing-contention", "version": 1,
            "status": "unavailable" if unavailable else "available", "unavailable": sorted(unavailable),
            "contended": contended, "reasons": reasons, "cores": cores,
            "median_procs_running": median, "runtime_ns": runtime_ns, "runqueue_wait_ns": wait_ns,
            "observed_tasks": len(totals), "sample_count": len(samples),
            "samples": samples[:600], "samples_truncated": len(samples) > 600,
            "end_observation": end, "post_exit_samples_discarded": discarded,
            "coverage": "retained snapshots bracketed by root liveness; final counters may precede exit",
            "elapsed_s": round(time.monotonic() - started, 3)}


def grade(exit_code, facts, strict):
    if exit_code == 0:
        return 0, "met budget (contention does not invalidate a pass)"
    if exit_code == SKIP_EXIT:
        return SKIP_EXIT, "optional dependency unavailable"
    if exit_code != BUDGET_EXIT:
        return 1, "correctness or runner failure (never downgraded)"
    if strict:
        return 1, "timing budget breach (strict grading)"
    if facts.get("contended") or facts.get("status") != "available":
        return SKIP_EXIT, "unverified (contended)"
    return 1, "timing budget breach (no measured contention)"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pid", type=int)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--grade", type=int, metavar="STEP_EXIT")
    parser.add_argument("--strict", action="store_true")
    args = parser.parse_args()
    if args.grade is None:
        if args.pid is None:
            parser.error("--pid is required when sampling")
        facts = sample(args.pid)
        code = 0
    else:
        try:
            facts = json.loads(args.output.read_text())
        except (OSError, ValueError):
            facts = {"kind": "vqro.timing-contention", "version": 1, "status": "unavailable",
                     "contended": True, "unavailable": ["sampler fact file unavailable"]}
        code, reason = grade(args.grade, facts, args.strict)
        facts.update(step_exit=args.grade, graded_exit=code, strict=args.strict, grading_reason=reason)
        print(reason)
    args.output.write_text(json.dumps(facts, indent=1) + "\n")
    return code


if __name__ == "__main__":
    sys.exit(main())
