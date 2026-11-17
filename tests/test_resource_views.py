"""Kind routing and ended-tab cleanup, using only the public extension surface."""
from pathlib import Path
import runpy
import sys
import types
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
import prim_ext
from prim_ext import PrimError, first_tabs
from prim_views import terminal_target
from agent_fixture import FakeCore


class Core(FakeCore):
    def active_layout(self):
        value = self.value("layout/main")
        return ("layout/main", self.version("layout/main"), value) if value else None

    def run(self):
        pass

    def on_event(self, fn):
        self.listener = fn
        return fn


def load(name):
    core = Core(name)
    with mock.patch.dict(sys.modules, {"prim_ext": types.SimpleNamespace(**dict(vars(prim_ext), Extension=lambda *a, **k: core))}):
        module = runpy.run_path(str(Path(__file__).resolve().parents[1] / "extensions" / (name+".py")))
    core.resources = {"r1": {"id": "r1", "kind": "terminal", "name": "Bridge", "state": "live"}}
    core.add_agent()
    return module, core


class ResourceViews(unittest.TestCase):
    def test_tabs_resource_and_picker_never_create_a_terminal_view_of_an_agent(self):
        module, core = load("tabs")
        for args in ({"resource": "r2"}, {"picked": "r2"}):
            module["show_cmd"](args)
            self.assertEqual(core.calls[-1], ("agents.show", {"resource": "r2"}))
        self.assertIsNone(core.value("view/term-r2"))
        core.fail_agents = True
        module["show_cmd"]({"resource": "r2"})
        self.assertEqual(core.value("view/term-r1")["resource"], "r1")
        self.assertIsNone(core.value("view/term-r2"))
        self.assertEqual(first_tabs(core.value("layout/main"))["tabs"][0]["title"], "Other agent r2 bridge")

    def test_failed_agent_show_does_not_replay_as_terminal_navigation(self):
        _, core = load("tabs")
        with mock.patch.object(core, "invoke", side_effect=PrimError("unknown layout outcome", "timeout")):
            with self.assertRaises(PrimError):
                terminal_target(core, "r2")
        self.assertIsNone(core.value("layout/main"))
        core.resources["r3"] = {"id": "r3", "kind": "opaque"}
        with self.assertRaises(PrimError):
            terminal_target(core, "r3")

    def test_pane_picker_routes_agent_but_only_rebinds_terminal_to_terminal(self):
        module, core = load("panes")
        core.apply([{"op": "set", "key": "view/term-r1", "value": {"kind": "terminal", "resource": "r1"}}], "fixture")
        args = {"context": {"view": "term-r1"}, "picked": "r2"}
        module["show"](args)
        self.assertEqual(core.calls[-1], ("agents.show", {"resource": "r2"}))
        self.assertEqual(core.value("view/term-r1")["resource"], "r1")
        core.fail_agents = True
        module["show"](args)
        self.assertEqual(core.value("view/term-r1")["resource"], "r1")

    def test_focuses_existing_nested_agent_tab_without_duplicates(self):
        module, core = load("tabs")
        root = {"tabs": [{"title": "nested", "node": {"split": "h", "children": [{"view": "sidebar"}, {"view": "agents-r2"}]}},
                         {"title": "shell", "node": {"view": "term-r1"}}], "active": 1}
        core.apply([{"op": "set", "key": "layout/main", "value": root}], "fixture")
        module["show"]("agents-r2", "Other")
        result = core.value("layout/main")
        self.assertEqual(len(result["tabs"]), 2)
        self.assertEqual(result["active"], 0)

    def test_close_ended_preserves_live_mixed_and_last_tabs(self):
        module, core = load("tabs")
        core.resources["r3"] = {"id": "r3", "kind": "terminal", "state": "finished"}
        core.apply([{"op": "set", "key": "view/term-r3", "value": {"kind": "terminal", "resource": "r3"}}], "fixture")
        root = {"tabs": [{"title": "gone", "node": {"view": "agent-r99"}},
                         {"title": "ended", "node": {"view": "term-r3"}},
                         {"title": "mixed", "node": {"split": "h", "children": [{"view": "term-r3"}, {"view": "agents-r2"}]}}], "active": 1}
        core.apply([{"op": "set", "key": "layout/main", "value": root}], "fixture")
        module["close_ended"]({})
        self.assertEqual([t["title"] for t in core.value("layout/main")["tabs"]], ["mixed"])
        core.resources.clear()
        module["close_ended"]({})
        self.assertEqual(len(core.value("layout/main")["tabs"]), 1)
        self.assertFalse(any(method == "resource.kill" for method, _ in core.calls))


if __name__ == "__main__":
    unittest.main()
