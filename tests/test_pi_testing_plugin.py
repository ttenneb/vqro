"""Model-free plugin hooks and explicit per-launch asset selection, never a native/model session."""
import os
import json
import socketserver
import threading
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "extensions"))
from pi_testing import testing_args, testing_environment


class PiTestingPluginTests(unittest.TestCase):
    def test_only_managed_endpoint_and_state_are_forwarded(self):
        class Ext:
            info = {"socket": "opaque:selected-endpoint", "state_dir": "/private/state", "extension_token": "private"}
        self.assertEqual(testing_environment(Ext(), "/private/ext"), {"VQRO_TESTING_SOCKET": "opaque:selected-endpoint",
                                                      "VQRO_TESTING_STATE_DIR": "/private/state", "VQRO_TESTING_EXTENSIONS_DIR": "/private/ext"})
        self.assertEqual(testing_environment(object(), "/private/ext"), {})

    def test_unavailable_assets_warn_and_log_once_without_rejecting_launch(self):
        class Ext:
            def __init__(self): self.notices, self.logs = [], []
            def notify(self, message, style): self.notices.append((message, style))
            def log(self, message): self.logs.append(message)
        ext = Ext()
        with tempfile.TemporaryDirectory(prefix="pi-testing-unavailable-") as temporary:
            self.assertEqual(testing_args(temporary, ext), [])
            self.assertEqual(testing_args(temporary, ext), [])
        self.assertEqual(len(ext.notices), 1)
        self.assertEqual(len(ext.logs), 1)
        self.assertIn("testing plugin unavailable", ext.logs[0])
        self.assertEqual(ext.notices[0][1], "warn")

    def test_checkout_and_installed_asset_selection(self):
        args = testing_args(ROOT / "extensions")
        self.assertEqual(args[::2], ["-e", "--skill"])
        self.assertTrue(all(Path(path).is_file() for path in args[1::2]))
        with tempfile.TemporaryDirectory(prefix="pi-testing-assets-") as temporary:
            extensions = Path(temporary) / "extensions"
            destination = extensions / "tier1"
            destination.mkdir(parents=True)
            shutil.copyfile(ROOT / "integrations/pi/vqro-testing.ts", destination / "vqro-testing.ts")
            shutil.copytree(ROOT / "integrations/pi/skills", destination / "skills")
            installed = testing_args(extensions)
            self.assertTrue(all(Path(path).is_relative_to(extensions) for path in installed[1::2]))
            Path(installed[-1]).unlink()
            self.assertEqual(testing_args(extensions), [])

    @unittest.skipUnless(shutil.which("bun"), "Bun unavailable: combined CLI controls not executed")
    @unittest.skipUnless(hasattr(socketserver, "ThreadingUnixStreamServer"), "requires private Unix-stream fixture, not native Windows coverage")
    def test_real_recorded_unit_receipt_is_adopted_and_coverage_gaps_are_not_a_gate(self):
        # A minimal read-only RPC fixture, not a vqro core or native runtime.
        with tempfile.TemporaryDirectory(prefix="pi-checker-") as temporary:
            root = Path(temporary)
            extensions, state, home = root / "ext", root / "state", root / "home"
            for path in (extensions, state, home):
                path.mkdir()
            for name in ("testing_unit.py", "testing_setup.py", "vqro_testing.py", "vqro_process.py", "vqro_transport.py"):
                shutil.copyfile(ROOT / "extensions" / name, extensions / name)
            (extensions / "manifests").mkdir()
            image = root / "fixture-image"
            image.write_text("#!/bin/sh\nexit 0\n")
            image.chmod(0o700)
            names = ("sample", "without-tests")
            for name in names:
                (extensions / f"{name}.py").write_text("# private fixture\n")
                (extensions / "manifests" / f"{name}.json").write_text(json.dumps({"name": name}))
            tests = extensions / "tests/sample"
            tests.mkdir(parents=True)
            (tests / "test_sample.py").write_text("import unittest\nclass Sample(unittest.TestCase):\n def test_pass(self): self.assertTrue(True)\n")
            endpoint = root / "p.sock"
            hello = {"server": "vqro", "version": "private-fixture", "epoch": "fixture-epoch",
                     "socket": str(endpoint), "state_dir": str(state)}
            snapshot = {"epoch": "fixture-epoch", "seq": 1, "extensions": {"test-unit": {"enabled": True, "display_status": "running"}},
                        "entries": {"extension/" + name: {"version": 1, "value": {"argv": [str(image), name + ".py"], "enabled": True}} for name in names}}
            snapshot["entries"].update({"doc/settings/testing.require_unit": {"version": 1, "value": True},
                                         "doc/principals/owner": {"version": 1, "value": "human"}})
            facts = {name: {"enabled": True, "display_status": "running", "crashes": 0,
                            "manifest": {"name": name}, "dependencies": {"status": "ok"}} for name in names}
            methods = []

            class Handler(socketserver.StreamRequestHandler):
                def handle(self):
                    for line in self.rfile:
                        request = json.loads(line)
                        methods.append(request["method"])
                        values = {"hello": hello, "workspace.snapshot": snapshot, "extension.list": {"extensions": facts}, "resource.list": {"resources": []}}
                        result = values[request["method"]]
                        self.wfile.write((json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}) + "\n").encode())
                        self.wfile.flush()

            class Server(socketserver.ThreadingUnixStreamServer):
                daemon_threads = True

            server = Server(str(endpoint), Handler)
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            env = {"HOME": str(home), "PATH": os.environ.get("PATH", os.defpath), "VQRO_EXTENSIONS_DIR": str(root / "wrong-ambient-extensions"),
                   "VQRO_TESTING_EXTENSIONS_DIR": str(extensions), "VQRO_TESTING_SOCKET": str(endpoint), "VQRO_TESTING_STATE_DIR": str(state)}

            def drive(action):
                result = subprocess.run([shutil.which("bun"), "run", "tests/fixtures/pi_testing_contract_driver.ts", action],
                                        env=env, cwd=ROOT, capture_output=True, text=True, timeout=60)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                return json.loads(result.stdout)

            try:
                # Seed a current Integration receipt, then make it stale via the actual plugin-observed edit.
                from testing_setup import SetupStore, stamp_receipt
                from vqro_testing import write_receipt, utc_now, STATUSES
                prime = subprocess.run([sys.executable, "-B", str(extensions / "testing_unit.py"), "--setup-status", "--json",
                                        "--socket", str(endpoint), "--state-dir", str(state), "--extensions-dir", str(extensions)],
                                       env=env, cwd=ROOT, capture_output=True, text=True, timeout=30)
                self.assertEqual(prime.returncode, 0, prime.stdout + prime.stderr)
                store = SetupStore(state)
                context = store.context(store.read())
                prior = {"kind": "vqro.test-receipt", "version": 1, "suite": "integration", "verdict": "pass", "started_at": utc_now(),
                         "rows": [{"id": "scenario/private", "subject": "private", "status": "pass", "evidence": "fixture", "advice": None}],
                         "counts": {name: int(name == "pass") for name in STATUSES}}
                stamp_receipt(prior, context, context)
                (state / "test-integration").mkdir()
                write_receipt(prior, state / "test-integration/receipt-before.json")
                changed = drive("change")
                self.assertFalse(changed["finish_ok"], changed)
                self.assertEqual(changed["status"]["unit"]["state"], "pass", changed)
                self.assertEqual(changed["status"]["integration"]["state"], "stale")
                self.assertIn("no tests installed for without-tests", changed["status"]["coverage_gaps"])
                self.assertIn("Coverage gaps:", changed["summary"])
                receipts = list((state / "test-unit").glob("receipt-*.json"))
                self.assertEqual(len(receipts), 1)
                receipt = json.loads(receipts[0].read_text())
                self.assertTrue(receipt["recorded"])
                self.assertTrue(receipt["closure"]["matched"])
                self.assertEqual(receipt["setup_id"], changed["status"]["setup_id"])
                self.assertEqual(receipt["generation"], changed["status"]["generation"])
                self.assertEqual(receipt["instance"], changed["status"]["instance"])
                # Record an explicit Integration explanation through the real policy helper.
                observed = store.read()
                store.skip(observed["fingerprint"], snapshot, "integration", "private model-free fixture")
                finished = drive("finish")
                self.assertTrue(finished["finish_ok"], finished)
                self.assertIn("Coverage gaps:", finished["summary"])
                self.assertEqual(len(list((state / "test-unit").glob("receipt-*.json"))), 1)
                self.assertTrue(set(methods) <= {"hello", "workspace.snapshot", "extension.list", "resource.list"})
                env["VQRO_TESTING_SOCKET"] = str(root / "missing.sock")
                unavailable = drive("finish")
                self.assertFalse(unavailable["finish_ok"])
                self.assertIsNone(unavailable["status"])
                self.assertIn("NOT VERIFIED", unavailable["summary"])
                self.assertIn("exit 2", unavailable["error"])
                self.assertEqual(len(list((state / "test-unit").glob("receipt-*.json"))), 1)
            finally:
                server.shutdown()
                server.server_close()
                thread.join(timeout=2)

    @unittest.skipUnless(shutil.which("bun"), "Bun unavailable: real error-status CLI chain not executed")
    @unittest.skipUnless(hasattr(socketserver, "ThreadingUnixStreamServer"), "requires private Unix-stream fixture, not native Windows coverage")
    def test_fresh_policy_survives_real_producer_cli_exit_two_and_default_adapter(self):
        with tempfile.TemporaryDirectory(prefix="pi-policy-error-") as temporary:
            root = Path(temporary)
            extensions, state, home = root / "ext", root / "state", root / "home"
            for directory in (extensions, state, home):
                directory.mkdir()
            for name in ("testing_setup.py", "vqro_testing.py", "vqro_process.py", "vqro_transport.py"):
                shutil.copyfile(ROOT / "extensions" / name, extensions / name)
            shutil.copyfile(ROOT / "extensions/testing_unit.py", extensions / "real_unit.py")
            (extensions / "sample.py").write_text("# private fingerprint input\n")
            mode = state / "fixture-mode.json"
            mode.write_text(json.dumps({"policy": True}))
            # Fault injection surrounds the real producer/main, not synthetic frontend status.
            (extensions / "testing_unit.py").write_text('''import contextlib, io, json, pathlib, sys
import real_unit as unit
state=pathlib.Path(sys.argv[sys.argv.index('--state-dir')+1])
with (state/'fixture-calls.jsonl').open('a') as calls: calls.write(json.dumps(sys.argv)+'\\n')
assert '--setup-status' in sys.argv, 'must not replay Unit'
mode=json.loads((state/'fixture-mode.json').read_text())
def fail(*args, **kwargs): raise OSError('private evidence store fault')
unit.SetupStore.observe=fail
unit.SetupStore.preview=fail
if mode.get('malformed'):
 with contextlib.redirect_stdout(io.StringIO()) as captured: code=unit.main()
 assert code==2
 value=json.loads(captured.getvalue())
 kind=mode['malformed']
 if kind=='text': print('not JSON')
 elif kind=='partial': print(json.dumps({'require_unit':True,'policy_state':'on'}))
 else:
  if kind=='finish': value['finish_ok']=True
  if kind=='extra': value['arbitrary']=True
  if kind=='version': value['version']=2
  print(json.dumps(value))
 raise SystemExit(2)
raise SystemExit(unit.main())
''')
            endpoint = root / "p.sock"
            hello = {"server": "vqro", "version": "private-fixture", "epoch": "private", "socket": str(endpoint), "state_dir": str(state)}
            snapshot = {"epoch": "private", "entries": {"extension/sample": {"version": 1, "value": {
                "argv": [sys.executable, "sample.py"], "enabled": True}}, "extension/pi": {"version": 1, "value": {
                "argv": [sys.executable, "sample.py"], "enabled": True}}}}
            methods = []

            class Handler(socketserver.StreamRequestHandler):
                def handle(self):
                    for line in self.rfile:
                        request = json.loads(line)
                        methods.append(request["method"])
                        config = json.loads(mode.read_text())
                        if request["method"] == "workspace.snapshot":
                            if config.get("unavailable"):
                                response = {"error": {"code": -32000, "message": "private snapshot unavailable"}}
                            else:
                                snapshot["entries"]["doc/settings/testing.require_unit"] = {"version": 1, "value": config["policy"]}
                                response = {"result": snapshot}
                        elif request["method"] == "resource.list":
                            assert request["params"] == {"kind": "agent"}
                            response = {"result": {"resources": []}}
                        else:
                            assert request["method"] == "hello"
                            response = {"result": hello}
                        self.wfile.write((json.dumps({"jsonrpc": "2.0", "id": request["id"], **response}) + "\n").encode())
                        self.wfile.flush()

            class Server(socketserver.ThreadingUnixStreamServer):
                daemon_threads = True

            server = Server(str(endpoint), Handler)
            thread = threading.Thread(target=server.serve_forever, daemon=True)
            thread.start()
            env = {"HOME": str(home), "PATH": os.environ.get("PATH", os.defpath), "VQRO_TESTING_EXTENSIONS_DIR": str(extensions),
                   "VQRO_TESTING_SOCKET": str(endpoint), "VQRO_TESTING_STATE_DIR": str(state)}
            try:
                direct = subprocess.run([sys.executable, "-B", str(extensions / "testing_unit.py"), "--setup-status", "--json",
                                         "--socket", str(endpoint), "--state-dir", str(state), "--extensions-dir", str(extensions)],
                                        env=env, capture_output=True, text=True, timeout=30)
                self.assertEqual(direct.returncode, 2, direct.stdout + direct.stderr)
                document = json.loads(direct.stdout)
                self.assertEqual(document["policy_state"], "on")
                self.assertTrue(document["require_unit"])
                self.assertFalse(document["finish_ok"])
                self.assertIn("NOT VERIFIED: rule not active for vqro-launched Pi agents yet", document["not_verified_reasons"])
                driven = subprocess.run([shutil.which("bun"), "run", "tests/fixtures/pi_testing_policy_error_driver.ts"],
                                        cwd=ROOT, env=env, capture_output=True, text=True, timeout=60)
                self.assertEqual(driven.returncode, 0, driven.stdout + driven.stderr)
                result = json.loads(driven.stdout)
                for field in ("prompt", "context"):
                    self.assertIn("Current setup policy ON", result["on"][field])
                    self.assertIn("vqro_setup_finish is required", result["on"][field])
                    self.assertIn("Current setup policy OFF", result["off"][field])
                    for case in (result["invalidPolicy"], result["unavailable"], *result["malformed"]):
                        self.assertIn("UNKNOWN/UNAVAILABLE", case[field])
                        self.assertNotIn("Current setup policy ON", case[field])
                for case in (result["on"], result["off"], result["invalidPolicy"], result["unavailable"], *result["malformed"]):
                    self.assertFalse(case["finish"]["finish_ok"])
                    self.assertIsNone(case["finish"]["status"])
                    self.assertIn("NOT VERIFIED", case["finish"]["summary"])
                self.assertEqual(result["providerRequests"], 0)
                calls = [json.loads(line) for line in (state / "fixture-calls.jsonl").read_text().splitlines()]
                self.assertTrue(all("--setup-status" in call and "--record" not in call for call in calls))
                self.assertTrue(set(methods) <= {"hello", "workspace.snapshot", "resource.list"})
            finally:
                server.shutdown()
                server.server_close()
                thread.join(timeout=2)

    @unittest.skipUnless(shutil.which("bun"), "Bun unavailable: model-free TypeScript controls not executed")
    def test_model_free_hook_controls(self):
        with tempfile.TemporaryDirectory(prefix="pi-testing-home-") as home:
            result = subprocess.run([shutil.which("bun"), "test", "tests/pi_testing_plugin.test.ts"], cwd=ROOT,
                                    env={"HOME": home, "PATH": os.environ.get("PATH", os.defpath)},
                                    capture_output=True, text=True, timeout=60)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
