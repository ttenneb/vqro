"""Retention policy using only a fake public extension surface; no threads or sockets."""
from pathlib import Path
import os
import runpy
import sys
import threading
import types
import unittest
from unittest import mock

EXT = Path(os.environ.get("VQRO_TEST_EXTENSIONS_DIR", Path(__file__).resolve().parents[1] / "extensions"))
sys.path.insert(0, str(EXT))
import vqro_ext
from agent_fixture import FakeCore


class RetentionTests(unittest.TestCase):
    def setUp(self):
        self.core = FakeCore("retention")
        self.core.cache_lock = threading.RLock()
        self.core.run = lambda: None
        replacement = types.SimpleNamespace(Extension=lambda *a, **k: self.core,
                                           VqroError=vqro_ext.VqroError, views_in=vqro_ext.views_in)
        with mock.patch.dict(sys.modules, {"vqro_ext": replacement}), mock.patch("threading.Thread"):
            self.module = runpy.run_path(str(EXT / "retention.py"))

    def plan(self, now, **kwargs):
        with mock.patch.object(self.module["time"], "monotonic", return_value=now):
            return self.module["plan"](**kwargs)

    def test_unavailable_agents_and_terminals_are_never_forgotten_by_time(self):
        # Host live (bridge may reconnect), host never reported in this run (inventory not synced after a
        # restart), no host at all, shown or not: an unexplained disconnect stays visible as unknown.
        self.core.resources["r1"] = {"id": "r1", "kind": "terminal", "state": "live", "available": False}
        self.core.resources["r2"] = {"id": "r2", "kind": "agent", "state": "live", "available": False, "host": "r1"}
        self.core.resources["r4"] = {"id": "r4", "kind": "agent", "state": "live", "available": False, "host": "r3"}
        self.core.resources["r5"] = {"id": "r5", "kind": "agent", "state": "live", "available": False}
        self.core.entries["layout/main"] = {"version": 1, "value": {"tabs": [{"node": {"view": "agents-r5"}}]}}
        for now in (0, 60, 3600, 10**6):
            self.assertEqual(self.plan(now), ([], None))

    def test_explicit_finished_only_command_never_forgets_agents(self):
        self.core.resources["r1"] = {"id": "r1", "kind": "agent", "state": "live", "available": False}
        self.assertEqual(self.module["forget_ended"]({}), {"forgotten": 0})
        self.assertEqual(self.core.calls, [])

    def test_reconnect_race_never_promotes_cached_candidate_into_native_forget(self):
        self.core.resources["r9"] = {"id": "r9", "kind": "terminal", "state": "finished", "available": True}
        self.core.resources["r1"] = {"id": "r1", "kind": "agent", "state": "live", "available": False, "host": "r9"}
        cached = set()
        self.assertEqual(self.plan(0, cached_due=cached)[0], ["r1"])
        self.core.resources["r1"].update(available=True, state="finished")
        call = mock.Mock(return_value={"outcome": "observed"})
        self.core.vqro = types.SimpleNamespace(call=call)
        self.module["forget"]("r1", cached="r1" in cached)
        call.assert_called_once_with("resource.forget_cached", {"resource": "r1"})

    def test_agent_with_finished_or_removed_host_is_forgotten_at_once(self):
        self.core.resources["r1"] = {"id": "r1", "kind": "terminal", "state": "live", "available": True}
        self.core.resources["r2"] = {"id": "r2", "kind": "agent", "state": "live", "available": True, "host": "r1"}
        self.core.resources["r3"] = {"id": "r3", "kind": "terminal", "state": "live", "available": True}
        self.core.resources["r4"] = {"id": "r4", "kind": "agent", "state": "live", "available": True, "host": "r3"}
        self.plan(0)
        self.core.resources["r2"]["available"] = self.core.resources["r4"]["available"] = False
        self.core.resources["r1"]["state"] = "finished"  # Stop agent: the bridge terminal ended
        del self.core.resources["r3"]                     # seen earlier, now removed (e.g. tmux pane gone)
        cached = set()
        ids, _ = self.plan(1, cached_due=cached)
        self.assertEqual(sorted(ids), ["r2", "r4"])
        self.assertEqual(cached, {"r2", "r4"}, "retired as cached facts, never a native forget")

    def test_bridge_cleanup_preserves_live_referenced_and_user_views(self):
        for i in range(1, 5):
            self.core.entries[f"view/agent-r{i}"] = {"version": 1, "value": {"kind": "panel", "bridge": {"instance": "i", "ref": "agent"}}}
        self.core.entries["view/agent-status-r1"] = {"version": 1, "value": {"kind": "panel", "bridge": {"instance": "i", "ref": "agent"}}}
        self.core.entries["view/agent-status-r2"] = {"version": 1, "value": {"kind": "panel", "bridge": {"instance": "i", "ref": "agent"}}}
        self.core.resources["r2"] = {"id": "r2", "kind": "agent"}
        self.core.entries["layout/main"] = {"version": 1, "value": {"tabs": [{"node": {"view": "agent-r3"}}]}}
        del self.core.entries["view/agent-r4"]["value"]["bridge"]
        self.module["prune_bridge_views"]()
        self.assertNotIn("view/agent-r1", self.core.entries)
        self.assertNotIn("view/agent-status-r1", self.core.entries)
        self.assertIn("view/agent-status-r2", self.core.entries)
        for i in (2, 3, 4):
            self.assertIn(f"view/agent-r{i}", self.core.entries)


if __name__ == "__main__":
    unittest.main()
