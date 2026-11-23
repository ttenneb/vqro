"""Powerless controls for check.sh's Windows preflight; only fake rustup/cargo run."""
from pathlib import Path
import subprocess
import tempfile
import unittest
import sys

SOURCE = (Path(__file__).resolve().parents[1] / "scripts/check.sh").read_text(encoding="utf-8")
if sys.platform == "win32":
    raise unittest.SkipTest("checks the Linux cross-target Bash gate with POSIX executable/symlink stubs")

START = SOURCE.index("windows_check() {")
FUNCTION = SOURCE[START:SOURCE.index("\n}", START) + 2]


class WindowsCheckTests(unittest.TestCase):
    def run_check(self, inventory=None, cargo_exit=0):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tools = root / "bin"
            tools.mkdir()
            # No real rustup or cargo is reachable, even for the missing-tool case.
            for name in ("env", "grep", "tee"):
                (tools / name).symlink_to("/usr/bin/" + name)
            clean = 'if [ "${HERDR_TEST+x}${POISON+x}" != "" ]; then echo inherited-environment >&2; exit 99; fi\n'
            if inventory is not None:
                rustup = tools / "rustup"
                rustup.write_text("#!/bin/bash\n" + clean + inventory, encoding="utf-8")
                rustup.chmod(0o700)
            cargo = tools / "cargo"
            cargo.write_text("#!/bin/bash\n" + clean + 'echo fake-cargo-invoked\n' + f"exit {cargo_exit}\n", encoding="utf-8")
            cargo.chmod(0o700)
            return subprocess.run(["/bin/bash", "-c", 'set -o pipefail\nSKIP_EXIT=77\nTARGET=$HOME/target\n' + FUNCTION + "\nwindows_check"],
                                  env={"HOME": directory, "PATH": str(tools), "HERDR_TEST": "forbidden", "POISON": "forbidden"},
                                  text=True, capture_output=True, timeout=5)

    def test_inventory_failure_is_failure_and_keeps_stderr(self):
        result = self.run_check('echo inventory-broken >&2\nexit 23\n')
        self.assertEqual(result.returncode, 1)
        self.assertIn("inventory-broken", result.stderr)
        self.assertIn("FAIL Windows check", result.stderr)
        self.assertNotIn("SKIP", result.stdout + result.stderr)
        self.assertNotIn("fake-cargo-invoked", result.stdout + result.stderr)

    def test_missing_rustup_is_failure_not_skip(self):
        result = self.run_check()
        self.assertEqual(result.returncode, 1)
        self.assertIn("rustup", result.stderr)
        self.assertNotIn("SKIP", result.stdout + result.stderr)

    def test_successful_inventory_without_target_is_skip(self):
        result = self.run_check('echo x86_64-unknown-linux-gnu\n')
        self.assertEqual(result.returncode, 77)
        self.assertIn("not installed", result.stdout)
        self.assertNotIn("fake-cargo-invoked", result.stdout + result.stderr)

    def test_inventory_and_cargo_receive_sanitized_environment(self):
        result = self.run_check('echo x86_64-pc-windows-gnu\n')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("fake-cargo-invoked", result.stdout + result.stderr)
        self.assertNotIn("inherited-environment", result.stdout + result.stderr)

    def test_cargo_failure_remains_failure(self):
        result = self.run_check('echo x86_64-pc-windows-gnu\n', cargo_exit=31)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotEqual(result.returncode, 77)
