#!/usr/bin/env python3
"""Independent performance contract controls, always using a private source core.

Use gate-slot.sh for every invocation. --source-only needs no perf implementation.
Performance ceiling breaches must warn, never fail. No wall-clock speed assertions
are made against the loaded host: purpose-built fixtures provide large separations
or deterministic overlapping spreads. Failed runs retain evidence, not processes.
"""
import argparse
from collections import Counter
import hashlib
import json
import math
import os
from pathlib import Path
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time

from e2e_testing_integration import Checks, Driver, Source, all_rows, process_group_members, root_paths

REPO = Path(__file__).resolve().parents[1]
BASE = '''import json, os, threading, time
from pathlib import Path
from vqro_ext import Extension
ext = Extension(event_types=[])
ext.contribute({"view/ctl-perf.panel": {"kind": "panel", "title": "Performance oracle fixture"}})
ext.publish("ctl-perf.panel", {"items": [{"text": "quiet"}]})
DELAY_MS = 0
PUBLISH_LOOP = False
CONTROLLED = False
MODE = "baseline"
serial = 0
@ext.command("ctl.perf", "Independent measured command")
def measured(args):
    global serial
    serial += 1
    delay = DELAY_MS
    if CONTROLLED:
        folder = Path(os.environ["VQRO_STATE_DIR"]) / "perf-probe"
        folder.mkdir(exist_ok=True)
        request = folder / ("request-" + str(serial) + ".json")
        response = folder / ("response-" + str(serial) + ".json")
        request.write_text(json.dumps({"mode": MODE, "serial": serial, "pid": os.getpid()}))
        deadline = time.monotonic() + 15
        while not response.exists():
            if time.monotonic() >= deadline:
                raise RuntimeError("independent timing fixture coordinator unavailable")
            time.sleep(0.005)
        delay = json.loads(response.read_text())["delay_ms"]
    time.sleep(delay / 1000)
    return {"ok": True}
if PUBLISH_LOOP:
    def publish_forever():
        serial = 0
        while True:
            serial += 1
            ext.publish("ctl-perf.panel", {"items": [{"text": "idle publication " + str(serial)}]})
            time.sleep(0.05)
    threading.Thread(target=publish_forever, daemon=True).start()
ext.run()
'''
SLOW = BASE.replace("DELAY_MS = 0", "DELAY_MS = 200")
IDLE = BASE.replace("PUBLISH_LOOP = False", "PUBLISH_LOOP = True")
CONTROL_BASE = BASE.replace("CONTROLLED = False", "CONTROLLED = True")
CONTROL_CANDIDATE = CONTROL_BASE.replace('MODE = "baseline"', 'MODE = "candidate"')


def state():
    git = subprocess.run(["git", "rev-parse", "HEAD"], cwd=REPO,
                         env={"HOME": os.environ["HOME"], "PATH": os.environ.get("PATH", "/usr/bin:/bin")},
                         capture_output=True, text=True, timeout=10)
    return {"head": git.stdout.strip(), "sha256": {
        name: hashlib.sha256((REPO / "extensions" / name).read_bytes()).hexdigest()
        for name in ("testing_perf.py", "vqro_testing.py") if (REPO / "extensions" / name).is_file()}}


def timing(name="timing"):
    return {"name": name, "steps": [{"invoke": "ctl.perf", "args": {}, "result": {"ok": True}}]}


def measurement_rows(section):
    """The contract's measurement scopes are recognizable independently of display rows."""
    if isinstance(section, dict):
        if "id" in section and isinstance(section.get("measure"), dict) and "samples" in section["measure"]:
            yield {**section, **section["measure"]}
        else:
            for value in section.values():
                yield from measurement_rows(value)
    elif isinstance(section, list):
        for value in section:
            yield from measurement_rows(value)


def scope_rows(rows, scope):
    return [row for row in rows if scope in row.get("id", "")]


def configure(source, script=BASE, scenarios=(), thresholds=None):
    source.configure([source.fixture("ctl-inspect", script, provides=("command/ctl.perf", "view/ctl-perf.panel"))], scenarios)
    present = "doc/settings/test-perf" in source.call("workspace.snapshot")["entries"]
    if thresholds is not None:
        source.call("workspace.apply", {"ops": [{"op": "set", "key": "doc/settings/test-perf", "value": thresholds}]})
    elif present:
        source.call("workspace.apply", {"ops": [{"op": "delete", "key": "doc/settings/test-perf"}]})


class Responder:
    """Feed a known overlapping delay sequence to fixture commands in each sandbox.

    Requests are discovered by owned files, not implementation directory names or
    repeat indices. All responses are written inside that requesting sandbox.
    """
    PLAN = {"baseline": [0, 10, 200, 400, 500], "candidate": [100, 300, 400, 600, 700]}

    def __init__(self, root):
        self.root = root
        self.stop = threading.Event()
        self.observed = []
        self.errors = []
        self.thread = threading.Thread(target=self.serve, daemon=True)

    def serve(self):
        seen = set()
        indices = Counter()
        while not self.stop.is_set():
            try:
                # Discover state directories from our independent exec audit, not
                # an implementation naming scheme; avoid walking copied packages
                # during idle measurement.
                log = self.root / "binary-launches.jsonl"
                states = set()
                if log.exists():
                    for line in log.read_text().splitlines():
                        try:
                            record = json.loads(line)
                        except ValueError:
                            continue
                        value = record.get("env", {}).get("VQRO_STATE_DIR")
                        if value and Path(value).is_relative_to(self.root):
                            states.add(Path(value))
                requests = [request for state in states for request in (state / "perf-probe").glob("request-*.json")]
                for request in sorted(requests):
                    if request in seen:
                        continue
                    try:
                        payload = json.loads(request.read_text())
                    except (OSError, ValueError):
                        continue
                    mode = payload["mode"]
                    index = indices[mode]
                    if index >= len(self.PLAN[mode]):
                        raise RuntimeError("unexpected extra command invocation: " + str(payload))
                    delay = self.PLAN[mode][index]
                    response = request.with_name(request.name.replace("request-", "response-"))
                    temporary = response.with_suffix(".new")
                    temporary.write_text(json.dumps({"delay_ms": delay}))
                    os.replace(temporary, response)
                    self.observed.append({"mode": mode, "delay_ms": delay, "request": str(request)})
                    indices[mode] += 1
                    seen.add(request)
            except Exception as error:
                self.errors.append(repr(error))
                return
            self.stop.wait(0.01)

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *ignored):
        self.stop.set()
        self.thread.join(timeout=5)


class PerfDriver(Driver):
    def run_perf(self, label, extra=(), repeats=5, candidate=None):
        self.index += 1
        root = self.root / ("r" + str(self.index))
        before = self.source.snapshot()
        launch_start = len(self.launches())
        args = ["python3", str(self.source.ext / "testing_perf.py"), "--json", "--vqro", str(self.launcher),
                "--backend", "fake", "--root", str(root), "--window", "1"]
        if repeats is not None:
            args += ["--repeats", str(repeats)]
        if candidate is not None:
            args += ["--candidate-ext", str(candidate)]
        args += list(map(str, extra))
        process = subprocess.run(args, cwd=self.source.root,
                                 env=dict(self.source.env, HERDR_PANE_ID="ambient-control", TMUX="ambient-control"),
                                 capture_output=True, text=True, timeout=self.timeout)
        (self.root / (label + ".stdout")).write_text(process.stdout)
        (self.root / (label + ".stderr")).write_text(process.stderr)
        try:
            receipt = json.loads(process.stdout)
        except ValueError:
            self.checks.check(label + ": JSON receipt only", False,
                              {"exit": process.returncode, "stdout": process.stdout[-3000:], "stderr": process.stderr[-3000:]})
            receipt = {}
        after = self.source.snapshot()
        self.checks.check(label + ": source seq and entries untouched", before[0]["seq"] == after[0]["seq"]
                          and before[0]["entries"] == after[0]["entries"])
        self.checks.check(label + ": source generations untouched", {n: r["generation"] for n, r in before[1].items()} ==
                          {n: r["generation"] for n, r in after[1].items()})
        self.checks.check(label + ": receipt schema", receipt.get("kind") == "vqro.test-receipt" and
                          receipt.get("version") == 1 and receipt.get("suite") == "perf", receipt)
        counts = receipt.get("counts", {})
        row_counts = Counter(row.get("status") for row in receipt.get("rows", []))
        self.checks.check(label + ": counts match top-level rows", bool(counts) and
                          sum(counts.values()) == len(receipt.get("rows", [])) and
                          all(counts.get(status) == count for status, count in row_counts.items()), counts)
        launches = [row for row in self.launches()[launch_start:] if "serve" in row["argv"]]
        expected_repeats = 5 if repeats is None else repeats
        expected_launches = max(0, expected_repeats) * (2 if candidate is not None else 1)
        self.checks.check(label + ": one fresh core per repeat and side", len(launches) == expected_launches,
                          {"expected": expected_launches, "observed": len(launches)})
        for launch in launches:
            self.checks.check(label + ": private core process group", launch["pid"] == launch["pgid"] == launch["sid"], launch)
            for key in ("VQRO_SOCKET", "VQRO_STATE_DIR", "VQRO_EXTENSIONS_DIR", "HC_SOCKET", "XDG_RUNTIME_DIR"):
                self.checks.check(label + ": private launch " + key, Path(launch["env"].get(key, "/")).is_relative_to(root), launch)
            self.checks.check(label + ": no ambient pane variables", not any(key.startswith(("HERDR_", "TMUX")) for key in launch["env"]), launch)
        self.checks.check(label + ": deleted requested root", not root.exists(), str(root))
        for path in root_paths(receipt):
            self.checks.check(label + ": deleted private receipt root", path.is_relative_to(self.root) and not path.exists(), str(path))
        for group in self.groups:
            self.checks.check(label + ": no surviving sandbox group", not process_group_members(group), group)
        return process, receipt, list(all_rows(receipt))


def validate_samples(checks, label, receipt, repeats, sections=("baseline",)):
    for section in sections:
        rows = list(measurement_rows(receipt.get(section, {})))
        checks.check(label + ": " + section + " measurements present", bool(rows), receipt.get(section))
        for row in rows:
            scope = row["id"]
            samples = row.get("samples", [])
            finite = len(samples) == repeats and all(isinstance(x, (int, float)) and not isinstance(x, bool)
                                                    and math.isfinite(x) and x >= 0 for x in samples)
            checks.check(label + ": " + section + ": finite census " + scope, finite and row.get("n") == repeats, row)
            checks.check(label + ": " + section + ": scope schema " + scope,
                         all(key in row for key in ("median", "min", "max", "unit", "workload")), row)
            checks.check(label + ": " + section + ": declared workload " + scope,
                         row.get("workload") == {"repeats": repeats, "window_s": 1, "settle_s": 2}, row.get("workload"))
            if scope.startswith("startup/"):
                checks.check(label + ": " + section + ": startup observed, not synthetic zero " + scope,
                             bool(samples) and all(value > 0 for value in samples), row)
            if finite:
                checks.check(label + ": " + section + ": statistics " + scope,
                             row.get("median") == statistics.median(samples) and row.get("min") == min(samples)
                             and row.get("max") == max(samples), row)
        scopes = ["startup/core", "startup/ctl-inspect", "idle/events", "idle/content", "size/total", "size/item/ctl-perf.panel"]
        if Path("/proc").is_dir():
            scopes += ["idle/cpu/ctl-inspect", "idle/rss/ctl-inspect"]
        for scope in scopes:
            checks.check(label + ": " + section + ": required scope " + scope, any(row.get("id") == scope for row in rows), rows)


def assert_warning_pass(checks, label, process, receipt, rows, scope):
    selected = scope_rows(rows, scope)
    checks.check(label + ": scope warns " + scope, any(row.get("status") == "warn" for row in selected), selected)
    checks.check(label + ": warning never fails verdict", process.returncode == 0 and receipt.get("verdict") == "pass",
                 {"exit": process.returncode, "verdict": receipt.get("verdict"), "rows": receipt.get("rows")})
    checks.check(label + ": warning count visible", receipt.get("counts", {}).get("warn", 0) > 0, receipt.get("counts"))


def thresholds(driver, source, checks):
    script = BASE.replace("DELAY_MS = 0", "DELAY_MS = 20")
    configure(source, script, [timing()])
    process, receipt, rows = driver.run_perf("default-thresholds", repeats=None)
    checks.check("defaults: thresholds source", receipt.get("thresholds", {}).get("source") == "default", receipt.get("thresholds"))
    checks.check("defaults: healthy pass", process.returncode == 0 and receipt.get("verdict") == "pass", receipt.get("rows"))
    validate_samples(checks, "defaults", receipt, 5)
    configure(source, script, [timing()], {"absolute": {"interaction_ms": 1}})
    process, receipt, rows = driver.run_perf("settings-thresholds")
    settings = receipt.get("thresholds", {})
    checks.check("settings: thresholds source", settings.get("source") == "settings", settings)
    checks.check("settings: override reported", settings.get("values", {}).get("absolute", {}).get("interaction_ms") == 1, settings)
    checks.check("settings: other defaults retained", settings.get("values", {}).get("absolute", {}).get("rss_mb") == 90, settings)
    assert_warning_pass(checks, "settings", process, receipt, rows, "interaction/")
    validate_samples(checks, "settings", receipt, 5)


def idle(driver, source, checks):
    configure(source, IDLE)
    process, receipt, rows = driver.run_perf("idle-loop-alone")
    assert_warning_pass(checks, "idle-alone", process, receipt, rows, "idle/events")
    validate_samples(checks, "idle-alone", receipt, 5)
    configure(source, BASE, [], {"noise_ms": 1000000, "rss_relative": 1000, "noise_rss_mb": 1000000})
    overlay = driver.root / "idle-candidate"
    overlay.mkdir()
    (overlay / "ctl-inspect.py").write_text(IDLE)
    process, receipt, rows = driver.run_perf("idle-loop", candidate=overlay)
    assert_warning_pass(checks, "idle", process, receipt, rows, "idle/events")
    validate_samples(checks, "idle", receipt, 5, ("baseline", "candidate"))


def slow(driver, source, checks):
    configure(source, BASE, [timing()], {"noise_ms": 150, "rss_relative": 1000, "noise_rss_mb": 1000000})
    overlay = driver.root / "slow-candidate"
    overlay.mkdir()
    (overlay / "ctl-inspect.py").write_text(SLOW)
    process, receipt, rows = driver.run_perf("slow-command", candidate=overlay)
    assert_warning_pass(checks, "slow", process, receipt, rows, "interaction/")
    checks.check("slow: relative slowdown reported", any(row.get("status") == "warn" and
                 any(word in row.get("evidence", "").lower() for word in ("slower", "slowdown", "relative"))
                 for row in scope_rows(receipt.get("rows", []), "interaction/")), receipt.get("rows"))
    validate_samples(checks, "slow", receipt, 5, ("baseline", "candidate"))


def noisy(driver, source, checks):
    configure(source, CONTROL_BASE, [timing("noisy-control")],
              {"noise_ms": 50, "rss_relative": 1000, "noise_rss_mb": 1000000,
               "absolute": {"interaction_ms": None}})
    overlay = driver.root / "noisy-candidate"
    overlay.mkdir()
    (overlay / "ctl-inspect.py").write_text(CONTROL_CANDIDATE)
    with Responder(driver.root) as responder:
        process, receipt, rows = driver.run_perf("noisy-command", candidate=overlay)
    (driver.root / "noisy-responder.json").write_text(json.dumps({"observed": responder.observed, "errors": responder.errors}, indent=2))
    checks.check("noisy: coordinator did not fail", not responder.errors, responder.errors)
    checks.check("noisy: all deterministic delays exercised", Counter(item["mode"] for item in responder.observed) ==
                 {"baseline": 5, "candidate": 5}, responder.observed)
    base_rows = [row for row in measurement_rows(receipt.get("baseline", {}))
                 if "/noisy-control/" in row["id"] and row["id"].endswith("/reply")]
    candidate_rows = [row for row in measurement_rows(receipt.get("candidate", {}))
                      if "/noisy-control/" in row["id"] and row["id"].endswith("/reply")]
    overlapping = bool(base_rows and candidate_rows) and (
        candidate_rows[0]["median"] > base_rows[0]["median"] * 1.25
        and candidate_rows[0]["median"] - base_rows[0]["median"] > 50
        and candidate_rows[0]["min"] <= base_rows[0]["max"])
    checks.check("noisy: raw samples establish overlapping slowdown", overlapping,
                 {"baseline": base_rows, "candidate": candidate_rows})
    checks.check("noisy: overlapping slowdown unverified", any(row.get("status") == "unverified" for row in
                 scope_rows(receipt.get("rows", []), "interaction/")), receipt.get("rows"))
    checks.check("noisy: verdict not pass", process.returncode == 77 and receipt.get("verdict") == "unverified",
                 {"exit": process.returncode, "verdict": receipt.get("verdict")})
    validate_samples(checks, "noisy", receipt, 5, ("baseline", "candidate"))


def missing(driver, source, checks):
    configure(source, BASE, [timing()])
    process, receipt, rows = driver.run_perf("zero-samples", repeats=0)
    checks.check("zero: invalid repeat count rejected before launch", process.returncode == 2 and receipt.get("verdict") == "error",
                 {"exit": process.returncode, "receipt": receipt})
    scenario = timing("missing-change")
    scenario["steps"].append({"wait": {"entry": "doc/perf-never-published", "exists": True}, "within_ms": 100})
    configure(source, BASE, [scenario])
    process, receipt, rows = driver.run_perf("missing-samples")
    checks.check("missing: broken measurements fail", process.returncode == 1 and receipt.get("verdict") == "fail",
                 {"exit": process.returncode, "receipt": receipt})
    checks.check("missing: missing change census reported at top level", any(row.get("status") == "fail" and
                 "/content" in row.get("id", "") and row.get("measure", {}).get("n", 5) < 5
                 for row in receipt.get("rows", [])), receipt.get("rows"))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--vqro", type=Path, default=Path(os.environ.get("CARGO_TARGET_DIR") or REPO / "target") / "debug" / "vqro")
    parser.add_argument("--cases", default="thresholds,idle,slow,noisy,missing")
    parser.add_argument("--source-only", action="store_true")
    parser.add_argument("--timeout", type=int, default=600)
    args = parser.parse_args()
    checks = Checks()
    scratch = REPO / ".run"  # ignored scratch space (AGENTS.md)
    scratch.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="tp-", dir=scratch))
    source = driver = None
    start = time.monotonic()
    before = state()
    executed = {}
    try:
        source = Source(root, args.vqro.resolve(), "s")
        executed = {name: hashlib.sha256((source.ext / name).read_bytes()).hexdigest()
                    for name in ("testing_perf.py", "vqro_testing.py") if (source.ext / name).is_file()}
        source.start()
        checks.check("source: safe mode", source.call("workspace.snapshot")["safe_mode"] is True)
        configure(source)
        for name, script in (("base", BASE), ("idle", IDLE), ("slow", SLOW), ("controlled", CONTROL_BASE)):
            compile(script, name, "exec")
        driver = PerfDriver(root, source, checks, args.timeout)
        compile(driver.launcher.read_text(), "audit-launcher", "exec")
        checks.check("source: fixtures compile", True)
        if not args.source_only:
            if not (source.ext / "testing_perf.py").is_file():
                raise RuntimeError("implementation not present: extensions/testing_perf.py")
            cases = {"thresholds": thresholds, "idle": idle, "slow": slow, "noisy": noisy, "missing": missing}
            for case in args.cases.split(","):
                try:
                    cases[case](driver, source, checks)
                except Exception as error:
                    checks.check(case + ": harness execution", False, repr(error))
    except Exception as error:
        checks.check("harness setup", False, repr(error))
    finally:
        if source:
            source.stop()
    leftovers = []
    if driver:
        driver.launches()
        for group in driver.groups:
            members = process_group_members(group)
            if members:
                leftovers += members
                os.killpg(group, signal.SIGKILL)
    checks.check("cleanup: no surviving sandbox groups", not leftovers, leftovers)
    checks.check("cleanup: source reaped", source is None or source.proc is None or source.proc.poll() is not None)
    failures = [check for check in checks.results if not check["ok"]]
    worktree = {"before": before, "after": state(), "executed_sha256": executed}
    summary = {"ok": not failures, "checks": len(checks.results), "failures": failures,
               "elapsed_seconds": round(time.monotonic() - start, 3), "leftovers": leftovers,
               "worktree": worktree, "evidence_root": str(root) if failures else None}
    (root / "worktree-state.json").write_text(json.dumps(worktree, indent=2))
    if not failures:
        shutil.rmtree(root)
    print(json.dumps(summary, indent=2))
    return int(bool(failures))


if __name__ == "__main__":
    sys.exit(main())
