"""Powerless controls for native-smoke cleanup; never run the Windows fixture here."""
import ast
from pathlib import Path
import unittest
from unittest import mock
from types import SimpleNamespace

import windows_background_smoke as smoke


class FixtureCleanupTests(unittest.TestCase):
    def api(self, statuses):
        return SimpleNamespace(WAIT_OBJECT_0=0, WaitForSingleObject=mock.Mock(side_effect=statuses),
                               CloseHandle=mock.Mock(), OpenProcess=mock.Mock(), TerminateProcess=mock.Mock())

    def test_exited_fixture_handles_are_only_waited_and_closed(self):
        api = self.api([0, 0])
        smoke.close_fixture_handles({11: "first", 22: "second"}, api)
        self.assertEqual(api.WaitForSingleObject.call_args_list, [mock.call("first", 5000), mock.call("second", 5000)])
        self.assertEqual(api.CloseHandle.call_args_list, [mock.call("first"), mock.call("second")])
        api.OpenProcess.assert_not_called()
        api.TerminateProcess.assert_not_called()

    def test_timeout_and_wait_failure_close_every_handle_and_fail_visibly(self):
        api = self.api([258, OSError("wait failed"), 0])
        with self.assertRaisesRegex(RuntimeError, "cleanup unconfirmed.*11.*22"):
            smoke.close_fixture_handles({11: "first", 22: "second", 33: "third"}, api)
        self.assertEqual(api.CloseHandle.call_args_list, [mock.call("first"), mock.call("second"), mock.call("third")])
        api.OpenProcess.assert_not_called()
        api.TerminateProcess.assert_not_called()

    def test_late_reports_are_reported_not_opened_or_terminated(self):
        api = self.api([0])
        with self.assertRaisesRegex(RuntimeError, "cleanup unconfirmed.*unobserved report native-late.json"):
            smoke.close_fixture_handles({11: "first"}, api, unobserved=["native-late.json"])
        api.CloseHandle.assert_called_once_with("first")
        api.OpenProcess.assert_not_called()
        api.TerminateProcess.assert_not_called()

    def test_source_has_no_fixture_termination_or_termination_access(self):
        tree = ast.parse(Path(smoke.__file__).read_text(encoding="utf-8"))
        calls = [node for node in ast.walk(tree) if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)]
        self.assertFalse([node for node in calls if node.func.attr == "TerminateProcess"])
        opened = [node for node in calls if node.func.attr == "OpenProcess"]
        self.assertEqual(len(opened), 1)
        self.assertEqual(ast.unparse(opened[0].args[0]), "_winapi.SYNCHRONIZE")
        killed = [node for node in calls if node.func.attr == "kill"]
        self.assertEqual([ast.unparse(node.func.value) for node in killed], ["core"])
        collect_calls = [node for node in ast.walk(tree) if isinstance(node, ast.Call)
                         and isinstance(node.func, ast.Name) and node.func.id == "collect"]
        self.assertEqual(len(collect_calls), 1)  # only the live observation/restart path
        self.assertFalse(collect_calls[0].keywords)


if __name__ == "__main__":
    unittest.main()
