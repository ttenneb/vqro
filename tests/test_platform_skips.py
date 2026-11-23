"""Linux-only harnesses skip before effects; portable/mocked tests remain enabled."""
import ast
import contextlib
import io
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

HERE = Path(__file__).resolve().parent
TOP_LEVEL = """e2e_tui_pane_basics e2e_tui_keys e2e_text_client e2e_providers
 e2e_rex e2e_same_setup e2e_opentui e2e_scale_herdr latency_typing
 e2e_tui_sidebar_tree e2e_tui_paste_dropdown e2e_tui_providers e2e_tui_pick_search
 e2e_item_streams tier1_smoke tier1_e2e claude_code_e2e claude_code_tui
 pi_pull_tui pi_pull_e2e""".split()
MAIN = """run_coord_conformance run_agent_conformance run_request_probes
 codex_launcher_e2e pi_provider_e2e e2e_agent_stop pi_bridge_smoke
 tmux_flood_load e2e_tui_scale e2e_testing_integration""".split()


class PlatformSkips(unittest.TestCase):
    def test_python_harness_guards_precede_effects_and_exit_77_with_reason(self):
        for name in TOP_LEVEL + MAIN:
            with self.subTest(harness=name):
                source = ast.parse((HERE / (name + '.py')).read_text())
                if name in MAIN:
                    body = next(node.body for node in source.body
                                if isinstance(node, ast.FunctionDef) and node.name == 'main')
                    guard = body[0]
                else:
                    body = source.body
                    guard = next(node for node in body if isinstance(node, ast.If))
                    for preceding in body[:body.index(guard)]:
                        self.assertTrue(isinstance(preceding, (ast.Expr, ast.Import)))
                        if isinstance(preceding, ast.Import):
                            self.assertEqual([item.name for item in preceding.names], ['sys'])
                self.assertIsInstance(guard, ast.If)
                program = compile(ast.Module(body=[guard], type_ignores=[]), name, 'exec')
                output = io.StringIO()
                with patch.object(sys, 'platform', 'darwin'), contextlib.redirect_stdout(output):
                    with self.assertRaises(SystemExit) as result:
                        exec(program, {'sys': sys})
                self.assertEqual(result.exception.code, 77)
                self.assertIn('SKIP', output.getvalue())
                self.assertIn('Linux', output.getvalue())

    def test_shell_harness_skips_before_arguments_or_private_roots(self):
        with tempfile.TemporaryDirectory(prefix='platform-skip-') as directory:
            root = Path(directory)
            uname = root / 'uname'
            uname.write_text("#!/bin/sh\nprintf '%s\\n' Darwin\n")
            uname.chmod(0o700)
            result = subprocess.run(['/bin/bash', str(HERE / 'e2e_tui_smoke.sh')],
                                    env={'PATH': str(root), 'HOME': str(root)},
                                    capture_output=True, text=True, timeout=5)
            self.assertEqual(result.returncode, 77, result.stderr)
            self.assertIn('SKIP TUI smoke', result.stdout)
            self.assertIn('Linux /proc', result.stdout)
