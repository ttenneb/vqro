"""Local stdio helpers must stay console-less at every subprocess boundary."""
import ast
import json
from pathlib import Path
import subprocess
import sys
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "extensions"))
from vqro_process import background_kwargs


class BackgroundProcessTests(unittest.TestCase):
    def test_unix_has_no_extra_process_options(self):
        with mock.patch("vqro_process.os.name", "posix"):
            self.assertEqual(background_kwargs(), {})

    def test_windows_uses_no_window_not_detach_or_new_console(self):
        # A sentinel permits this branch test on Unix without inventing a native flag value.
        with mock.patch("vqro_process.os.name", "nt"), mock.patch.object(
                subprocess, "CREATE_NO_WINDOW", 1234, create=True):
            self.assertEqual(background_kwargs(), {"creationflags": 1234})

    def test_stdio_still_works_and_windows_child_has_no_console(self):
        code = (
            "import ctypes,json,os,sys; "
            "console=ctypes.windll.kernel32.GetConsoleWindow if os.name=='nt' else lambda:0; "
            "console.restype=ctypes.c_void_p; "
            "print(json.dumps({'console':console() or 0,'input':sys.stdin.readline().strip()})); "
            "print('stderr still works',file=sys.stderr)"
        )
        result = subprocess.run([sys.executable, "-c", code], input="hello\n",
                                capture_output=True, text=True, timeout=15,
                                check=True, **background_kwargs())
        self.assertEqual(json.loads(result.stdout), {"console": 0, "input": "hello"})
        self.assertEqual(result.stderr.strip(), "stderr still works")

    def test_background_extension_calls_explicitly_suppress_descendant_consoles(self):
        # CREATE_NO_WINDOW isn't inherited. Guard the actual default-extension and diagnostics
        # call sites, not just the helper. Intentional links/UI/terminal launches are excluded.
        for name in ("workspaces", "tier1", "testing_unit", "testing_integration",
                     "testing_perf", "vqro_testing"):
            tree = ast.parse((ROOT / "extensions" / (name + ".py")).read_text(encoding="utf-8"))
            calls = [node for node in ast.walk(tree) if isinstance(node, ast.Call)
                     and isinstance(node.func, ast.Attribute)
                     and isinstance(node.func.value, ast.Name)
                     and node.func.value.id == "subprocess"
                     and node.func.attr in ("run", "Popen")]
            self.assertTrue(calls, name)
            for call in calls:
                with self.subTest(module=name, line=call.lineno):
                    self.assertTrue(any(key.arg is None and isinstance(key.value, ast.Call)
                                        and isinstance(key.value.func, ast.Name)
                                        and key.value.func.id == "background_kwargs"
                                        for key in call.keywords))


if __name__ == "__main__":
    unittest.main()
