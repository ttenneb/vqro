#!/usr/bin/env python3
"""Independent finish-evidence policy controls. Run only through the normal gate slot.

Every effect targets a copied extension tree and private fake core. This uses the
public CLI/RPC and actual Unit/Integration receipts, never checker internals.
No model, live install, backend, or model-provider call is made.
"""
import argparse
import hashlib
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

from e2e_testing_unit import PrivateCore, Oracle, PASS_TEST, FAIL_TEST, decode, wait_for

REPO = Path(__file__).resolve().parents[1]
POLICY = "doc/settings/testing.require_unit"
BARRIER_TEST = '''import os, time, unittest
from pathlib import Path
class ConcurrentControl(unittest.TestCase):
    def test_barrier(self):
        folder = Path(os.environ["VQRO_TEST_EXTENSIONS_DIR"]) / ".cache"
        folder.mkdir(exist_ok=True)
        (folder / "unit-entered").write_text("entered")
        deadline = time.monotonic() + 30
        while not (folder / "unit-release").exists():
            if time.monotonic() >= deadline:
                self.fail("independent test barrier was not released")
            time.sleep(0.01)
        self.assertTrue(True)
'''


def identities():
    result = subprocess.run(["git", "rev-parse", "HEAD"], cwd=REPO,
                            env={"HOME": os.environ["HOME"], "PATH": os.environ.get("PATH", "/usr/bin:/bin")},
                            capture_output=True, text=True, timeout=10)
    return {"head": result.stdout.strip(), "sha256": {
        name: hashlib.sha256((REPO / "extensions" / name).read_bytes()).hexdigest()
        for name in ("vqro_testing.py", "testing_unit.py", "testing_setup.py", "testing_integration.py")}}


class FinishCore(PrivateCore):
    def rpc(self, method, params=None, identity=None):
        with socket.socket(socket.AF_UNIX) as connection:
            connection.settimeout(30)
            connection.connect(str(self.sock))
            with connection.makefile("rwb", buffering=0) as stream:
                for ident, name, args in ((1, "hello", {"client": "independent-finish-control", **(
                    {"identity": identity} if identity else {})}), (2, method, params or {})):
                    stream.write((json.dumps({"jsonrpc": "2.0", "id": ident, "method": name, "params": args}) + "\n").encode())
                    while True:
                        line = stream.readline(1024 * 1024)
                        if not line:
                            raise RuntimeError("private core disconnected")
                        message = json.loads(line)
                        if message.get("id") == ident:
                            break
                    if "error" in message:
                        raise RuntimeError(json.dumps(message["error"]))
                return message["result"]

    def invoke(self, name, args=None, identity=None):
        return self.rpc("command.invoke", {"name": name, "args": args or {}}, identity)

    def status(self):
        return self.invoke("test.setup.status")

    def stable_status(self):
        last = [None]
        def probe():
            current = self.status()
            key = (current.get("setup_id"), current.get("generation"))
            ready = key == last[0]
            last[0] = key
            return current if ready else None
        return wait_for(probe, "stable observed setup identity/generation")

    def set_value(self, key, value):
        self.call("workspace.apply", {"ops": [{"op": "set", "key": key, "value": value}],
                                      "note": "independent finish-gate control"})

    def cli_args(self, *extra):
        return [sys.executable, str(self.ext / "testing_unit.py"), "--json", "--socket", str(self.sock),
                "--state-dir", str(self.state), "--extensions-dir", str(self.ext), *extra]

    def record(self, oracle, label, verdict="pass", code=0):
        process = self.command(self.cli_args("--record"))
        receipt = decode(oracle, label, process)
        oracle.check(label + ": exit", process.returncode == code, {"exit": process.returncode, "receipt": receipt})
        oracle.check(label + ": verdict", receipt.get("verdict") == verdict, receipt)
        path = Path(receipt.get("receipt", "missing"))
        private = path.is_absolute() and path.is_relative_to(self.state / "test-unit")
        oracle.check(label + ": recorded private receipt exists", private and path.is_file(), str(path))
        if private and path.is_file():
            stored = json.loads(path.read_text())
            oracle.check(label + ": disk/stdout identity matches", stored.get("setup_id") == receipt.get("setup_id")
                         and stored.get("generation") == receipt.get("generation"), stored)
            oracle.check(label + ": receipt mode 0600", stat.S_IMODE(path.stat().st_mode) == 0o600)
        return receipt

    def notice(self, oracle, label, text="NOT VERIFIED"):
        status = self.status()
        live = self.call("workspace.snapshot").get("live", {})
        content = live.get("testing.status", {})
        oracle.check(label + ": Testing notice", text in content.get("label", ""), content)
        if text == "NOT VERIFIED":
            oracle.check(label + ": non-green semantic notice", content.get("style") in ("warn", "error"), content)
        return status, live


def check_gate(core, oracle):
    core.start()
    # Known, minimal setup: absent optional providers get a separate explicit test.
    snapshot = core.call("workspace.snapshot")
    core.call("workspace.apply", {"ops": [{"op": "delete", "key": key} for key in snapshot["entries"]
                                          if key.startswith("extension/")], "note": "private control declarations only"})
    core.seed("ctl-pass", test=PASS_TEST)
    core.seed("ctl-gap")
    core.enable_reporter()
    core.settled({"ctl-pass": "running", "ctl-gap": "running", "test-unit": "running"})
    status = core.stable_status()
    oracle.check("policy off: default remains off", status.get("require_unit") is False, status)
    oracle.check("policy off: finish allowed without receipt", status.get("finish_ok") is True, status)
    oracle.check("known setup identity", bool(re.fullmatch(r"[a-f0-9]{64}", str(status.get("setup_id")))), status)
    core.set_value(POLICY, True)
    status = core.stable_status()
    oracle.check("required missing Unit blocks finish", status.get("unit", {}).get("state") == "missing"
                 and status.get("finish_ok") is False, status)
    core.notice(oracle, "missing Unit")
    ad_hoc = core.command(core.cli_args("--out", str(core.root / "adhoc.json")))
    oracle.check("ad hoc Unit pass not adopted", ad_hoc.returncode == 0
                 and core.stable_status().get("unit", {}).get("state") == "missing", ad_hoc.stdout + ad_hoc.stderr)
    receipt = core.record(oracle, "current Unit pass")
    status = core.stable_status()
    oracle.check("record adopted for current setup", status.get("unit", {}).get("state") == "pass"
                 and status["unit"].get("receipt") == receipt.get("receipt") and status.get("setup_id") == receipt.get("setup_id")
                 and status.get("generation") == receipt.get("generation"), status)
    oracle.check("Integration missing still blocks finish", status.get("integration", {}).get("state") == "missing"
                 and status.get("finish_ok") is False, status)
    status = core.invoke("test.setup.skip", {"suite": "integration", "reason": "Private control: integration deliberately omitted"})
    oracle.check("explicit Integration skip allows finish", status.get("finish_ok") is True
                 and bool(status.get("integration", {}).get("skip_reason")), status)
    oracle.check("soft coverage gap disclosed, not blocking", status.get("finish_ok") is True
                 and "ctl-gap" in json.dumps(status.get("coverage_gaps", [])), status)
    _, live = core.notice(oracle, "coverage gap notice")
    page = live.get("settings.page.testing", {})
    oracle.check("Testing page shows coverage gap", "ctl-gap" in json.dumps(page), page)
    core.set_value("doc/settings/control", {"edit": 1})
    status = core.stable_status()
    oracle.check("change after receipt makes Unit stale", status.get("unit", {}).get("state") == "stale"
                 and status.get("finish_ok") is False, status)
    core.notice(oracle, "stale Unit")

    test = core.ext / "tests/ctl-pass/test_control.py"
    test.write_text(BARRIER_TEST)
    core.stable_status()
    barrier = core.ext / ".cache"
    process = subprocess.Popen(core.cli_args("--record"), env=core.env, cwd=core.root,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        wait_for(lambda: (barrier / "unit-entered").exists(), "Unit test entered independent barrier")
        core.set_value("doc/settings/control", {"edit": 2})
        (barrier / "unit-release").write_text("release")
        stdout, stderr = process.communicate(timeout=45)
    finally:
        (barrier / "unit-release").write_text("release")
        if process.poll() is None:
            process.kill()
            process.wait(timeout=5)
    concurrent = decode(oracle, "concurrent change", subprocess.CompletedProcess([], process.returncode, stdout, stderr))
    oracle.check("change during Unit: unverified exit 77", process.returncode == 77 and concurrent.get("verdict") == "unverified", concurrent)
    oracle.check("change during Unit: closure rejects changed setup", concurrent.get("closure", {}).get("matched") is False
                 and concurrent["closure"].get("before") != concurrent["closure"].get("after"), concurrent)
    oracle.check("concurrent result cannot finish", core.stable_status().get("finish_ok") is False)

    test.write_text(FAIL_TEST)
    core.stable_status()
    core.record(oracle, "failing Unit", "fail", 1)
    status = core.invoke("test.setup.skip", {"suite": "integration", "reason": "Private failed-Unit control"})
    oracle.check("Unit failure blocks finish", status.get("unit", {}).get("state") == "fail" and not status.get("finish_ok"), status)
    try:
        denied = core.invoke("test.setup.accept", {"reason": "Agent must not accept its own failure"},
                             {"kind": "principal", "principal": "control-agent"})
        oracle.check("non-owner acceptance refused", False, denied)
    except RuntimeError as failure:
        oracle.check("non-owner acceptance refused", "forbidden" in str(failure), str(failure))
    oracle.check("refused acceptance does not alter finish", core.status().get("finish_ok") is False)
    owner = core.call("workspace.snapshot")["entries"]["doc/principals/owner"]["value"]
    status = core.invoke("test.setup.accept", {"reason": "Owner accepts intentional private-control failure"}, {"kind": "owner"})
    exception = status.get("unit", {}).get("exception") or {}
    oracle.check("current owner exception allows finish without hiding fail", status.get("finish_ok") is True
                 and status["unit"].get("state") == "fail" and exception.get("accepted_by") == owner
                 and exception.get("setup_id") == status.get("setup_id") and exception.get("generation") == status.get("generation"), status)
    core.set_value("doc/settings/control", {"edit": 3})
    status = core.stable_status()
    oracle.check("next change expires owner exception", status.get("finish_ok") is False
                 and status.get("unit", {}).get("exception") is None, status)

    declaration = core.call("workspace.snapshot")["entries"]["extension/test-unit"]["value"]
    core.set_value("extension/test-unit", dict(declaration, enabled=False))
    core.settled({"test-unit": "off"})
    process = core.command(core.cli_args("--setup-status"))
    status = decode(oracle, "disabled guard status", process)
    reasons = " ".join(status.get("not_verified_reasons", []))
    oracle.check("disabled guard blocks with explicit reason", process.returncode == 0 and status.get("finish_ok") is False
                 and "testing extension is off, so changes are not tracked: turn it on, or ask the owner for an exception" in reasons, status)
    core.set_value("extension/test-unit", declaration)
    core.settled({"test-unit": "running"})
    wait_for(lambda: any(row.get("name") == "test.setup.status" for row in core.call("workspace.snapshot")["commands"]), "guard commands re-registered")

    # The default-off provider case has deterministic absence, not an invented
    # successful image digest or a permanent unknown setup identity.
    core.set_value("extension/ctl-absent", {"enabled": False, "argv": ["vqro-control-not-installed", "absent.py"]})
    status = core.stable_status()
    oracle.check("disabled missing executable has known setup identity", bool(re.fullmatch(r"[a-f0-9]{64}", str(status.get("setup_id")))), status)
    core.set_value("extension/ctl-absent", {"enabled": True, "argv": ["vqro-control-not-installed", "absent.py"]})
    core.settled({"ctl-absent": "failed"})
    status = core.stable_status()
    oracle.check("enabled unresolved executable remains unknown", status.get("setup_id") is None
                 and status.get("finish_ok") is False, status)
    core.set_value("extension/ctl-absent", {"enabled": False, "argv": ["python3", "absent.py"], "cwd": str(core.ext)})
    status = core.stable_status()
    oracle.check("disabled missing script has known setup identity", bool(re.fullmatch(r"[a-f0-9]{64}", str(status.get("setup_id")))), status)
    unreadable = core.ext / "absent.py"
    unreadable.write_text("# Existing but unreadable must not be treated as absent.\n")
    unreadable.chmod(0)
    try:
        status = core.stable_status()
        oracle.check("disabled existing unreadable script remains unknown", status.get("setup_id") is None
                     and status.get("finish_ok") is False, status)
    finally:
        unreadable.chmod(0o600)
        unreadable.unlink()
    core.call("workspace.apply", {"ops": [{"op": "delete", "key": "extension/ctl-absent"}]})

    # Produce a real failed Integration receipt rather than fabricating its
    # private binding fields. --out in the suite receipt area is explicit.
    test.write_text(PASS_TEST)
    scenarios = core.ext / "tests/ctl-pass/scenarios"
    scenarios.mkdir(exist_ok=True)
    (scenarios / "failure.json").write_text(json.dumps({"name": "intentional Integration failure", "steps": [
        {"expect": {"entry": "doc/control-never-created", "exists": True}}]}))
    core.stable_status()
    core.record(oracle, "Unit before Integration")
    output = core.state / "test-integration/receipt-independent.json"
    output.parent.mkdir(exist_ok=True)
    process = core.command([sys.executable, str(core.ext / "testing_integration.py"), "--json", "--vqro", str(core.binary),
                            "--backend", "fake", "--root", str(core.root / "i"), "--out", str(output)], timeout=180)
    integration = decode(oracle, "failed Integration", process)
    oracle.check("real Integration failure receipt exists", process.returncode == 1 and integration.get("verdict") == "fail"
                 and output.is_file(), integration)
    status = core.stable_status()
    oracle.check("Integration failed receipt adopted", status.get("integration", {}).get("state") == "fail"
                 and status["integration"].get("receipt") == str(output), status)
    try:
        status = core.invoke("test.setup.skip", {"suite": "integration", "reason": "Owner sees the failed receipt; no rerun requested"})
    except RuntimeError as failure:
        oracle.check("failed Integration cannot be relabeled skipped", "invalid" in str(failure), str(failure))
        status = core.status()
    oracle.check("later skip attempt retains failed Integration evidence", status.get("integration", {}).get("receipt") == str(output)
                 and status["integration"].get("state") == "fail", status)
    core.notice(oracle, "failed Integration remains visible")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--vqro", type=Path, default=Path(os.environ.get("CARGO_TARGET_DIR") or REPO / "target") / "debug/vqro")
    args = parser.parse_args()
    oracle = Oracle()
    scratch = REPO / ".run"
    scratch.mkdir(exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="fg-", dir=scratch))
    before = identities()
    core = None
    started = time.monotonic()
    try:
        core = FinishCore(root, args.vqro.resolve(), "g")
        check_gate(core, oracle)
    except Exception as failure:
        oracle.check("harness execution", False, repr(failure))
    finally:
        if core:
            core.stop()
    leftovers = []
    if Path("/proc").is_dir():
        marker = ("VQRO_SOCKET=" + str(root) + "/").encode()
        for directory in Path("/proc").iterdir():
            if directory.name.isdigit():
                try:
                    if any(value.startswith(marker) for value in (directory / "environ").read_bytes().split(b"\0")):
                        leftovers.append(directory.name)
                except OSError:
                    pass
    oracle.check("cleanup: no owned core/extension survivors", not leftovers, leftovers)
    oracle.check("cleanup: private socket removed", core is None or not core.sock.exists())
    failures = [row for row in oracle.checks if not row["ok"]]
    result = {"ok": not failures, "checks": len(oracle.checks), "failures": failures, "leftovers": leftovers,
              "worktree": {"before": before, "after": identities()}, "elapsed_seconds": round(time.monotonic() - started, 3),
              "evidence_root": str(root) if failures else None}
    if not failures:
        shutil.rmtree(root)
    print(json.dumps(result, indent=2))
    return int(bool(failures))


if __name__ == "__main__":
    sys.exit(main())
