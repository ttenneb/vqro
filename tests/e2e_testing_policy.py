#!/usr/bin/env python3
"""Private core policy-switch/attribution/undo proof. Scratch files retained, never pruned here."""
import argparse
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time

REPO = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO / "extensions"))
from vqro_testing import SandboxClient, private_processes, process_identity

KEY = "doc/settings/testing.require_unit"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--vqro", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    (REPO / ".run").mkdir(exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="tsw-", dir=REPO / ".run"))
    root.chmod(0o700)
    checks, clients = [], []
    process = log = None
    error = None
    def check(name, condition, actual=None):
        checks.append({"name": name, "ok": bool(condition), **({"actual": actual} if not condition else {})})
        if not condition:
            raise AssertionError(name + ": " + str(actual))
    def wait(read, accept):
        deadline, last = time.monotonic() + 30, None
        while time.monotonic() < deadline:
            last = read()
            if accept(last):
                return last
            time.sleep(.02)
        raise TimeoutError(str(last))
    try:
        ext, state = root / "ext", root / "state"
        shutil.copytree(REPO / "extensions", ext, ignore=shutil.ignore_patterns("__pycache__"))
        defaults = json.loads((ext / "defaults.json").read_text())
        for declaration in defaults["extensions"]:
            declaration["enabled"] = False
        (ext / "defaults.json").write_text(json.dumps(defaults))
        for name in ("home", "state", "run", "config", "tmp"):
            (root / name).mkdir()
        env = {"HOME": str(root / "home"), "PATH": os.environ["PATH"], "VQRO_SOCKET": str(root / "p.sock"),
               "VQRO_STATE_DIR": str(state), "VQRO_EXTENSIONS_DIR": str(ext), "VQRO_HERDR": '["/bin/false"]',
               "HC_SOCKET": str(root / "h.sock"), "HC_SESSION": "policy-fixture", "XDG_RUNTIME_DIR": str(root / "run"),
               "XDG_CONFIG_HOME": str(root / "config"), "TMPDIR": str(root / "tmp")}
        log = (root / "core.log").open("w")
        process = subprocess.Popen([str(args.vqro), "serve", "--safe", "--fake"], env=env, cwd=root,
                                   stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
        birth = process_identity(process.pid)
        wait(lambda: (root / "p.sock").exists(), bool)
        owner = SandboxClient(root / "p.sock", root)
        clients.append(owner)
        owner.call("hello", {"client": "cli", "identity": {"kind": "owner"}})
        snapshot = owner.call("workspace.snapshot")
        ops = [{"op": "delete", "key": key} for key in snapshot["entries"] if key.startswith("extension/") and key != "extension/test-unit"]
        ops.append({"op": "set", "key": "extension/test-unit", "value": {"argv": [sys.executable, str(ext / "testing_unit.py")], "enabled": True}})
        owner.apply(ops)
        owner.invoke("vqro.safe_mode", {"on": False})
        wait(lambda: owner.call("command.list")["commands"], lambda rows: any(row["name"] == "test.setup.policy" and row.get("available", True) for row in rows))
        def value():
            return owner.call("workspace.snapshot")["entries"].get(KEY)
        def row():
            page = owner.call("workspace.snapshot")["live"]["settings.page.testing"]
            return next(item for item in page["items"] if item.get("command") == "test.setup.policy")
        initial = owner.invoke("test.setup.status")
        check("missing policy displays OFF checkbox", row()["type"] == "toggle" and row()["checked"] is False)
        on = owner.invoke("test.setup.policy", {"on": True})
        check("ON is a setup change requiring fresh evidence", value()["value"] is True and on["policy_state"] == "on" and not on["finish_ok"] and on["setup_id"] != initial["setup_id"], on)
        history = owner.call("workspace.history", {"limit": 1})["txns"][0]
        check("command mutation belongs to human invoker, not extension undo scope", history["principal"] == "owner" and history["initiator"] == "owner" and history["actor"] == "extension:test-unit" and history["author"].startswith("client:"), history)
        owner.invoke("vqro.revert", {})
        owner.invoke("test.setup.status")
        check("human default undo reverts ON to absence", value() is None and row()["checked"] is False)
        owner.invoke("test.setup.policy", {"on": True})
        off = owner.invoke("test.setup.policy", {"on": False})
        check("OFF writes false without evidence prerequisite", value()["value"] is False and off["policy_state"] == "off")
        owner.invoke("vqro.revert", {})
        owner.invoke("test.setup.status")
        check("human default undo of OFF restores ON", value()["value"] is True and row()["checked"] is True)
        owner.apply([{"op": "set", "key": KEY, "value": "false"}])
        unknown = owner.invoke("test.setup.status")
        unknown_row = row()
        check("malformed value stays unknown, never unchecked OFF", unknown["policy_state"] == "unknown" and unknown_row["type"] == "item" and unknown_row["detail"] == "unknown" and "checked" not in unknown_row, unknown_row)
        check("unknown row offers explicit ON and OFF", [action["args"] for action in unknown_row["menu"]] == [{"on": True}, {"on": False}])
        owner.invoke("test.setup.policy", {"on": False})
        owner.invoke("vqro.revert", {})
        owner.invoke("test.setup.status")
        check("undo preserves the prior malformed value honestly", value()["value"] == "false" and row()["detail"] == "unknown")
        agent = SandboxClient(root / "p.sock", root)
        clients.append(agent)
        agent.call("hello", {"client": "cli", "identity": {"kind": "principal", "principal": "policy-agent"}})
        agent.invoke("test.setup.policy", {"on": False})
        history = owner.call("workspace.history", {"limit": 1})["txns"][0]
        check("cooperative agent toggle retains its own attribution", value()["value"] is False and history["principal"] == "policy-agent" and history["actor"] == "extension:test-unit", history)
        agent.invoke("vqro.revert", {})
        check("agent default undo reverts its own toggle", value()["value"] == "false")
    except Exception as failure:
        error = str(failure)
    finally:
        if process is not None:
            current = process_identity(process.pid)
            if current and birth and current[0] == birth[0]:
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=3)
                except ProcessLookupError:
                    pass
        for client in clients:
            client.close()
        if log:
            log.close()
        survivors = private_processes(root, 0) or []
        checks.append({"name": "no private process survivors", "ok": not survivors, **({"actual": survivors} if survivors else {})})
        result = {"ok": error is None and all(check["ok"] for check in checks), "checks": checks, "error": error,
                  "scratch_retained": str(root), "pin": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=REPO, text=True).strip()}
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(result, indent=2))
        print(json.dumps(result, indent=2))
    return 0 if result["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
