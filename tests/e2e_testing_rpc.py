#!/usr/bin/env python3
"""Actual Rust bridge + private core + real testing hooks in a model-free native RPC double."""
import argparse
import hashlib
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
from vqro_testing import SandboxClient, process_identity, private_processes
from testing_setup import RPC_RULE_NOTICE


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--vqro", type=Path, required=True)
    parser.add_argument("--bridge", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--pi-cli", type=Path, help="Optional actual Pi CLI: read-only startup, never input or a model request")
    args = parser.parse_args()
    (REPO / ".run").mkdir(exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="rpc-", dir=REPO / ".run"))
    root.chmod(0o700)
    checks, processes, files = [], [], []
    client = None
    error = None
    started = time.monotonic()

    def check(name, condition, actual=None):
        checks.append({"name": name, "ok": bool(condition), **({"actual": actual} if not condition else {})})
        if not condition:
            raise AssertionError(name + ": " + str(actual))

    def wait(read, predicate, seconds=60):
        deadline, last = time.monotonic() + seconds, None
        while time.monotonic() < deadline:
            last = read()
            if predicate(last):
                return last
            time.sleep(.025)
        raise TimeoutError("observation deadline: " + str(last))

    def spawn(argv, env, label):
        log = (root / (label + ".log")).open("w")
        files.append(log)
        process = subprocess.Popen(argv, cwd=root, env=env, stdin=subprocess.PIPE, stdout=log,
                                   stderr=subprocess.STDOUT, start_new_session=True)
        identity = process_identity(process.pid)
        processes.append((process, identity))
        return process

    try:
        ext, state = root / "ext", root / "state"
        shutil.copytree(REPO / "extensions", ext, ignore=shutil.ignore_patterns("__pycache__"))
        shutil.copytree(REPO / "integrations/pi", ext / "tier1", dirs_exist_ok=True)
        defaults = json.loads((ext / "defaults.json").read_text())
        for declaration in defaults["extensions"]:
            declaration["enabled"] = False
        (ext / "defaults.json").write_text(json.dumps(defaults))
        (ext / "fixture.py").write_text("from vqro_ext import Extension\ne=Extension('fixture',event_types=[])\ne.contribute({})\ne.run()\n")
        (ext / "manifests/fixture.json").write_text('{"name":"fixture","provides":[]}')
        (ext / "manifests/rpc-fixture.json").write_text('{"name":"rpc-fixture","kind":"provider","provides":["vqro.terminals"]}')
        for directory in ("home", "state", "run", "config", "xstate", "pi", "bin", "tmp"):
            (root / directory).mkdir()
        env = {"HOME": str(root / "home"), "PATH": os.environ["PATH"], "VQRO_SOCKET": str(root / "p.sock"),
               "VQRO_STATE_DIR": str(state), "VQRO_EXTENSIONS_DIR": str(ext), "VQRO_HERDR": '["/bin/false"]',
               "XDG_RUNTIME_DIR": str(root / "run"), "XDG_STATE_HOME": str(root / "xstate"),
               "XDG_CONFIG_HOME": str(root / "config"), "HC_SOCKET": str(root / "h.sock"), "HC_SESSION": "rpc-fixture",
               "PI_CODING_AGENT_DIR": str(root / "pi"), "PI_SKIP_VERSION_CHECK": "1",
               "TMPDIR": str(root / "tmp"), "TEMP": str(root / "tmp"), "TMP": str(root / "tmp")}
        core = spawn([str(args.vqro), "serve", "--safe", "--fake"], env, "core")
        wait(lambda: (root / "p.sock").exists(), bool)
        client = SandboxClient(root / "p.sock", root)
        client.call("hello", {"client": "cli", "identity": {"kind": "owner"}})
        snapshot = client.call("workspace.snapshot")
        operations = [{"op": "delete", "key": key} for key in snapshot["entries"]
                      if key.startswith("extension/") and key != "extension/test-unit"]
        operations += [{"op": "set", "key": "extension/test-unit", "value": {"argv": [sys.executable, str(ext / "testing_unit.py")], "enabled": True}},
                       {"op": "set", "key": "extension/fixture", "value": {"argv": [sys.executable, str(ext / "fixture.py")], "enabled": True}},
                       {"op": "set", "key": "extension/rpc-fixture", "value": {"argv": [sys.executable, "-I", str(REPO / "tests/fixtures/testing_rpc_backend.py"), "--root", str(root)], "kind": "provider", "enabled": True}},
                       {"op": "set", "key": "doc/settings/testing.require_unit", "value": True}]
        client.apply(operations)
        client.invoke("vqro.safe_mode", {"on": False})
        wait(lambda: client.call("extension.list")["extensions"],
             lambda rows: all(rows.get(name, {}).get("display_status") == "running"
                              for name in ("test-unit", "rpc-fixture", "fixture")))
        wait(lambda: client.call("command.list")["commands"], lambda rows: any(row["name"] == "test.setup.status" and row.get("available", True) for row in rows))

        def launch(label, argv, count):
            selected = {"VQRO_PRINCIPAL": "p:rpc-" + label, "VQRO_TESTING_SOCKET": env["VQRO_SOCKET"],
                        "VQRO_TESTING_STATE_DIR": str(state), "VQRO_TESTING_EXTENSIONS_DIR": "must-not-forward",
                        "VQRO_BAD": "must-not-forward", "HERDR_BAD": "must-not-forward",
                        "FIXTURE_RPC_ROOT": str(root), "FIXTURE_RPC_LABEL": label}
            answer = client.call("resource.spawn", {"provider": "rpc-fixture", "bridge": True, "argv": argv, "env": selected})
            captured = root / ("spawn-f" + str(count) + ".json")
            wait(captured.exists, bool)
            request = json.loads(captured.read_text())
            process = spawn(request["argv"], {**env, **request["env"]}, label)
            return answer["resource"]["id"], process

        native = [shutil.which("bun"), str(REPO / "tests/fixtures/testing_rpc_native.ts")]
        host, new = launch("new", [str(args.bridge), "--testing-plugin", str(ext / "tier1/vqro-testing.ts"), "--", *native], 1)
        rows = wait(lambda: client.call("resource.list", {"host": host})["resources"],
                    lambda rows: any(row.get("kind") == "agent" and row.get("data", {}).get("agent", {}).get("testing_plugin", {}).get("state") == "loaded" for row in rows))
        agent = next(row for row in rows if row.get("kind") == "agent")
        check("bridge confirms the actual plugin command and skill inventory", agent["data"]["agent"]["testing_plugin"]["source"] == "rpc:get_commands")
        global_status = client.invoke("test.setup.status")
        scoped = client.invoke("test.setup.status", {"rpc_principal": "p:rpc-new"})
        check("global caveat remains", RPC_RULE_NOTICE in global_status["not_verified_reasons"])
        page = client.call("workspace.snapshot").get("live", {}).get("settings.page.testing", {})
        check("global page discloses observed launch state", any("Observed RPC launch " + agent["id"] in item.get("text", "") for item in page.get("items", [])), page)
        check("only confirmed launch clears its caveat", scoped["rpc_testing"]["verified"] and RPC_RULE_NOTICE not in scoped["not_verified_reasons"], scoped)
        reply = client.call("resource.invoke", {"resource": agent["id"], "op": "agent.send", "deadline_ms": 60000,
                            "args": {"v": 0, "mode": "start", "delivery": "fixture-once", "origin": {"kind": "human"},
                                     "content": [{"type": "text", "text": "model-free hook fixture"}]}})
        check("one model-free native input dispatched", reply["outcome"] in ("accepted", "observed", "dispatched"), reply)
        trace = wait(lambda: json.loads((root / "new-native.json").read_text()), lambda value: "agent_before_settle" in value["events"])
        check("actual hooks execute in RPC order", trace["events"] == ["session_start", "before_agent_start", "tool_result", "agent_before_settle"], trace)
        check("actual finish tool refuses missing Integration evidence", trace["finishRefused"] is True, trace)
        check("no provider requests or continuation", trace["providerRequests"] == 0 and "continue" not in trace["settle"], trace)
        plugin_status = trace["settle"]["entries"][-1]["details"]["status"]
        check("RPC plugin CLI selects its own confirmed scope", plugin_status["rpc_testing"]["verified"] and RPC_RULE_NOTICE not in plugin_status["not_verified_reasons"], plugin_status)
        check("only two extra protected names reach native", set(trace["protectedEnvironment"]) == {"VQRO_SOCKET", "VQRO_PRINCIPAL", "VQRO_TESTING_SOCKET", "VQRO_TESTING_STATE_DIR"}, trace)
        check("managed selectors reach native unchanged", trace["selectors"] == {"socket": env["VQRO_SOCKET"], "state": str(state)}, trace)
        after = client.invoke("test.setup.status", {"rpc_principal": "p:rpc-new"})
        check("tool edit records current Unit but never invents Integration", after["unit"]["state"] == "pass" and after["integration"]["state"] == "missing" and not after["finish_ok"], after)

        # An older bridge executable advertises no field; its single actual launch delegates to the new binary
        # without the optional field. This tests the real host wrapper's downgrade, not a launch retry.
        old = root / "bin/vqro-bridge-pi"
        old.write_text("#!" + sys.executable + "\nimport os,sys\nif '--help' in sys.argv: print('old bridge');sys.exit(0)\nos.execv(" + repr(str(args.bridge)) + ",[" + repr(str(args.bridge)) + ",*sys.argv[1:]])\n")
        old.chmod(0o700)
        old_env = env["PATH"]
        env["PATH"] = str(root / "bin") + os.pathsep + old_env
        old_host, old_process = launch("old", [sys.executable, "-I", str(ext / "pi_bridge/launch.py"), "--testing-plugin", str(ext / "tier1/vqro-testing.ts"), "--", *native], 2)
        wait(lambda: client.call("resource.list", {"host": old_host})["resources"], lambda rows: any(row.get("kind") == "agent" and row.get("available") for row in rows))
        old_status = client.invoke("test.setup.status", {"rpc_principal": "p:rpc-old"})
        check("older bridge launches once but remains NOT VERIFIED", old_process.poll() is None and not old_status["rpc_testing"]["verified"] and RPC_RULE_NOTICE in old_status["not_verified_reasons"], old_status)
        check("old launch did not register hooks", not json.loads((root / "old-native.json").read_text())["commands"])
        check("one confirmed launch never upgrades another", client.invoke("test.setup.status", {"rpc_principal": "p:rpc-new"})["rpc_testing"]["verified"])
        env["PATH"] = old_env

        # Arbitrary native -e remains rejected before the fixture can execute.
        _, rejected = launch("explicit", [str(args.bridge), "--", *native, "-e", str(ext / "tier1/vqro-testing.ts")], 3)
        rejected.wait(timeout=10)
        check("arbitrary native -e is refused before any child hooks", rejected.returncode != 0 and not (root / "explicit-native.json").exists())
        # An inadmissible optional path is refused without vetoing the underlying launch.
        _, outside = launch("outside", [str(args.bridge), "--testing-plugin", str(REPO / "integrations/pi/vqro-testing.ts"), "--", *native], 4)
        wait(lambda: (root / "outside-native.json").exists(), bool)
        check("outside plugin is never loaded and does not veto launch", outside.poll() is None and not json.loads((root / "outside-native.json").read_text())["commands"])
        legacy_host, legacy = launch("legacy", [sys.executable, "-I", str(ext / "pi_bridge/python_launch.py"),
            "--testing-plugin", str(ext / "tier1/vqro-testing.ts"), "--", *native, "--mode", "rpc"], 5)
        wait(lambda: client.call("resource.list", {"host": legacy_host})["resources"],
             lambda rows: any(row.get("data", {}).get("agent", {}).get("testing_plugin", {}).get("state") == "loaded" for row in rows))
        legacy_scope = client.invoke("test.setup.status", {"rpc_principal": "p:rpc-legacy"})
        legacy_trace = json.loads((root / "legacy-native.json").read_text())
        check("legacy bridge also confirms native command/skill evidence", legacy.poll() is None and legacy_scope["rpc_testing"]["verified"], legacy_scope)
        check("legacy forwards only two new VQRO selectors plus its existing marker", set(legacy_trace["protectedEnvironment"]) ==
              {"VQRO_SOCKET", "VQRO_PRINCIPAL", "VQRO_SOCKET_SOURCE", "VQRO_TESTING_SOCKET", "VQRO_TESTING_STATE_DIR"}, legacy_trace)
        if args.pi_cli:
            # Only discovery/state/metadata reads are made to actual Pi, with an empty private agent directory.
            actual_host, actual = launch("actual", [str(args.bridge), "--testing-plugin", str(ext / "tier1/vqro-testing.ts"), "--",
                shutil.which("node"), str(args.pi_cli), "--no-session", "--no-extensions", "--no-skills", "--no-prompt-templates"], 6)
            actual_rows = wait(lambda: client.call("resource.list", {"host": actual_host})["resources"],
                               lambda rows: any(row.get("data", {}).get("agent", {}).get("testing_plugin", {}).get("state") == "loaded" for row in rows))
            check("actual Pi native inventory confirms installed plugin and skill", actual.poll() is None and any(row.get("kind") == "agent" for row in actual_rows))
            actual_scope = client.invoke("test.setup.status", {"rpc_principal": "p:rpc-actual"})
            check("actual Pi scope clears only its RPC caveat", actual_scope["rpc_testing"]["verified"] and RPC_RULE_NOTICE not in actual_scope["not_verified_reasons"], actual_scope)
    except Exception as failure:
        error = str(failure)
    finally:
        for process, identity in reversed(processes):
            current = process_identity(process.pid)
            if current and identity and current[0] == identity[0]:
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=3)
                except ProcessLookupError:
                    pass
        if client:
            client.close()
        for stream in files:
            stream.close()
        # Reap only processes carrying this unique private endpoint/state namespace.
        survivors = private_processes(root, 0) or []
        for row in survivors:
            current = process_identity(row["pid"])
            if current and current[0] == row["start_ticks"]:
                os.kill(row["pid"], signal.SIGKILL)
        checks.append({"name": "zero private process survivors", "ok": not survivors, **({"actual": survivors} if survivors else {})})
        ok = error is None and all(row["ok"] for row in checks)
        result = {"ok": ok, "checks": checks, "error": error, "elapsed_s": round(time.monotonic() - started, 3),
                  "pin": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=REPO, text=True).strip(),
                  "bridge_sha256": hashlib.sha256(args.bridge.read_bytes()).hexdigest(),
                  "pi_cli_sha256": hashlib.sha256(args.pi_cli.read_bytes()).hexdigest() if args.pi_cli else None,
                  "evidence_root": None if ok else str(root)}
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(json.dumps(result, indent=2))
        print(json.dumps(result, indent=2))
        if ok:
            shutil.rmtree(root)
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
