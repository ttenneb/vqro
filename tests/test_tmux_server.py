"""Tmux management is policy over public primitives, never provider RPC."""
import importlib.util
import json
import tempfile
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
            "extension/tmux": {"version": 4, "value": {"enabled": True, "argv": ["/bin/vqro-tmux", "--name", "tmux"]}},
            "extension/other": {"version": 2, "value": {"enabled": True, "argv": ["vqro-tmux", "--tmux-socket", "/other"]}},
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
    def run(self): pass
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
            with self.assertRaises(m.VqroError): self.server.stop(args)
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

    def test_main_never_spawns_helper_over_idle_ticks_without_socket(self):
        with tempfile.TemporaryDirectory() as root:
            self.ext.info["state_dir"] = root
            child = Mock(returncode=0)
            child.communicate.return_value = (b'{"running":false,"terminals":0}', b'')
            with patch.object(m, "Extension", return_value=self.ext), \
                    patch.object(m.subprocess, "Popen", return_value=child) as spawn, \
                    patch.object(m.time, "sleep", side_effect=[None]*7 + [StopIteration]) as sleep:
                with self.assertRaises(StopIteration): m.main()
            self.assertEqual(sleep.call_count, 8)
            spawn.assert_not_called()
            self.assertTrue(all(call.args == (5,) for call in sleep.call_args_list))
            self.assertNotIn("menu/tmux-server", self.ext.contributions[-1])
            self.assertEqual(set(self.ext.commands), {"tmux.server.status", "tmux.server.stop"})

    def test_poll_discovers_server_and_removes_menu_within_one_interval(self):
        with tempfile.TemporaryDirectory() as root:
            self.ext.info["state_dir"] = root
            socket = Path(root) / "tmux.sock"
            ticks = []
            def reply(*args, **kwargs):
                observed = {"running": True, "instance": "native", "terminals": 2} if socket.exists() else {"running": False}
                child = Mock(returncode=0)
                child.communicate.return_value = (json.dumps(observed).encode(), b'')
                return child
            def tick(seconds):
                ticks.append(seconds)
                self.assertEqual(seconds, 5)
                if len(ticks) == 1:
                    self.assertNotIn("menu/tmux-server", self.ext.contributions[-1])
                    socket.touch()
                elif len(ticks) == 2:
                    self.assertIn("menu/tmux-server", self.ext.contributions[-1])
                    socket.unlink()
                else:
                    self.assertNotIn("menu/tmux-server", self.ext.contributions[-1])
                    raise StopIteration
            with patch.object(m, "Extension", return_value=self.ext), \
                    patch.object(m.subprocess, "Popen", side_effect=reply) as spawn, \
                    patch.object(m.time, "sleep", side_effect=tick):
                with self.assertRaises(StopIteration): m.main()
            self.assertEqual(spawn.call_count, 1)

    def test_stale_socket_negative_cache_invalidates_on_replacement_or_explicit_status(self):
        with tempfile.TemporaryDirectory() as root:
            self.ext.info["state_dir"] = root
            server = Server(self.ext)
            socket = Path(root) / "tmux.sock"
            socket.touch()
            server.current = {"running": False, "terminals": 0}
            for _ in range(12): server.poll()
            self.assertEqual(len(server.calls), 2, "confirm stale endpoint, then stat only")
            socket.unlink()
            socket.touch()
            server.current = {"running": True, "terminals": 3, "instance": "new"}
            server.poll()
            self.assertIn("menu/tmux-server", self.ext.contributions[-1])
            server.current = {"running": False, "terminals": 0}
            for _ in range(3): server.poll()
            before = len(server.calls)
            server.current = {"running": True, "terminals": 3, "instance": "new"}
            server.status({})  # explicit commands never use a negative cache
            server.poll()
            self.assertEqual(len(server.calls), before + 2)
            self.assertIn("menu/tmux-server", self.ext.contributions[-1])

    def test_non_posix_timeout_kills_helper_without_posix_session_or_group(self):
        for action, kind in [("stop", "indeterminate"), ("status", "timeout")]:
            with self.subTest(action=action):
                server = m.TmuxServer(self.ext)
                child = Mock(pid=123)
                child.communicate.side_effect = [subprocess.TimeoutExpired("helper", 4), (b"", b"")]
                with patch.object(m.os, "name", "nt"), \
                        patch.object(m.os, "killpg", create=True) as kill_group, \
                        patch.object(m.subprocess, "Popen", return_value=child) as spawn:
                    with self.assertRaises(m.VqroError) as error: server.call(action)
                self.assertEqual(error.exception.kind, kind)
                self.assertNotIn("start_new_session", spawn.call_args.kwargs)
                child.kill.assert_called_once_with()
                kill_group.assert_not_called()
                self.assertEqual(spawn.call_count, 1, "never retry after timeout")
                self.assertEqual(child.communicate.call_count, 2)

    def test_timeout_falls_back_when_group_primitive_is_unavailable(self):
        server = m.TmuxServer(self.ext)
        child = Mock(pid=123)
        child.communicate.side_effect = [subprocess.TimeoutExpired("helper", 4), (b"", b"")]
        with patch.object(m.os, "killpg", None, create=True), \
                patch.object(m.subprocess, "Popen", return_value=child) as spawn:
            with self.assertRaises(m.VqroError) as error: server.call("stop")
        self.assertEqual(error.exception.kind, "indeterminate")
        self.assertNotIn("start_new_session", spawn.call_args.kwargs)
        child.kill.assert_called_once_with()

    @unittest.skipUnless(m.os.name == "posix" and callable(getattr(m.os, "killpg", None)) and hasattr(m.signal, "SIGKILL"),
                         "requires POSIX process groups")
    def test_stop_timeout_kills_only_own_helper_group_and_is_indeterminate(self):
        server = m.TmuxServer(self.ext)
        child = Mock(pid=123)
        child.communicate.side_effect = [subprocess.TimeoutExpired("helper", 4), (b"", b"")]
        with patch.object(m.subprocess, "Popen", return_value=child), patch.object(m.os, "killpg") as kill:
            with self.assertRaises(m.VqroError) as error: server.call("stop")
        self.assertEqual(error.exception.kind, "indeterminate")
        kill.assert_called_once_with(123, m.signal.SIGKILL)
        self.assertEqual(child.communicate.call_count, 2)


if __name__ == "__main__": unittest.main()
