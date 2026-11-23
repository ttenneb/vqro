#!/usr/bin/env python3
"""Independent slice-1 contract controls; no live endpoints or installed files used.

Run: python3 tests/e2e_testing_unit.py [--vqro /path/to/debug/vqro]
Build first with CARGO_TARGET_DIR=<private-notes>
cargo build. Stdlib only. Failure evidence is retained in the printed private root;
success removes it. All children get an explicit empty-based environment.
"""
import argparse
from collections import Counter
from datetime import datetime
import json
import os
from pathlib import Path
import re
import shutil
import socket
import stat
import subprocess
import sys
import tempfile
import time

REPO = Path(__file__).resolve().parents[1]
STATUSES = ("pass", "warn", "fail", "skip", "unverified")
HEALTHY = '''from vqro_ext import Extension
ext = Extension(event_types=[])
ext.contribute({})
ext.run()
'''
PASS_TEST = '''import unittest
class Control(unittest.TestCase):
    def test_ok(self):
        self.assertTrue(True)
'''
FAIL_TEST = PASS_TEST.replace('self.assertTrue(True)', 'self.fail("intentional negative control")')
ENV_TEST = '''import os, pathlib, unittest
class Control(unittest.TestCase):
    def test_no_endpoint_or_supervision_environment(self):
        self.assertEqual(["VQRO_TEST_EXTENSIONS_DIR"], sorted(
            k for k in os.environ if k.startswith(("VQRO_", "HC_", "HERDR_"))))
        self.assertEqual(str(pathlib.Path(__file__).resolve().parents[2]),
                         os.environ["VQRO_TEST_EXTENSIONS_DIR"])
'''


class Oracle:
    def __init__(self):
        self.checks = []

    def check(self, label, condition, actual=None):
        row = {"check": label, "ok": bool(condition)}
        if not condition:
            row["actual"] = actual
        self.checks.append(row)

    def receipt(self, label, receipt, expected, source):
        self.check(label + ": receipt object", isinstance(receipt, dict), receipt)
        if not isinstance(receipt, dict):
            return
        for key, value in (("kind", "vqro.test-receipt"), ("version", 1),
                           ("suite", "unit"), ("verdict", expected), ("source", source)):
            self.check(label + ": " + key, receipt.get(key) == value, receipt.get(key))
        rows = receipt.get("rows", [])
        self.check(label + ": rows", isinstance(rows, list) and bool(rows), rows)
        if not isinstance(rows, list):
            return
        ids = [r.get("id") for r in rows]
        self.check(label + ": unique row ids", len(ids) == len(set(ids)), ids)
        self.check(label + ": row schema", all(
            set(("id", "subject", "status", "evidence", "advice")) <= r.keys()
            and r["status"] in STATUSES and isinstance(r["evidence"], str)
            and len(r["evidence"]) <= 300 for r in rows), rows)
        counts = Counter(r.get("status") for r in rows)
        self.check(label + ": consistent counts", receipt.get("counts") ==
                   {s: counts[s] for s in STATUSES}, receipt.get("counts"))
        fp = receipt.get("fingerprint", {})
        self.check(label + ": fingerprint schema", set(("vqro", "extensions_dir", "declarations", "extensions")) <= fp.keys(), fp)
        for key in ("extensions_dir", "declarations"):
            self.check(label + ": sha256 " + key, bool(re.fullmatch(r"[a-f0-9]{64}", str(fp.get(key, "")))), fp.get(key))
        self.check(label + ": per-extension sha256", isinstance(fp.get("extensions"), dict)
                   and bool(fp["extensions"]) and all(re.fullmatch(r"[a-f0-9]{64}", str(v)) for v in fp["extensions"].values()), fp)
        try:
            stamp = datetime.fromisoformat(receipt["started_at"].replace("Z", "+00:00"))
            valid_stamp = stamp.utcoffset().total_seconds() == 0
        except (KeyError, ValueError, TypeError, AttributeError):
            valid_stamp = False
        self.check(label + ": UTC start", valid_stamp, receipt.get("started_at"))
        self.check(label + ": elapsed_ms", isinstance(receipt.get("elapsed_ms"), (int, float))
                   and receipt["elapsed_ms"] >= 0, receipt.get("elapsed_ms"))
        for key in ("inputs", "not_covered", "skipped"):
            self.check(label + ": " + key + " list", isinstance(receipt.get(key), list), receipt.get(key))
        self.check(label + ": bounded receipt", len(json.dumps(receipt).encode()) < 1024 * 1024)

    def statuses(self, label, receipt, expected):
        rows = {r["id"]: r["status"] for r in receipt.get("rows", [])}
        for ident, status in expected.items():
            self.check(label + ": " + ident + " must be " + status, rows.get(ident) == status, rows.get(ident))
        disabled = [ident.split("/", 1)[1] for ident, status in rows.items()
                    if ident.startswith("status/") and status == "skip"]
        for name in disabled:
            self.check(label + ": disabled checks omitted for " + name,
                       not any(prefix + "/" + name in rows for prefix in ("deps", "log", "tests")))


def wait_for(probe, description, timeout=60):
    """A delay only paces retries: success is always an observed fact."""
    deadline = time.monotonic() + timeout
    last = None
    while time.monotonic() < deadline:
        try:
            last = probe()
            if last:
                return last
        except (OSError, ValueError, RuntimeError, subprocess.TimeoutExpired) as error:
            last = str(error)
        time.sleep(0.05)
    raise RuntimeError(f"deadline waiting for {description}: {last}")


class PrivateCore:
    def __init__(self, root, binary, scenario):
        self.root = root / scenario
        self.root.mkdir()
        self.binary = binary
        self.ext = self.root / "ext"
        shutil.copytree(REPO / "extensions", self.ext, ignore=shutil.ignore_patterns("__pycache__", "*.pyc"))
        # Disable defaults BEFORE launch, including providers; fake alone is not isolation.
        defaults = json.loads((self.ext / "defaults.json").read_text())
        for spec in defaults["extensions"]:
            spec["enabled"] = False
        (self.ext / "defaults.json").write_text(json.dumps(defaults))
        for name in ("home", "run", "st", "cfg", "xst"):
            (self.root / name).mkdir(mode=0o700)
        self.state = self.root / "st"
        self.sock = self.root / "p.sock"
        assert len(os.fsencode(self.sock)) < 100, self.sock
        self.env = {"HOME": str(self.root / "home"), "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
                    "XDG_RUNTIME_DIR": str(self.root / "run"), "XDG_CONFIG_HOME": str(self.root / "cfg"),
                    "XDG_STATE_HOME": str(self.root / "xst"), "HC_SOCKET": str(self.root / "h.sock"),
                    "HC_SESSION": "unit-controls", "VQRO_STATE_DIR": str(self.state),
                    "VQRO_SOCKET": str(self.sock), "VQRO_EXTENSIONS_DIR": str(self.ext),
                    "VQRO_HERDR": '["/bin/false"]', "PYTHONDONTWRITEBYTECODE": "1"}
        self.proc = None
        self.log = None

    def command(self, args, timeout=180):
        # subprocess env replaces the entire inherited environment (env -i semantics).
        return subprocess.run(args, env=self.env, cwd=self.root, capture_output=True,
                              text=True, timeout=timeout)

    def call(self, method, params=None):
        result = self.command([str(self.binary), "call", method, json.dumps(params or {})], timeout=10)
        if result.returncode:
            raise RuntimeError(f"{method}: {result.stderr or result.stdout}")
        return json.loads(result.stdout)

    def start(self):
        self.log = (self.root / "core.log").open("w")
        self.proc = subprocess.Popen([str(self.binary), "serve", "--fake"], env=self.env,
                                     cwd=self.root, stdout=self.log, stderr=subprocess.STDOUT)
        wait_for(lambda: self.sock.exists() and self.call("extension.list"), "private core")

    def seed(self, name, script=HEALTHY, test=None, enabled=True, badmanifest=False, needs=False):
        filename = name + ".py"
        if script is not None:
            (self.ext / filename).write_text(script)
        manifest = {"name": "not-" + name if badmanifest else name, "title": name,
                    "version": "0.1.0", "license": "Apache-2.0", "author": "Test control",
                    "summary": "Independent oracle fixture", "provides": []}
        if needs:
            manifest["needs"] = [{"cap": "ability/ctl.nobody-provides", "suggest": "ctl-nonexistent", "why": "negative control"}]
        (self.ext / "manifests" / (name + ".json")).write_text(json.dumps(manifest))
        if test:
            folder = self.ext / "tests" / name
            folder.mkdir(parents=True, exist_ok=True)
            (folder / "test_control.py").write_text(test)
        self.call("workspace.apply", {"ops": [{"op": "set", "key": "extension/" + name,
                  "value": {"argv": ["python3", filename], "cwd": str(self.ext), "enabled": enabled}}],
                  "note": "independent private test control"})

    def settled(self, desired):
        def probe():
            extensions = self.call("extension.list")["extensions"]
            return extensions if all(extensions.get(n, {}).get("display_status") == s
                                     for n, s in desired.items()) else None
        return wait_for(probe, "extension status " + json.dumps(desired))

    def enable_reporter(self):
        self.call("workspace.apply", {"ops": [{"op": "set", "key": "extension/test-unit",
                  "value": {"argv": ["python3", str(self.ext / "testing_unit.py")], "cwd": str(self.ext)}}]})
        self.settled({"test-unit": "running"})
        wait_for(lambda: any(c.get("name") == "test.unit" for c in
                            self.call("workspace.snapshot").get("commands", [])), "test.unit registration")

    def cli(self):
        return self.command(["python3", str(self.ext / "testing_unit.py"), "--json", "--socket", str(self.sock),
                             "--state-dir", str(self.state), "--extensions-dir", str(self.ext)])

    def snapshot(self):
        return self.call("workspace.snapshot"), self.call("extension.list")["extensions"]

    def events_between(self, before, after):
        if before["seq"] == after["seq"]:
            return []
        with socket.socket(socket.AF_UNIX) as connection:
            connection.settimeout(10)
            connection.connect(str(self.sock))
            with connection.makefile("rwb", buffering=0) as stream:
                def send(ident, method, params):
                    stream.write((json.dumps({"jsonrpc": "2.0", "id": ident, "method": method,
                                              "params": params}) + "\n").encode())
                send(1, "hello", {"client": "independent-readonly-proof"})
                json.loads(stream.readline())
                send(2, "events.subscribe", {"from": before["seq"], "epoch": before["epoch"]})
                events = []
                while True:
                    message = json.loads(stream.readline())
                    if message.get("error") or message.get("method") == "missed":
                        raise RuntimeError(f"read-only event proof unavailable: {message}")
                    if message.get("method") == "event":
                        event = message["params"]
                        if event["seq"] <= after["seq"]:
                            events.append(event)
                        if event["seq"] >= after["seq"]:
                            return events

    def stop(self):
        if self.proc is not None:
            if self.proc.poll() is None:
                try:
                    self.call("core.shutdown")
                except (RuntimeError, OSError, ValueError, subprocess.TimeoutExpired):
                    self.proc.terminate()
                try:
                    self.proc.wait(timeout=12)
                except subprocess.TimeoutExpired:
                    self.proc.kill()
                    self.proc.wait(timeout=5)
            self.log.close()


def readonly(oracle, label, before, after, core):
    old, oldext = before
    new, newext = after
    changed = {k for k in old["entries"].keys() | new["entries"].keys()
               if old["entries"].get(k) != new["entries"].get(k)}
    allowed = all(k == "view/test-unit.report" and
                  new["entries"].get(k, {}).get("value", {}).get("owner") == "test-unit" for k in changed)
    oracle.check(label + ": only own view contribution may change", allowed, sorted(changed))
    events = core.events_between(old, new)
    oracle.check(label + ": sequence changes are own report publications only", all(
        event.get("type") == "view.content" and event.get("data", {}).get("view") == "test-unit.report"
        and event.get("data", {}).get("author") == "ext:test-unit"
        for event in events), events)
    oracle.check(label + ": all sequence changes accounted for", len(events) == new["seq"] - old["seq"],
                 [old["seq"], new["seq"], events])
    generations = {k: v.get("generation") for k, v in oldext.items() if k != "test-unit"}
    oracle.check(label + ": no other extension restarted", generations ==
                 {k: v.get("generation") for k, v in newext.items() if k != "test-unit"}, newext)


def decode(oracle, label, process):
    try:
        return json.loads(process.stdout)
    except ValueError:
        oracle.check(label + ": stdout must contain JSON only", False,
                     {"stdout": process.stdout[-2000:], "stderr": process.stderr[-2000:], "exit": process.returncode})
        return {}


def negative(core, oracle):
    core.start()
    core.seed("ctl-ok", test=PASS_TEST)
    core.seed("ctl-crash", script="raise SystemExit(1)\n")
    core.seed("ctl-badmanifest", badmanifest=True)
    core.seed("ctl-missing", script=None)
    core.seed("ctl-traceback", script='import sys\nprint("Traceback (most recent call last):", file=sys.stderr, flush=True)\n' + HEALTHY)
    core.seed("ctl-failtest", test=FAIL_TEST)
    core.seed("ctl-envleak", test=ENV_TEST)
    # Prove the exception cannot hide an endpoint or another inherited live variable.
    environment = {"HOME": core.env["HOME"], "PATH": core.env["PATH"],
                   "VQRO_TEST_EXTENSIONS_DIR": str(core.ext)}
    for variable in ("VQRO_TEST_EXTENSIONS_DIR", "VQRO_SOCKET", "HC_SOCKET", "HERDR_SOCKET"):
        contaminated = dict(environment, **{variable: str(core.sock)})
        control = subprocess.run([sys.executable, "-B", "-m", "unittest", "discover", "-s", "."],
                                 cwd=core.ext / "tests/ctl-envleak", env=contaminated,
                                 capture_output=True, text=True, timeout=10)
        oracle.check("environment control rejects " + variable, control.returncode != 0,
                     control.stdout + control.stderr)
    core.seed("ctl-needs", needs=True)
    core.seed("ctl-off", enabled=False)
    core.settled({"ctl-ok": "running", "ctl-crash": "failed", "ctl-missing": "failed",
                  "ctl-badmanifest": "running", "ctl-traceback": "running", "ctl-failtest": "running",
                  "ctl-envleak": "running", "ctl-needs": "running", "ctl-off": "off"})
    wait_for(lambda: (core.state / "ext-ctl-traceback.log").exists() and
             "Traceback" in (core.state / "ext-ctl-traceback.log").read_text(), "traceback captured in log")
    core.enable_reporter()
    expected = {"status/ctl-ok": "pass", "tests/ctl-ok": "pass", "status/ctl-crash": "fail",
                "manifest/ctl-badmanifest": "warn", "argv/ctl-missing": "fail", "log/ctl-traceback": "warn",
                "tests/ctl-failtest": "fail", "tests/ctl-envleak": "pass", "deps/ctl-needs": "fail",
                "status/ctl-off": "skip"}
    before = core.snapshot()
    process = core.cli()
    cli = decode(oracle, "live CLI", process)
    oracle.check("live CLI: exit 1", process.returncode == 1, process.returncode)
    oracle.receipt("live CLI", cli, "fail", "live")
    oracle.statuses("live CLI", cli, expected)
    readonly(oracle, "CLI read-only", before, core.snapshot(), core)
    oracle.check("CLI: no implicit receipt files", not list(core.state.rglob("receipt-*.json")))
    not_covered = json.dumps(cli.get("not_covered", []))
    for name in ("ctl-crash", "ctl-badmanifest", "ctl-missing", "ctl-traceback", "ctl-needs"):
        oracle.check("not_covered includes " + name, name in not_covered, not_covered)
    process = core.command([str(core.binary), "run", "test.unit", "{}"])
    result = decode(oracle, "test.unit", process)
    oracle.check("test.unit: RPC transport success", process.returncode == 0, process.stderr)
    oracle.check("test.unit: fail verdict", result.get("verdict") == "fail", result)
    oracle.check("test.unit: popup", result.get("client", {}).get("popup", {}).get("view") == "test-unit.report", result)
    receipt_path = Path(str(result.get("receipt", "missing")))
    contained = receipt_path.is_absolute() and receipt_path.is_relative_to(core.state / "test-unit")
    oracle.check("test.unit: receipt inside private state/test-unit", contained, str(receipt_path))
    if contained and receipt_path.is_file():
        receipt = json.loads(receipt_path.read_text())
        oracle.receipt("extension receipt", receipt, "fail", "live")
        oracle.statuses("extension receipt", receipt, expected)
        oracle.check("test.unit: counts match receipt", result.get("counts") == receipt.get("counts"), result)
        oracle.check("test.unit: receipt mode 0600", stat.S_IMODE(receipt_path.stat().st_mode) == 0o600)
    else:
        oracle.check("test.unit: receipt written", False, str(receipt_path))
    readonly(oracle, "extension read-only", before, core.snapshot(), core)
    outside = [str(p) for p in core.root.rglob("receipt-*.json") if not p.is_relative_to(core.state / "test-unit")]
    oracle.check("no receipt outside private state", not outside, outside)
    core.stop()
    process = core.cli()
    receipt = decode(oracle, "offline CLI", process)
    oracle.receipt("offline CLI", receipt, "fail", "offline")
    oracle.check("offline CLI: exit 1", process.returncode == 1, process.returncode)
    oracle.statuses("offline CLI", receipt, {"core/reachable": "warn", "argv/ctl-missing": "fail"})


def self_control(core, oracle, disabled):
    core.start()
    core.seed("ctl-ok", test=PASS_TEST, enabled=not disabled)
    core.settled({"ctl-ok": "off" if disabled else "running"})
    before = core.snapshot()
    process = core.cli()
    receipt = decode(oracle, core.root.name, process)
    oracle.receipt(core.root.name, receipt, "unverified" if disabled else "pass", "live")
    oracle.check(core.root.name + ": exit " + str(77 if disabled else 0), process.returncode == (77 if disabled else 0), process.returncode)
    oracle.statuses(core.root.name, receipt, {"status/ctl-ok": "skip" if disabled else "pass"})
    readonly(oracle, core.root.name + " read-only", before, core.snapshot(), core)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--vqro", type=Path, default=Path(os.environ.get("CARGO_TARGET_DIR") or REPO / "target") / "debug" / "vqro")
    args = parser.parse_args()
    oracle = Oracle()
    scratch = REPO / ".run"  # ignored scratch space (AGENTS.md)
    scratch.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="tu-", dir=scratch))
    cores = []
    try:
        if not args.vqro.is_file():
            raise RuntimeError(f"build missing: {args.vqro}")
        if not (REPO / "extensions/testing_unit.py").is_file():
            raise RuntimeError("implementation not yet present: extensions/testing_unit.py")
        for scenario, run in (("neg", lambda c: negative(c, oracle)),
                              ("ok", lambda c: self_control(c, oracle, False)),
                              ("off", lambda c: self_control(c, oracle, True))):
            core = PrivateCore(root, args.vqro.resolve(), scenario)
            cores.append(core)
            try:
                run(core)
            except Exception as error:
                oracle.check(scenario + ": harness execution", False, repr(error))
            finally:
                core.stop()
    except Exception as error:
        oracle.check("harness setup", False, repr(error))
    leftovers = [str(c.sock) for c in cores if c.sock.exists()]
    # Linux-only supplementary orphan check: inspect, never signal, matching private
    # environment entries. The core's shutdown owns its supervised children.
    if Path("/proc").is_dir():
        marker = ("VQRO_SOCKET=" + str(root) + "/").encode()
        for process_dir in Path("/proc").iterdir():
            if not process_dir.name.isdigit():
                continue
            try:
                environment = (process_dir / "environ").read_bytes().split(b"\0")
                if any(entry.startswith(marker) for entry in environment):
                    leftovers.append("pid:" + process_dir.name)
            except (OSError, PermissionError):
                pass
    oracle.check("cleanup: all started cores reaped", all(c.proc is None or c.proc.poll() is not None for c in cores))
    oracle.check("cleanup: no private sockets left", not leftovers, leftovers)
    failed = [c for c in oracle.checks if not c["ok"]]
    summary = {"ok": not failed, "checks": len(oracle.checks), "failures": failed,
               "leftovers": leftovers, "evidence_root": str(root) if failed else None}
    if not failed:
        shutil.rmtree(root)
    print(json.dumps(summary, indent=2))
    return int(bool(failed))


if __name__ == "__main__":
    sys.exit(main())
