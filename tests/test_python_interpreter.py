"""Resolver controls: old system Python first, override, and clear failures."""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

RESOLVER = Path(__file__).resolve().parents[1] / "scripts/test-python.sh"


class InterpreterTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="python-resolver-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.old = self.root / "old"
        self.real = self.root / "real interpreter"
        self.old.mkdir()
        self.real.mkdir()
        # Behave like 3.9 for the minimum-version probe, never execute test code.
        (self.old / "python3").write_text("#!/bin/sh\nexit 1\n")
        (self.old / "python3").chmod(0o700)
        (self.real / "python3").symlink_to(sys.executable)
        self.env = {"PATH": os.pathsep.join(map(str, (self.old, self.real)))}

    def resolve(self, **env):
        return subprocess.run(["/bin/sh", str(RESOLVER)], env=self.env | env,
                              text=True, capture_output=True, timeout=10)

    def test_skips_python_39_and_selects_later_supported_interpreter(self):
        result = self.resolve()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(Path(result.stdout.strip()), self.real / "python3")

    def test_explicit_override_wins_even_with_nonstandard_basename(self):
        override = self.root / "chosen-python"
        override.symlink_to(sys.executable)
        result = self.resolve(VQRO_PYTHON=str(override))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), str(override))

    def test_invalid_or_old_override_does_not_silently_fall_back(self):
        for override in ("", str(self.old / "python3"), str(self.root / "absent")):
            with self.subTest(override=override):
                result = self.resolve(VQRO_PYTHON=override)
                self.assertEqual(result.returncode, 2)
                self.assertIn("VQRO_PYTHON", result.stderr)

    def test_no_supported_interpreter_reports_the_minimum(self):
        self.env["PATH"] = str(self.old)
        result = self.resolve()
        self.assertEqual(result.returncode, 2)
        self.assertIn("Python >= 3.11", result.stderr)
