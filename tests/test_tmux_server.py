"""Tmux management is policy over public primitives, never provider RPC."""
import importlib.util
from pathlib import Path
import subprocess
import unittest
from unittest.mock import patch, Mock

SPEC = importlib.util.spec_from_file_location("tmux_server_ext", Path(__file__).resolve().parents[1] / "extensions/tmux_server.py")
m = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(m)


class Ext:
    def __init__(self):
        self.info = {"state_dir": "/private/state"}
        self.commands = {}
        self.entries = {
            "doc/scalar": {"value": "not an extension"},
            "extension/tmux": {"version": 4, "value": {"enabled": True, "argv": ["/bin/prim-tmux", "--name", "tmux"]}},
            "extension/other": {"version": 2, "value": {"enabled": True, "argv": ["prim-tmux", "--tmux-socket", "/other"]}},
        }
        self.contributions = []
        self.published = []
        self.applied = []
        self.notifications = []
    def command(self, name, title):
        def decorate(fn):
            self.commands[name] = fn
            return fn
        return decorate
    def contribute(self, entries): self.contributions.append(entries)
    def publish(self, view, content): self.published.append((view, content))
    def notify(self, *args): self.notifications.append(args)
    def fresh(self): pass
    def apply(self, ops, note): self.applied.append(ops)


class Server(m.TmuxServer):
    def __init__(self, ext):
        super().__init__(ext)
        self.current = {"running": True, "instance": "old-instance", "terminals": 3}
        self.calls = []
    def call(self, action, *args):
        self.calls.append((action, args))
        if action == "status": return dict(self.current)
        self.current = {"running": False, "terminals": 0}
        return {"outcome": "accepted"}


class TmuxServerTests(unittest.TestCase):
    def setUp(self):
        self.ext = Ext()
        self.server = Server(self.ext)

    def test_absent_has_commands_but_no_menu_and_never_submits_stop(self):
        self.server.current = {"running": False, "terminals": 0}
        self.assertEqual(set(self.ext.commands), {"tmux.server.status", "tmux.server.stop"})
        self.assertFalse(self.server.status({})["running"])
        self.assertEqual(self.server.stop({"text": "STOP 0"})["outcome"], "not_submitted")
        self.assertNotIn("menu/tmux-server", self.ext.contributions[-1])
        self.assertFalse(any(action == "stop" for action, _ in self.server.calls))

    def test_confirmation_uses_count_and_instance_with_generic_input(self):
        reply = self.server.stop({})
        self.assertEqual(reply["client"]["popup"]["view"], "tmux-server")
        row = self.ext.published[-1][1]["items"][-1]
        self.assertIn("3 terminals", row["input"])
        self.assertIn("STOP 3", row["input"])
        self.assertEqual(row["args"], {"instance": "old-instance", "terminals": 3})
        self.assertFalse(self.ext.applied)

    def test_wrong_text_count_or_instance_rejects_before_mutation(self):
        for args in [{"text": "yes", "instance": "old-instance", "terminals": 3},
                     {"text": "STOP 3", "instance": "replacement", "terminals": 3},
                     {"text": "STOP 4", "instance": "old-instance", "terminals": 4}]:
            with self.assertRaises(m.PrimError): self.server.stop(args)
        self.assertFalse(self.ext.applied)
        self.assertFalse(any(action == "stop" for action, _ in self.server.calls))

    def test_confirmed_stop_disables_only_owned_provider_and_calls_helper_once(self):
        self.assertEqual(self.server.stop({"text": "STOP 3", "instance": "old-instance", "terminals": 3})["outcome"], "accepted")
        self.assertEqual(len(self.ext.applied), 1)
        self.assertEqual([op["key"] for op in self.ext.applied[0]], ["extension/tmux"])
        self.assertFalse(self.ext.applied[0][0]["value"]["enabled"])
        self.assertEqual([c for c in self.server.calls if c[0] == "stop"], [
            ("stop", ("--expect-instance", "old-instance", "--confirm-terminals", "3"))])

    def test_stop_works_with_provider_already_disabled(self):
        self.ext.entries["extension/tmux"]["value"]["enabled"] = False
        self.server.stop({"text": "STOP 3", "instance": "old-instance", "terminals": 3})
        self.assertFalse(self.ext.applied)
        self.assertTrue(any(action == "stop" for action, _ in self.server.calls))

    def test_menu_changes_only_on_server_existence_counts_are_live(self):
        self.server.refresh()
        self.server.current["terminals"] = 4
        self.server.refresh()
        self.assertEqual(len(self.ext.contributions), 1)
        self.assertEqual(len(self.ext.published), 2)
        self.server.current = {"running": False, "terminals": 0}
        self.server.refresh()
        self.assertNotIn("menu/tmux-server", self.ext.contributions[-1])

    def test_stop_timeout_kills_only_own_helper_group_and_is_indeterminate(self):
        server = m.TmuxServer(self.ext)
        child = Mock(pid=123)
        child.communicate.side_effect = [subprocess.TimeoutExpired("helper", 4), (b"", b"")]
        with patch.object(m.subprocess, "Popen", return_value=child), patch.object(m.os, "killpg") as kill:
            with self.assertRaises(m.PrimError) as error: server.call("stop")
        self.assertEqual(error.exception.kind, "indeterminate")
        kill.assert_called_once_with(123, m.signal.SIGKILL)
        self.assertEqual(child.communicate.call_count, 2)


if __name__ == "__main__": unittest.main()
