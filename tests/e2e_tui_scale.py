#!/usr/bin/env python3
"""Opt-in real-TUI scale probe, entirely private: --fake core and explicit private tmux socket.
Usage: python3 tests/e2e_tui_scale.py TARGET [--duration 15] [--receipt FILE]
CPU is percent of one core; latency is tmux send-keys completion to first capture-pane observation.
Performance budgets are enforced, as are correctness and owned-process cleanup. BUDGETS are set from measured
results (docs/test-plan.md, "Scale budgets"): twice the worst of the current check.sh full runs, rounded up to 10;
frame-rate limits are the renderer's design caps (background redraw is capped near 60 fps).
Receipts record screen deadline misses and associated historical stream signals, never inferred loss.
A complete token is positive screen evidence even when late. Key/stream evidence is capped with explicit
dropped counts; missing or truncated metadata is labelled incomplete and is not associated with keys.
"""
import argparse
from collections import deque
import json
import math
import os
from pathlib import Path
import queue
import re
import shutil
import socket
import subprocess
import tempfile
import threading
import time

# measure -> (limit, worst observed in 7 runs on 2026-11-17)
BUDGETS = {"cpu_pct_one_core": (40, 18.0), "rss_peak_mib": (80, 38.6), "key_p99_ms": (170, 81.8),
           "background_fps_peak_complete_second": (64, 55), "fps_peak_complete_second": (120, 94)}


KEY_DEADLINE_SECONDS = 2
KEY_LEDGER_CAP = 512
TOKEN = re.compile(r"K[0-9]{5}Z")
STREAM_EVENT_CAP = 256
ERROR_KINDS = {"invalid", "conflict", "not_found", "unavailable", "forbidden", "backend", "disconnected",
               "io", "durability_unconfirmed", "timeout", "overloaded", "unsupported", "stale_generation", "too_large", "other"}
CLOSE_REASONS = {"slow_consumer", "closed", "detached", "finished", "invalid", "provider_disconnected", "viewer_disconnected", "other"}


def stream_evidence_complete(metrics):
    """Validate the bounded categorical schema before associating historical signals."""
    def number(value):
        return type(value) in (int, float) and 0 <= value <= 1e15  # Also excludes NaN/infinity.
    def ident(value):
        return type(value) is int and 0 <= value < 2**64
    def resource(value):
        return (isinstance(value, str) and 0 < len(value) <= 128 and value.isascii() and
                all(c.isalnum() or c in "-._:" for c in value))
    if not isinstance(metrics, dict) or not number(metrics.get("started_unix_s")):
        return False
    if type(metrics.get("stream_events_dropped")) is not int or metrics["stream_events_dropped"] != 0:
        return False
    if type(metrics.get("stream_events_cap")) is not int or metrics["stream_events_cap"] != STREAM_EVENT_CAP:
        return False
    total = 0
    for field, methods, payload in (
            ("stream_attachments", {"resource.attach"}, "resource"),
            ("stream_errors", {"stream.error"}, "kind"),
            ("stream_closures", {"stream.closed", "stream.gap"}, "reason")):
        events = metrics.get(field)
        if not isinstance(events, list):
            return False
        total += len(events)
        if total > STREAM_EVENT_CAP:
            return False
        for event in events:
            if not isinstance(event, dict) or set(event) != {"at_s", "method", "connection", "stream", payload}:
                return False
            if (not isinstance(event["method"], str) or event["method"] not in methods or not number(event["at_s"]) or
                    not ident(event["stream"]) or not ident(event["connection"])):
                return False
            value = event[payload]
            if payload == "resource" and not resource(value):
                return False
            if payload == "kind" and (not isinstance(value, str) or value not in ERROR_KINDS):
                return False
            if payload == "reason" and (not isinstance(value, dict) or set(value) != {"reason"} or
                    not isinstance(value["reason"], str) or value["reason"] not in CLOSE_REASONS):
                return False
    return True


class KeyLedger:
    """Bounded screen observations, not an assertion of backend input acceptance.

    Local monotonic times measure latency; the wall-clock origins only correlate
    the harness with the separate TUI's diagnostic stream timestamps.
    """
    def __init__(self, started_unix_s, capacity=KEY_LEDGER_CAP):
        self.started_unix_s = started_unix_s
        self.records = deque(maxlen=capacity)
        self.by_token = {}
        self.dropped = 0

    def sent(self, token, at_s):
        if len(self.records) == self.records.maxlen:
            self.by_token.pop(self.records[0]["token"])
            self.dropped += 1
        record = {"token": token, "sent_s": at_s, "seen_s": None, "missed_deadline": False}
        self.records.append(record)
        self.by_token[token] = record

    def observe(self, screen, at_s):
        for token in TOKEN.findall(screen):
            record = self.by_token.get(token)
            if record is not None and record["seen_s"] is None:
                record["seen_s"] = at_s

    def finish(self, token):
        record = self.by_token[token]
        record["missed_deadline"] = (record["seen_s"] is None or
            record["seen_s"] - record["sent_s"] > KEY_DEADLINE_SECONDS)

    def summary(self, metrics, idle_resource, flood_resource, elapsed_s):
        complete = self.dropped == 0 and stream_evidence_complete(metrics)
        origin = metrics["started_unix_s"] - self.started_unix_s if complete else 0
        events = sorted((event for field in ("stream_attachments", "stream_errors", "stream_closures")
                         for event in metrics[field]), key=lambda event: event["at_s"]) if complete else []
        records = [dict(row, classification="observed" if row["seen_s"] is not None else "unknown",
                        associated_signals=[], evidence_status="complete" if complete else "evidence incomplete")
                   for row in self.records]
        for index, row in enumerate(records):
            sent = row["sent_s"]
            row["missed_deadline"] |= (row["seen_s"] is None and elapsed_s - sent > KEY_DEADLINE_SECONDS)
            if row["seen_s"] is not None:
                row["missed_deadline"] |= row["seen_s"] - sent > KEY_DEADLINE_SECONDS
            end = records[index + 1]["sent_s"] if index + 1 < len(records) else elapsed_s
            if row["missed_deadline"]:
                # Time-window association only: neither stream errors nor attach replies identify
                # a key's forwarding stream, acceptance, rejection or eventual delivery.
                row["associated_signals"] = [dict(event, at_s=event["at_s"] + origin, classification="unknown") for event in events
                                             if sent <= event["at_s"] + origin < end]
        latencies = sorted((row["seen_s"] - row["sent_s"]) * 1000 for row in records if row["seen_s"] is not None)
        return {"samples": len(latencies), "missed_deadline": [r["token"] for r in records if r["missed_deadline"]],
                "resources": {"idle": idle_resource, "flood": flood_resource},
                "signal_scope": "historical time-window association, not a forwarding witness",
                "p50_ms": latencies[len(latencies)//2] if latencies else None,
                "p99_ms": latencies[min(len(latencies)-1, int(len(latencies)*.99))] if latencies else None,
                "max_ms": max(latencies, default=None), "ledger": records, "ledger_dropped": self.dropped,
                "started_unix_s": self.started_unix_s, "stream_evidence_complete": complete,
                "evidence_status": "complete" if complete else "evidence incomplete"}


def finite_measure(value):
    # Preserve the probe's existing zero-as-missing guard as well as finiteness.
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value) and value > 0


def exceeded(measured):
    """Missing/non-finite evidence is invalid, never an apparently met budget."""
    return sorted(name for name, (limit, _) in BUDGETS.items()
                  if not finite_measure(measured.get(name)) or measured[name] > limit)


def timing_exit(receipt):
    """78 is ONLY a finite key-latency breach after correctness/cleanup passed.

    Memory/CPU/frame caps, missing measurements, invalid workload, lost inputs,
    exceptions and leftovers remain hard failures, even on a contended machine.
    """
    keys, tui, events = (receipt.get(name) for name in ("keys", "tui", "events"))
    extensions, cleanup = receipt.get("extensions"), receipt.get("cleanup")
    if not all(isinstance(value, dict) for value in (keys, tui, events, extensions, cleanup)):
        return 1
    sidebar = extensions.get("sidebar")
    if not isinstance(sidebar, dict):
        return 1
    if (receipt.get("error") is not None or receipt.get("leftover_processes") != []
        or receipt.get("correctness_ok") is not True or receipt.get("workload_valid") is not True
        or type(keys.get("samples")) is not int or keys["samples"] <= 0 or keys.get("lost") != []
        or type(events.get("missed")) is not int or events["missed"] != 0
        or sidebar.get("status") != "running" or type(sidebar.get("crashes")) is not int or sidebar["crashes"] != 0
        or cleanup.get("ok") is not True or cleanup.get("shutdown_requested") is not True
        or cleanup.get("forced_kill") is not False or type(cleanup.get("core_exit")) is not int or cleanup["core_exit"] != 0
        or any(cleanup.get(name, "missing") is not None for name in ("shutdown_error", "client_close_error", "wait_error"))):
        return 1
    measured = {**tui, "key_p99_ms": keys.get("p99_ms")}
    if not all(finite_measure(measured.get(key)) for key in BUDGETS):
        return 1
    # Derive from actual evidence; neither a PASS tag nor a claimed breach list
    # can bypass validation, invent a breach or suppress a measured one.
    over = exceeded(measured)
    return 0 if not over else 78 if over == ["key_p99_ms"] else 1


def cleanup_core(receipt, client, core):
    """Only an observed successful shutdown/exit can complete correctness proof."""
    cleanup = dict(shutdown_requested=False, shutdown_error=None, client_close_error=None,
                   core_exit=None, forced_kill=False, wait_error=None)
    if client is not None:
        cleanup["shutdown_requested"] = True
        try:
            client.call("core.shutdown")
        except Exception as error:
            cleanup["shutdown_error"] = f"{type(error).__name__}: {error}"
        try:
            client.close()
        except Exception as error:
            cleanup["client_close_error"] = f"{type(error).__name__}: {error}"
    if core is not None:
        try:
            cleanup["core_exit"] = core.wait(timeout=5)
        except subprocess.TimeoutExpired:
            cleanup["forced_kill"] = True
            try:
                core.kill()
                cleanup["core_exit"] = core.wait()
            except Exception as error:
                cleanup["wait_error"] = f"{type(error).__name__}: {error}"
        except Exception as error:
            cleanup["wait_error"] = f"{type(error).__name__}: {error}"
    cleanup["ok"] = bool(cleanup["shutdown_requested"] and cleanup["core_exit"] == 0
        and not cleanup["forced_kill"] and not any(cleanup[key] for key in
        ("shutdown_error", "client_close_error", "wait_error")))
    receipt["cleanup"] = cleanup
    if not cleanup["ok"]:
        receipt.update(result="fail", correctness_ok=False)


class Peer:
    def __init__(self, path, note=lambda _: None):
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.settimeout(15)
        self.sock.connect(str(path))
        self.sock.settimeout(None)
        self.file = self.sock.makefile("rwb", buffering=0)
        self.pending, self.lock, self.next, self.note = {}, threading.Lock(), 1, note
        self.reader = threading.Thread(target=self.read, daemon=True)
        self.reader.start()
        self.call("hello", {"client": "tui-scale-probe"})

    def read(self):
        try:
            while True:
                line = self.file.readline(64 * 1024 * 1024 + 1)
                if not line:
                    break
                message = json.loads(line)
                if "id" in message:
                    with self.lock:
                        waiter = self.pending.get(message["id"])
                    if waiter:
                        waiter.put(message)
                else:
                    self.note(message)
        except (OSError, ValueError):
            pass

    def call(self, method, params=None):
        waiter = queue.Queue(maxsize=1)
        with self.lock:
            ident, self.next = self.next, self.next + 1
            self.pending[ident] = waiter
            self.file.write((json.dumps({"jsonrpc": "2.0", "id": ident, "method": method, "params": params or {}}) + "\n").encode())
        try:
            result = waiter.get(timeout=15)
            if "error" in result:
                raise RuntimeError(f"{method}: {result['error']}")
            return result["result"]
        finally:
            with self.lock:
                self.pending.pop(ident, None)

    def close(self):
        try:
            self.sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        self.sock.close()


def wait(predicate, seconds=10):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        result = predicate()
        if result:
            return result
        time.sleep(.02)
    raise TimeoutError("private probe condition")


def process(pid):
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    cpu = (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")
    rss = int(Path(f"/proc/{pid}/status").read_text().split("VmRSS:")[1].split()[0]) / 1024
    return cpu, rss, fields[19]


def owned(root):
    """Exact private root, including socket inode ownership when tmux scrubs/renames itself."""
    sockets = {}
    for line in Path("/proc/net/unix").read_text().splitlines()[1:]:
        fields = line.split()
        if len(fields) >= 8 and fields[7].startswith(str(root) + "/"):
            sockets[fields[6]] = fields[7]
    result = []
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit() or int(entry.name) == os.getpid():
            continue
        try:
            hit = str(root).encode() in (entry / "environ").read_bytes()
            if not hit:
                for fd in (entry / "fd").iterdir():
                    link = os.readlink(fd)
                    if link.startswith("socket:[") and link[8:-1] in sockets:
                        hit = True
                        break
            if hit:
                result.append((int(entry.name), (entry / "stat").read_text().rsplit(")", 1)[1].split()[19]))
        except OSError:
            pass
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target", type=Path)
    parser.add_argument("--duration", type=float, default=15)
    parser.add_argument("--terminals", type=int, default=600)
    parser.add_argument("--agents", type=int, default=200)
    parser.add_argument("--receipt", type=Path)
    args = parser.parse_args()
    binary = args.target.resolve() / "release/vqro"
    if not binary.exists():
        parser.error(f"build release binary first: {binary}")
    repo = Path(__file__).resolve().parent.parent
    root = Path(tempfile.mkdtemp(prefix="ptus", dir="/tmp"))
    for name in ("home", "st", "ext", "cfg", "run"):
        (root / name).mkdir()
    # Only sidebar: no unrelated policies/workspace scanners or real backend extensions.
    for source in (repo / "extensions").glob("*.py"):
        shutil.copy(source, root / "ext" / source.name)
    (root / "run").chmod(0o700)
    env = {"HOME": str(root / "home"), "PATH": "/usr/bin:/bin", "TERM": "xterm-256color", "LANG": "C.UTF-8",
           "SHELL": "/bin/sh", "VQRO_STATE_DIR": str(root / "st"), "VQRO_SOCKET": str(root / "p.sock"),
           "VQRO_EXTENSIONS_DIR": str(root / "ext"), "VQRO_HERDR": "/bin/false", "HC_SOCKET": str(root / "unused-hc.sock"),
           "HC_SESSION": "private-tui-scale", "XDG_CONFIG_HOME": str(root / "cfg"), "XDG_RUNTIME_DIR": str(root / "run"),
           "VQRO_TUI_PROBE": str(root / "tui-metrics.json")}
    harness = ["/usr/bin/tmux", "-S", str(root / "tmux.sock"), "-f", "/dev/null"]
    def tmux(*command):
        return subprocess.run([*harness, *([] if command[0] == "new-session" else ["-N"]), *command], env=env, capture_output=True, timeout=5, check=True).stdout.decode()
    client, observer, core = None, None, None
    receipt = {"kind": "vqro.tui-scale", "version": 1, "profile": {"terminals": args.terminals, "agents": args.agents,
               "status_per_second": args.agents / 2, "flood_bytes_per_second": 1_000_000, "views": 3, "sidebar": True, "duration_s": args.duration},
               "budgets": {"enforced": True, **{name: limit for name, (limit, _) in BUDGETS.items()}}}
    events = {"resource": 0, "extension": 0, "missed": 0}
    ledger, metrics, elapsed = None, None, None
    try:
        log = (root / "serve.log").open("wb")
        core = subprocess.Popen([str(binary), "serve", "--fake"], env=env, stdout=log, stderr=subprocess.STDOUT)
        wait(lambda: (root / "p.sock").exists())
        client = Peer(root / "p.sock")
        wait(lambda: client.call("workspace.snapshot")["providers"]["fake"]["synced"])
        receipt["stage"] = "spawn-resources"
        resources = []
        for index in range(args.terminals):
            argv = ["agent", "--status-every-ms", "2000", "--status-phase-ms", str(index * 2000 // max(1, args.agents) + 1)] if index < args.agents else ["sh"]
            if index == args.agents:
                argv = ["flood", "--output-bytes-per-sec", "1000000"]
            resources.append(client.call("resource.spawn", {"argv": argv, "name": f"probe-{index}"})["resource"]["id"])
        client.call("workspace.apply", {"ops": [{"op": "set", "key": "extension/sidebar", "value": {"argv": ["/usr/bin/python3", str(root / "ext/sidebar.py")]}}]})
        wait(lambda: client.call("workspace.snapshot")["live"].get("sidebar"))
        views = {"probe-input": resources[-1], "probe-flood": resources[args.agents], "probe-other": resources[-2]}
        ops = [{"op": "set", "key": f"view/{name}", "value": {"kind": "terminal", "resource": rid, "title": name}} for name, rid in views.items()]
        layout = {"split": "h", "sizes": [1, 3], "children": [{"view": "sidebar"}, {"split": "v", "children": [
            {"view": "probe-input"}, {"split": "h", "children": [{"view": "probe-flood"}, {"view": "probe-other"}]}]}]}
        ops += [{"op": "set", "key": "layout/main", "value": layout}, {"op": "set", "key": "workspace/active", "value": {"layout": "main"}}]
        client.call("workspace.apply", {"note": "private TUI scale layout", "ops": ops})
        wait(lambda: client.call("workspace.snapshot")["live"].get("sidebar"))
        receipt["stage"] = "start-tui"
        tmux("new-session", "-d", "-x", "160", "-y", "44", str(binary), "tui")
        pid = int(tmux("display-message", "-p", "#{pane_pid}").strip())
        wait(lambda: "probe-input" in tmux("capture-pane", "-p"))
        time.sleep(1)
        # SGR click: focus the idle top-right terminal, never the sidebar or flood.
        tmux("send-keys", "-H", *[f"{b:02x}" for b in b"\x1b[<0;80;5M\x1b[<0;80;5m"])
        def note(message):
            kind = message.get("params", {}).get("type", message.get("method"))
            if kind in events:
                events[kind] += 1
        observer = Peer(root / "p.sock", note)
        start_snapshot = client.call("workspace.snapshot")
        observer.call("events.subscribe", {"from": start_snapshot["seq"], "epoch": start_snapshot["epoch"], "types": list(events), "partial_resources": True})
        receipt["stage"] = "measurement"
        cpu_start, rss_start, birth = process(pid)
        start = time.monotonic()
        ledger = KeyLedger(time.time())
        samples = []
        def capture():
            screen = tmux("capture-pane", "-p")
            ledger.observe(screen, time.monotonic() - start)
            return screen
        index = 0
        while time.monotonic() - start < args.duration:
            token = f"K{index:05d}Z"
            ledger.sent(token, time.monotonic() - start)
            # Retain earlier echoes on separate rows; the next send must not erase a late token.
            tmux("send-keys", "-l", "\r\n" + token)
            try:
                wait(lambda: token in capture(), KEY_DEADLINE_SECONDS)
            except TimeoutError:
                pass  # Missing the screen deadline alone is not evidence of lost input.
            ledger.finish(token)
            cpu, rss, _ = process(pid)
            samples.append((time.monotonic() - start, cpu - cpu_start, rss))
            index += 1
            time.sleep(.2)
        elapsed = time.monotonic() - start
        cpu_end, rss_end, _ = process(pid)
        end_snapshot = client.call("workspace.snapshot")
        capture()  # One final observation, not an additional wait or retry.
        receipt["stage"] = "quit"
        tmux("send-keys", "C-g")
        time.sleep(.1)
        tmux("send-keys", "q")
        wait(lambda: not Path(f"/proc/{pid}").exists(), 5)
        metrics = json.loads((root / "tui-metrics.json").read_text())
        complete_buckets = [row for row in metrics["frames_by_second"] if 1 < row[0] < int(metrics["elapsed_s"])]
        complete = [row[1] for row in complete_buckets]
        background = [row[2] for row in complete_buckets]
        received_bps = metrics["received_stream_bytes"] / metrics["elapsed_s"]
        workload_valid = received_bps >= 900_000 and events["resource"] / elapsed >= args.agents / 2 * .9
        keys = ledger.summary(metrics, resources[-1], resources[args.agents], elapsed)
        receipt.update({"tui": {"cpu_pct_one_core": ((cpu_end - cpu_start) / elapsed) * 100, "rss_start_mib": rss_start,
            "rss_peak_mib": max(row[2] for row in samples), "frames": metrics["frames"], "fps_mean_complete_seconds": sum(complete) / max(1, len(complete)),
            "fps_peak_complete_second": max(complete, default=0), "background_fps_peak_complete_second": max(background, default=0), "draw_total_ms": metrics["draw_total_ms"], "draw_max_ms": metrics["draw_max_ms"]},
            "keys": keys,
            "events": events, "extensions": end_snapshot["extensions"], "status_per_second_observed": events["resource"] / elapsed,
            "sidebar_publishes_estimated_per_second": (end_snapshot["seq"] - start_snapshot["seq"] - events["resource"] - events["extension"]) / elapsed,
            "received_stream_bytes_per_s": received_bps, "workload_valid": workload_valid,
            "metrics": metrics})
        over = exceeded({**receipt["tui"], "key_p99_ms": receipt["keys"]["p99_ms"]})
        receipt["budgets_exceeded"] = over
        receipt["correctness_ok"] = bool(workload_valid and not keys["missed_deadline"] and not keys["ledger_dropped"] and keys["stream_evidence_complete"] and not events["missed"] and keys["samples"] and end_snapshot["extensions"]["sidebar"]["status"] == "running" and end_snapshot["extensions"]["sidebar"]["crashes"] == 0)
        receipt["result"] = "pass" if receipt["correctness_ok"] and not over else "fail"
    except Exception as error:
        receipt.update(result="fail", error=f"{type(error).__name__}: {error}")
        try:
            receipt["screen_on_failure"] = tmux("capture-pane", "-p")
            if ledger is not None:
                ledger.observe(receipt["screen_on_failure"], time.monotonic() - start)
        except Exception:
            pass
    finally:
        subprocess.run([*harness, "-N", "kill-server"], env=env, capture_output=True, timeout=5)
        if observer:
            observer.close()
        cleanup_core(receipt, client, core)
        time.sleep(.1)
        left = owned(root)
        receipt["leftover_processes"] = left
        if left:
            receipt["result"] = "fail"
            # Exact private-root membership and PID birth are checked again before any signal.
            for pid, birth in left:
                try:
                    if process(pid)[2] == birth:
                        os.kill(pid, 9)
                except (OSError, IndexError):
                    pass
        if receipt.get("result") != "pass":
            receipt["serve_log"] = (root / "serve.log").read_text(errors="replace")[-4000:]
        if ledger is not None:
            # Preserve partial evidence on exceptions too, before deleting the private root.
            if metrics is None:
                try:
                    metrics = json.loads((root / "tui-metrics.json").read_text())
                except (OSError, ValueError):
                    metrics = {}
                receipt["metrics"] = metrics
            receipt["keys"] = ledger.summary(metrics, resources[-1], resources[args.agents],
                                              elapsed if elapsed is not None else time.monotonic() - start)
        shutil.rmtree(root)
    code = timing_exit(receipt)
    receipt["failure_class"] = {0: "pass", 78: "budget"}.get(code, "correctness")
    receipt["result"] = "pass" if code == 0 else "fail"
    wire = json.dumps(receipt, indent=2) + "\n"
    if args.receipt:
        args.receipt.parent.mkdir(parents=True, exist_ok=True)
        args.receipt.write_text(wire)
    print(wire)
    return code


if __name__ == "__main__":
    raise SystemExit(main())
