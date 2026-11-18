"""Retention policy using only a fake public extension surface; no threads or sockets."""
from pathlib import Path
import runpy
import sys
import threading
import types
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
import prim_ext
from agent_fixture import FakeCore


class RetentionTests(unittest.TestCase):
    def setUp(self):
        self.core = FakeCore("retention")
        self.core.cache_lock = threading.RLock()
        self.core.run = lambda: None
        replacement = types.SimpleNamespace(Extension=lambda *a, **k: self.core,
                                           PrimError=prim_ext.PrimError, views_in=prim_ext.views_in)
        with mock.patch.dict(sys.modules, {"prim_ext": replacement}), mock.patch("threading.Thread"):
            self.module = runpy.run_path(str(Path(__file__).resolve().parents[1] / "extensions/retention.py"))

    def plan(self, now, **kwargs):
        with mock.patch.object(self.module["time"], "monotonic", return_value=now):
            return self.module["plan"](**kwargs)

    def test_unavailable_live_agent_expires_after_continuous_grace_even_if_shown(self):
        self.core.resources["r1"] = {"id": "r1", "kind": "agent", "state": "live", "available": False}
        self.core.entries["layout/main"] = {"version": 1, "value": {"tabs": [{"node": {"view": "agents-r1"}}]}}
        self.assertEqual(self.plan(0), ([], 60))
        self.assertEqual(self.plan(59), ([], 1))
        self.assertEqual(self.plan(60)[0], ["r1"])

    def test_reconnect_cancels_old_grace_and_live_terminals_are_not_expired(self):
        self.core.resources["r1"] = {"id": "r1", "kind": "agent", "state": "live", "available": False}
        self.core.resources["r2"] = {"id": "r2", "kind": "terminal", "state": "live", "available": False}
        self.plan(0)
        self.core.resources["r1"]["available"] = True
        self.assertEqual(self.plan(30)[0], [])
        self.core.resources["r1"]["available"] = False
        self.plan(40)
        self.assertEqual(self.plan(99)[0], [])
        self.assertEqual(self.plan(100)[0], ["r1"])

    def test_explicit_finished_only_command_does_not_bypass_agent_grace(self):
        self.core.resources["r1"] = {"id": "r1", "kind": "agent", "state": "live", "available": False}
        self.assertEqual(self.module["forget_ended"]({}), {"forgotten": 0})
        self.assertEqual(self.core.calls, [])

    def test_reconnect_race_never_promotes_cached_candidate_into_native_forget(self):
        self.core.resources["r1"] = {"id": "r1", "kind": "agent", "state": "live", "available": False}
        self.plan(0)
        cached = set()
        self.assertEqual(self.plan(60, cached_due=cached)[0], ["r1"])
        self.core.resources["r1"].update(available=True, state="finished")
        call = mock.Mock(return_value={"outcome": "observed"})
        self.core.prim = types.SimpleNamespace(call=call)
        self.module["forget"]("r1", cached="r1" in cached)
        call.assert_called_once_with("resource.forget_cached", {"resource": "r1"})

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
