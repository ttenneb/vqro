"""The e2e item-streams harness fails promptly, naming the process, when its core or fake provider dies before the
agents register (it used to wait forever). Stub core and provider binaries; nothing is built."""
import json, os, subprocess, sys, tempfile, time, unittest
from pathlib import Path

HARNESS = Path(__file__).resolve().parent / "e2e_item_streams.py"

# A stand-in core: serves hello and an empty resource.list on VQRO_SOCKET until core.shutdown.
CORE = r'''
import json, os, socket, sys
if os.path.exists(os.path.join(os.path.dirname(__file__), "core-dies")):
    sys.stderr.write("stub core: fatal before listening\n"); sys.exit(7)
s = socket.socket(socket.AF_UNIX); s.bind(os.environ["VQRO_SOCKET"]); s.listen(8)
while True:
    c, _ = s.accept(); f = c.makefile("rb")
    for line in f:
        m = json.loads(line)
        r = {"resources": []} if m["method"] == "resource.list" else {}
        c.sendall((json.dumps({"jsonrpc": "2.0", "id": m["id"], "result": r}) + "\n").encode())
        if m["method"] == "core.shutdown": sys.exit(0)
    c.close()
'''
PROVIDER = 'import sys\nsys.stderr.write("stub provider: cannot reach the core\\n")\nsys.exit(3)\n'


class HarnessFailsFast(unittest.TestCase):
    def run_harness(self, core_dies=False):
        with tempfile.TemporaryDirectory() as td:
            debug = Path(td) / "target" / "debug"
            debug.mkdir(parents=True)
            for name, body in (("vqro", CORE), ("vqro-fake-items", PROVIDER)):
                p = debug / name
                p.write_text(f"#!{sys.executable}\n{body}")
                p.chmod(0o755)
            if core_dies: (debug / "core-dies").touch()
            env = {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "CARGO_TARGET_DIR": str(Path(td) / "target")}
            t0 = time.monotonic()
            r = subprocess.run([sys.executable, str(HARNESS), str(Path(td) / "out"), "--agents", "3"],
                               env=env, capture_output=True, text=True, timeout=60)
            return r, time.monotonic() - t0, Path(td) / "out"

    def test_provider_exiting_before_registration_fails_fast(self):
        r, took, out = self.run_harness()
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        result = json.loads(r.stdout.strip().splitlines()[-1])
        self.assertEqual(result["result"], "fail")
        msg = " ".join(result["failures"])
        self.assertIn("provider", msg)
        self.assertIn("exit status 3", msg)
        self.assertIn("cannot reach the core", msg)
        self.assertLess(took, 20)

    def test_core_exiting_before_listening_fails_fast(self):
        r, took, out = self.run_harness(core_dies=True)
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        msg = " ".join(json.loads(r.stdout.strip().splitlines()[-1])["failures"])
        self.assertIn("core", msg)
        self.assertIn("exit status 7", msg)
        self.assertIn("fatal before listening", msg)
        self.assertLess(took, 20)


if __name__ == "__main__":
    unittest.main()
