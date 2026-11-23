"""Portable pure resolver controls plus Unix bootstrap integration controls."""
import ast
import importlib.util
import json
import os
from pathlib import Path
import runpy
import shlex
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

    def test_shell_fixture_preserves_literal_interpreter_and_source(self):
        from python_interpreter import shell_fixture
        interpreter = "C:/test python's directory/python.exe"
        source = 'import sys; print("literal \\\" quoted source")'
        script = shell_fixture(source, interpreter)
        command = script.removeprefix('#!/bin/sh\nexec ').removesuffix(' "$@"\n')
        self.assertEqual(shlex.split(command), [interpreter, '-c', source])
        self.assertTrue(script.startswith('#!/bin/sh\n'))

    def test_actual_harness_shell_expressions_preserve_literal_paths(self):
        chosen, root = "/test python's/bin/python3", "/private root's directory"
        namespace = {'python': lambda: chosen, 'R': root, 'shlex': shlex}
        for name, marker, expected in (
                ('e2e_opentui.py', 'mouse.py', [chosen, root + '/mouse.py']),
                ('e2e_tui_providers.py', 'icons.py', [chosen, root + '/ext/icons.py'])):
            tree = ast.parse((ROOT / 'tests' / name).read_text())
            expressions = [node.args[0] for node in ast.walk(tree) if isinstance(node, ast.Call) and node.args
                           and marker in ast.unparse(node.args[0])
                           and ((isinstance(node.func, ast.Name) and node.func.id == 'keys')
                                or (isinstance(node.func, ast.Attribute) and node.func.attr == 'write'))]
            self.assertEqual(len(expressions), 1)
            command = eval(compile(ast.Expression(expressions[0]), name, 'eval'), namespace)
            command = command.removeprefix('/usr/bin/clear; ').removeprefix('#!/bin/sh\nexec ')
            self.assertEqual(shlex.split(command), expected)

    def test_conformance_registry_imports_do_not_probe_interpreters(self):
        path = ROOT / 'tests/agent_conformance/core.py'
        spec = importlib.util.spec_from_file_location('agent_conformance._import_control', path)
        module = importlib.util.module_from_spec(spec)
        with patch.object(subprocess, 'Popen', side_effect=AssertionError('registry import launched a child')):
            spec.loader.exec_module(module)
        self.assertTrue(hasattr(module, 'PublicImplementation'))

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

    def test_shell_fixture_executes_with_spaced_apostrophe_interpreter_and_literal_args(self):
        from python_interpreter import shell_fixture
        fixture = self.root / "fixture's wrapper"
        fixture.write_text(shell_fixture('import json,sys; print(json.dumps(sys.argv[1:]))', str(self.real / 'python3')))
        fixture.chmod(0o700)
        arguments = ['one spaced argument', 'apostrophe\' and "quote"', 'snowman ☃']
        result = subprocess.run([str(fixture), *arguments], env=self.env, text=True, capture_output=True, timeout=5)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout), arguments)

    def test_hung_probe_is_killed_and_reaped(self):
        shim = self.root / 'hung-python'
        shim.write_text('#!/bin/sh\nexec /bin/sleep 60\n')
        shim.chmod(0o700)
        children = []
        real_popen = subprocess.Popen
        def owned_child(*args, **kwargs):
            child = real_popen(*args, **kwargs)
            children.append(child)
            return child
        with patch.dict(os.environ, {'VQRO_PYTHON': str(shim)}, clear=True), \
                patch.object(resolver, 'PROBE_SECONDS', .05), \
                patch.object(resolver.subprocess, 'Popen', side_effect=owned_child):
            with self.assertRaisesRegex(RuntimeError, 'VQRO_PYTHON'):
                resolver.resolve()
        self.assertEqual(len(children), 1)
        self.assertIsNotNone(children[0].returncode)
        with self.assertRaises(ChildProcessError):
            os.waitpid(children[0].pid, os.WNOHANG)
