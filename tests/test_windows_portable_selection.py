"""Pin-6 coverage controls: portable contracts must not inherit Unix fixture skips."""
import ast
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

import test_testing_unit as health_fixture

ROOT = Path(__file__).resolve().parent
PORTABLE = {
    "test_agent_conformance.py": {"PeerTests": ["test_cancelled_close_still_aborts_transport_and_joins_reader"]},
    "test_vqro_ext_slow_core.py": {"SlowCore": ["test_normal_interpreter_exit_with_a_lifetime_pipe_is_not_a_fatal_error"]},
    "test_coord_harness.py": {"SilentPeerTests": ["test_silent_peer_receive_uses_the_remaining_call_deadline"]},
    "test_testing_integration.py": {
        "SandboxTests": ["test_apply_fills_compare_and_set", "test_process_facts_exclude_other_groups_and_zombies"],
        "IntegrationModesTests": ["test_fake_backend_dependency_gap_is_listed_as_not_covered",
                                  "test_scenarios_are_not_invoked_after_failed_restart_readiness",
                                  "test_async_extension_returns_before_waiting_then_publishes_completion"],
    },
    "test_agent_conformance_codex_l3.py": {"WholeHandlerControls": None},
}
UNIX = {
    "test_agent_conformance.py": ("PeerTests", "test_stalled_socket_close_is_bounded_and_joins_reader"),
    "test_vqro_ext_slow_core.py": ("SlowCore", "test_startup_waits_out_a_slow_core"),
    "test_coord_harness.py": ("SilentPeerTests", "test_an_immediate_reply_is_accepted"),
    "test_testing_integration.py": ("SandboxTests", "test_copy_rebases_launch_paths_and_scrubs_environment"),
}


def classes(filename):
    return {node.name: node for node in ast.parse((ROOT / filename).read_text(encoding="utf-8")).body
            if isinstance(node, ast.ClassDef)}


def methods(node):
    return {method.name: method for method in node.body
            if isinstance(method, (ast.FunctionDef, ast.AsyncFunctionDef))}


def skips(node):
    return [decorator for decorator in node.decorator_list
            if isinstance(decorator, ast.Call) and isinstance(decorator.func, ast.Attribute)
            and decorator.func.attr.startswith("skip")]


class PortableSelection(unittest.TestCase):
    def test_portable_methods_have_no_method_or_inherited_class_skip(self):
        for filename, selected in PORTABLE.items():
            for name, requested in selected.items():
                cls = classes(filename)[name]
                with self.subTest(file=filename, cls=name):
                    self.assertEqual(skips(cls), [], "portable controls must not inherit fixture-wide skips")
                members = methods(cls)
                for method in requested or [name for name in members if name.startswith("test_")]:
                    with self.subTest(file=filename, method=method):
                        self.assertEqual(skips(members[method]), [])

    def test_real_unix_fixture_methods_keep_explicit_skips(self):
        for filename, (cls, method) in UNIX.items():
            with self.subTest(file=filename):
                self.assertTrue(skips(methods(classes(filename)[cls])[method]))

    def test_cli_content_check_runs_without_a_windows_mode_claim(self):
        fixture = health_fixture.HealthChecks("test_cli_offline_json_and_out")
        fixture.setUp()
        try:
            with patch.object(sys, "platform", "win32"), \
                    patch.object(health_fixture.unit, "resolve_program", return_value=Path(sys.executable)), \
                    patch.object(health_fixture.stat, "S_IMODE", return_value=0o666) as mode:
                fixture.test_cli_offline_json_and_out()
                mode.assert_not_called()
        finally:
            fixture.doCleanups()
