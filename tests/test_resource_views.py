"""Kind routing and ended-tab cleanup, using only the public extension surface."""
import copy
from pathlib import Path
import runpy
import sys
import types
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
import vqro_ext
from vqro_ext import VqroError, first_tabs
from vqro_views import terminal_target
from agent_fixture import FakeCore


class Core(FakeCore):
    def active_layout(self):
        value = self.value("layout/main")
        return ("layout/main", self.version("layout/main"), copy.deepcopy(value)) if value else None

    def retrying(self, fn, attempts=3):
        return vqro_ext.Extension.retrying(self, fn, attempts)

    def run(self):
        pass

    def on_event(self, fn):
        self.listener = fn
        return fn


def load(name):
    core = Core(name)
    with mock.patch.dict(sys.modules, {"vqro_ext": types.SimpleNamespace(**dict(vars(vqro_ext), Extension=lambda *a, **k: core))}):
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
        self.assertEqual(module["show_cmd"]({"resource": "r1"})["client"], {"focus": {"view": "term-r1"}})

    def test_tabs_show_retries_transient_cas_losses_without_duplicate_tabs_or_effects(self):
        module, core=load("tabs")
        core.apply([{"op":"set","key":"layout/main","value":{"tabs":[{"title":"original","node":{"view":"original"}}],"active":0}}],"fixture")
        apply=core.apply
        count=0
        def competing_write(ops,note):
            nonlocal count
            count+=1
            if count<=4:
                root=copy.deepcopy(core.value("layout/main"))
                root["tabs"].append({"title":f"other-{count}","node":{"view":f"other-{count}"}})
                apply([{"op":"set","key":"layout/main","value":root}],"concurrent writer")
                raise VqroError("CAS lost","conflict")
            return apply(ops,note)
        with mock.patch.object(core,"apply",side_effect=competing_write), mock.patch("vqro_ext.time.sleep"):
            result=module["show_cmd"]({"resource":"r1"})
        self.assertEqual(count,5)
        tabs=core.value("layout/main")["tabs"]
        self.assertEqual([t["title"] for t in tabs[:5]],["original","other-1","other-2","other-3","other-4"])
        self.assertEqual(sum(t["node"].get("view")=="term-r1" for t in tabs),1)
        self.assertEqual(result["client"],{"focus":{"view":"term-r1"}})
        self.assertFalse(any(c[0]=="resource.spawn" for c in core.calls))
        with mock.patch.object(core,"apply",side_effect=VqroError("unknown write outcome","timeout")) as failed:
            with self.assertRaises(VqroError):module["show_cmd"]({"view":"other-view"})
            self.assertEqual(failed.call_count,1,"unknown outcomes are never retried as CAS")

    def test_failed_agent_show_does_not_replay_as_terminal_navigation(self):
        _, core = load("tabs")
        with mock.patch.object(core, "invoke", side_effect=VqroError("unknown layout outcome", "timeout")):
            with self.assertRaises(VqroError):
                terminal_target(core, "r2")
        self.assertIsNone(core.value("layout/main"))
        core.resources["r3"] = {"id": "r3", "kind": "opaque"}
        with self.assertRaises(VqroError):
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

    def test_ended_markers_follow_lifecycle_without_status_transaction_churn(self):
        module, core = load("tabs")
        core.apply([{"op": "set", "key": "layout/main", "value": {"tabs": [
            {"title": "User title", "node": {"view": "agents-r2"}}], "active": 0}}], "fixture")
        module["resource_state"]("resource", {"id": "r2"})
        count = len(core.transactions)
        for _ in range(20):
            module["resource_state"]("resource", {"id": "r2"})
        self.assertEqual(len(core.transactions), count)
        core.resources.pop("r2")
        module["resource_state"]("resource.removed", {"id": "r2"})
        self.assertEqual(core.value("layout/main")["tabs"][0]["title"], "User title [gone]")
        core.add_agent()
        module["resource_state"]("resource", {"id": "r2"})
        self.assertEqual(core.value("layout/main")["tabs"][0]["title"], "User title")

    def test_close_tab_deletes_nested_orphans_but_preserves_other_layout_and_popup_views(self):
        module, core = load("tabs")
        root = {"tabs": [{"title": "closed", "node": {"split": "h", "children": [{"view": "orphan"}, {"view": "shared"}]}},
                         {"title": "kept", "node": {"view": "kept"}}], "active": 0}
        ops = [{"op": "set", "key": "view/"+name, "value": {"kind": "panel", "items": []}} for name in ("orphan", "shared", "kept", "popup")]
        ops += [{"op": "set", "key": "layout/main", "value": root},
                {"op": "set", "key": "layout/other", "value": {"view": "shared"}}]
        core.apply(ops, "fixture")
        module["close_tab"]({"context": {"layout": "main", "layout_version": core.version("layout/main"), "tab_path": [], "tab_index": 0}})
        self.assertIsNone(core.value("view/orphan"))
        for name in ("shared", "kept", "popup"):
            self.assertIsNotNone(core.value("view/"+name))
        deleted = [op["key"] for op in core.transactions[-1] if op["op"] == "delete"]
        self.assertEqual(deleted, ["view/orphan"])
        self.assertFalse(any(method == "resource.kill" for method, _ in core.calls))

    def test_close_failure_does_not_mutate_the_cached_layout_before_commit(self):
        module, core = load("tabs")
        root = {"tabs": [{"title": "a", "node": {"view": "a"}}, {"title": "b", "node": {"view": "b"}}], "active": 0}
        core.apply([{"op": "set", "key": "layout/main", "value": root}], "fixture")
        with mock.patch.object(core, "value", side_effect=lambda key, default=None: core.entries.get(key, {}).get("value", default)), \
             mock.patch.object(core, "apply", side_effect=VqroError("layout changed", "conflict")):
            with self.assertRaises(VqroError):
                module["close_tab"]({"context": {"layout": "main", "layout_version": 1, "tab_path": [], "tab_index": 0}})
        self.assertEqual(core.value("layout/main"), root)

    def test_close_ended_deletes_only_unreferenced_removed_views(self):
        module, core = load("tabs")
        for rid in ("r3", "r4"):
            core.resources[rid] = {"id": rid, "kind": "terminal", "state": "finished"}
            core.apply([{"op": "set", "key": "view/term-"+rid, "value": {"kind": "terminal", "resource": rid}}], "fixture")
        root = {"tabs": [{"title": rid, "node": {"view": "term-"+rid}} for rid in ("r3", "r4")]+[
            {"title": "live", "node": {"view": "agents-r2"}}], "active": 0}
        core.apply([{"op": "set", "key": "layout/main", "value": root},
                    {"op": "set", "key": "layout/other", "value": {"view": "term-r3"}}], "fixture")
        module["close_ended"]({})
        self.assertIsNone(core.value("view/term-r4"))
        self.assertIsNotNone(core.value("view/term-r3"), "another layout still pins the shared view")
        self.assertEqual([op["key"] for op in core.transactions[-1] if op["op"] == "delete"], ["view/term-r4"])

    def test_close_ended_removes_a_view_only_after_its_last_layout_reference(self):
        module, core = load("tabs")
        core.resources["r3"] = {"id": "r3", "kind": "terminal", "state": "finished"}
        core.apply([{"op": "set", "key": "view/term-r3", "value": {"kind": "terminal", "resource": "r3"}}], "fixture")
        for name in ("main", "other"):
            core.apply([{"op": "set", "key": "layout/"+name, "value": {"tabs": [
                {"title": "ended", "node": {"view": "term-r3"}}, {"title": "live", "node": {"view": "agents-r2"}}], "active": 0}}], "fixture")
        module["close_ended"]({})
        self.assertIsNone(core.value("view/term-r3"))
        self.assertEqual([op["key"] for op in core.transactions[-1] if op["op"] == "delete"], ["view/term-r3"])

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
