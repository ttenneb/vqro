"""Portable pure resolver controls plus Unix bootstrap integration controls."""
import importlib.util
import os
from pathlib import Path
import runpy
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
RESOLVER = ROOT / "scripts/test-python.sh"
SPEC = importlib.util.spec_from_file_location("test_python_resolver", ROOT / "scripts/test_python.py")
resolver = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(resolver)


class PureInterpreterTests(unittest.TestCase):
    def test_adapter_import_is_lazy_even_without_a_shell(self):
        with patch.object(subprocess, 'Popen', side_effect=AssertionError('import launched a child')):
            imported = runpy.run_path(str(ROOT / 'tests/python_interpreter.py'))
        self.assertTrue(callable(imported['python']))

    def test_windows_uses_native_python_without_shell(self):
        with patch.object(resolver, 'WINDOWS', True), patch.dict(os.environ, {'PATH': ''}, clear=True), \
                patch.object(resolver.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0)) as run:
            selected = resolver.resolve()
        self.assertEqual(selected, os.path.abspath(sys.executable))
        self.assertEqual(run.call_args.args[0][0], sys.executable)
        self.assertLessEqual(run.call_args.kwargs['timeout'], resolver.PROBE_SECONDS)
        self.assertNotIn('/bin/sh', run.call_args.args[0])

    def test_old_or_hung_candidate_is_not_selected_and_each_probe_is_bounded(self):
        for first in (subprocess.CompletedProcess([], 1), subprocess.TimeoutExpired('owned probe', .01)):
            with self.subTest(first=first), tempfile.TemporaryDirectory() as directory:
                old, real = Path(directory) / 'old', Path(directory) / 'real with spaces'
                for path in (old, real):
                    path.touch()
                    path.chmod(0o700)
                with patch.object(resolver, 'candidates', return_value=iter(map(str, (old, real)))), \
                        patch.object(resolver.subprocess, 'run', side_effect=[first, subprocess.CompletedProcess([], 0)]) as run:
                    self.assertEqual(resolver.resolve(), str(real))
                self.assertEqual(run.call_count, 2)
                self.assertTrue(all(0 < call.kwargs['timeout'] <= resolver.PROBE_SECONDS for call in run.call_args_list))

    def test_explicit_override_is_literal_and_does_not_fall_back(self):
        with tempfile.TemporaryDirectory() as directory:
            chosen = Path(directory) / "chosen python's executable"
            chosen.touch()
            chosen.chmod(0o700)
            with patch.dict(os.environ, {'VQRO_PYTHON': str(chosen)}, clear=True), \
                    patch.object(resolver.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0)) as run:
                self.assertEqual(resolver.resolve(), str(chosen))
                self.assertEqual(run.call_args.args[0][0], str(chosen))
            with patch.dict(os.environ, {'VQRO_PYTHON': str(chosen)}, clear=True), \
                    patch.object(resolver.subprocess, 'run', return_value=subprocess.CompletedProcess([], 1)) as run:
                with self.assertRaisesRegex(RuntimeError, 'VQRO_PYTHON'):
                    resolver.resolve()
                self.assertEqual(run.call_count, 1)

    def test_resolution_has_an_overall_deadline(self):
        with patch.object(resolver, 'candidates', return_value=iter(['never probed'])), \
                patch.object(resolver.time, 'monotonic', side_effect=[0, resolver.RESOLVE_SECONDS + 1]), \
                patch.object(resolver.subprocess, 'run') as run:
            with self.assertRaisesRegex(RuntimeError, 'Python >= 3.11'):
                resolver.resolve()
            run.assert_not_called()


@unittest.skipUnless(os.name == 'posix', 'Unix shell bootstrap integration; pure controls remain portable')
class InterpreterTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="python-resolver-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.old = self.root / "old"
        self.real = self.root / "real interpreter's directory"
        self.old.mkdir()
        self.real.mkdir()
        (self.old / "python3").write_text("#!/bin/sh\nexit 1\n")
        (self.old / "python3").chmod(0o700)
        (self.real / "python3").symlink_to(sys.executable)
        self.env = {"PATH": os.pathsep.join(map(str, (self.old, self.real)))}

    def resolve(self, **env):
        return subprocess.run(["/bin/sh", str(RESOLVER)], env=self.env | env,
                              text=True, capture_output=True, timeout=20)

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

    def test_hung_probe_is_killed_and_reaped(self):
        shim = self.root / 'hung-python'
        pidfile = self.root / 'pid'
        shim.write_text('#!/bin/sh\nprintf "%s" "$$" > "' + str(pidfile) + '"\nexec /bin/sleep 60\n')
        shim.chmod(0o700)
        with patch.dict(os.environ, {'VQRO_PYTHON': str(shim)}, clear=True), \
                patch.object(resolver, 'PROBE_SECONDS', .05):
            with self.assertRaisesRegex(RuntimeError, 'VQRO_PYTHON'):
                resolver.resolve()
        with self.assertRaises(ProcessLookupError):
            os.kill(int(pidfile.read_text()), 0)
