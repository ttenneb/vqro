"""Sidebar as one tree: levels can be skipped in any combination; worktree options; one option state."""
import copy
from pathlib import Path
import runpy
import sys
import threading
import types
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
import vqro_ext  # noqa: E402

WS = "/w/proj"


class Extension:
    """Public surface the sidebar uses: caches, contribute/publish/apply, nothing running."""
    def __init__(self, *_, **__):
        self.entries, self.live, self.providers, self.resources = {}, {}, {}, {}
        self.cache_lock, self.published = threading.RLock(), []
    def value(self, key, default=None):
        return copy.deepcopy(self.entries.get(key, {}).get("value", default))
    def version(self, key):
        return self.entries.get(key, {}).get("version", 0)
    def apply(self, ops, note):
        for op in ops:
            self.entries[op["key"]] = {"value": op["value"], "version": self.version(op["key"]) + 1}
        return {}
    def retrying(self, fn, attempts=3):
        return fn()
    def on_event(self, fn): return fn
    def command(self, *_): return lambda fn: fn
    def contribute(self, *_): pass
    def run(self): pass
    def publish(self, name, value): self.published.append((name, copy.deepcopy(value)))
    def log(self, *args): pass


def load():
    fake = types.SimpleNamespace(Extension=Extension, VqroError=vqro_ext.VqroError, views_in=vqro_ext.views_in)
    with mock.patch.dict(sys.modules, {"vqro_ext": fake}), mock.patch.object(threading, "Thread"):
        module = runpy.run_path(str(Path(__file__).resolve().parents[1] / "extensions/sidebar.py"))
    return module, module["ext"]


def term(rid, cwd="/elsewhere", **extra):
    return {"id": rid, "kind": "terminal", "argv": ["zsh"], "name": f"t{rid[1:]}", "state": "live", "cwd": cwd, **extra}


def fixture(ext):
    """proj: tab 0 = r1 beside agent r5 (bridge r6), tab 1 = r2; r3 works in proj in no tab; r4 in no folder or tab;
    four worktrees (dirty, detached, ahead, clean); layout `main` (unclaimed) has r7."""
    ext.resources = {rid: term(rid) for rid in ("r1", "r2", "r4", "r6", "r7")}
    ext.resources["r3"] = term("r3", cwd=WS + "/src")
    ext.resources["r5"] = {"id": "r5", "kind": "agent", "name": "Pi", "host": "r6", "state": "live", "cwd": WS}
    views = {f"term-{rid}": {"kind": "terminal", "resource": rid} for rid in ("r1", "r2", "r7")}
    layouts = {
        "ws-a": {"split": "h", "children": [{"view": "sidebar"}, {"tabs": [
            {"title": "edit", "node": {"split": "h", "children": [{"view": "term-r1"}, {"view": "agents-r5"}]}},
            {"title": "run", "node": {"view": "term-r2"}}], "active": 1}]},
        "main": {"tabs": [{"title": "misc", "node": {"view": "term-r7"}}], "active": 0},
    }
    for key, value in [*((f"view/{k}", v) for k, v in views.items()), *((f"layout/{k}", v) for k, v in layouts.items())]:
        ext.entries[key] = {"value": value, "version": 1}
    worktrees = [{"path": "/w/wt-dirty", "branch": "fix", "dirty": 3, "layout": "ws-b"},
                 {"path": "/w/wt-detached", "branch": "(detached)", "dirty": 0, "layout": "ws-c"},
                 {"path": "/w/wt-ahead", "branch": "feat", "dirty": 0, "ahead": 2, "layout": "ws-d"},
                 {"path": "/w/wt-clean", "branch": "old", "dirty": 0, "ahead": 0, "layout": "ws-e"}]
    ext.live["workspaces"] = {"data": [{"path": WS, "name": "proj", "git": True, "branch": "main", "layout": "ws-a",
                                        "worktrees": worktrees}]}


def ids(rows):
    return [row.get("id") or ("#" + row.get("text", "")) for row in rows]


def find(rows, rid):
    for row in rows:
        if row.get("id") == rid:
            return row
        found = find(row.get("children", []), rid)
        if found:
            return found
    return None


def set_show(module, ext, **show):
    ext.entries[module["PREFS"]] = {"value": dict(ext.value(module["PREFS"]) or {}, show=dict(module["SHOW_DEFAULT"], **show)), "version": 9}


class SidebarTree(unittest.TestCase):
    def setUp(self):
        self.module, self.ext = load()
        fixture(self.ext)

    def build(self):
        return self.module["build"]()

    def test_default_is_one_tree_workspace_tabs_panes_then_not_in_a_tab_and_worktrees(self):
        rows = self.build()
        self.assertEqual(ids(rows), ["#Workspaces", "ws:" + WS, "add", "#Layouts", "layout:main", "#Other terminals", "term:r4"])
        ws = rows[1]
        self.assertEqual(ids(ws["children"]), ["tab:ws-a:0", "tab:ws-a:1", "loose:" + WS,
                                               "wt:/w/wt-dirty", "wt:/w/wt-detached", "wt:/w/wt-ahead", "wt:/w/wt-clean"])
        edit = ws["children"][0]
        self.assertEqual([row["args"]["resource"] for row in edit["children"]], ["r1", "r5"])
        self.assertEqual(edit["children"][1]["children"][0]["args"]["resource"], "r6", "the agent carries its bridge")
        self.assertEqual(ws["children"][1]["icon"], "●", "the selected tab is marked")
        self.assertEqual([row["args"]["resource"] for row in ws["children"][2]["children"]], ["r3"], "r3 works in proj, in no tab")
        self.assertEqual(find(rows, "layout:main")["children"][0]["children"][0]["args"]["resource"], "r7")
        flat = str(rows)
        self.assertNotIn("_level", flat)
        bridges = []
        def walk(rs, parent=None):
            for row in rs:
                if row.get("args", {}).get("resource") == "r6":
                    bridges.append(parent)
                walk(row.get("children", []), row.get("args", {}).get("resource"))
        walk(rows)
        self.assertEqual(bridges, ["r5"], "a bridge is only its agent's child, never a row of its own")

    def test_hiding_tabs_moves_panes_up_under_the_workspace(self):
        set_show(self.module, self.ext, tabs=False)
        ws = find(self.build(), "ws:" + WS)
        self.assertEqual([row["args"].get("resource") or row["id"] for row in ws["children"][:4]], ["r1", "r5", "r2", "r3"])
        self.assertFalse(any(row["id"].startswith(("tab:", "loose:")) for row in ws["children"]))

    def test_hiding_panes_keeps_tabs_and_drops_empty_not_in_a_tab(self):
        set_show(self.module, self.ext, panes=False)
        ws = find(self.build(), "ws:" + WS)
        self.assertEqual(ids(ws["children"])[:2], ["tab:ws-a:0", "tab:ws-a:1"])
        self.assertEqual(ws["children"][0]["children"], [])
        self.assertIsNone(find([ws], "loose:" + WS))

    def test_hiding_workspaces_gives_a_flat_list_and_every_combination_builds(self):
        set_show(self.module, self.ext, workspaces=False, worktrees=False, tabs=False)
        rows = self.build()
        self.assertEqual([row["args"]["resource"] for row in rows if row.get("id", "").startswith("term:")][:5], ["r1", "r5", "r2", "r3", "r7"])
        self.assertNotIn("#Workspaces", ids(rows))
        self.assertNotIn("add", ids(rows))
        import itertools
        keys = list(self.module["SHOW_DEFAULT"])
        for values in itertools.product([True, False], repeat=len(keys)):
            set_show(self.module, self.ext, **dict(zip(keys, values)))
            rows = self.build()
            self.assertTrue(rows, values)
            self.assertNotIn("_level", str(rows), values)

    def test_worktree_options_limit_changed_only_and_hide_detached(self):
        prefs = self.module["PREFS"]
        def worktrees():
            return [row["id"] for row in find(self.build(), "ws:" + WS)["children"] if row["id"].startswith(("wt:", "more:"))]
        self.module["worktrees_cmd"]({"key": "limit", "value": 2})
        self.assertEqual(worktrees(), ["wt:/w/wt-dirty", "wt:/w/wt-detached", "more:" + WS])
        self.assertEqual(find(self.build(), "more:" + WS)["text"], "+2 more worktrees")
        self.module["worktrees_more"]({"path": WS})
        self.assertEqual(len(worktrees()), 4, "+N more lists them all for this session")
        self.module["expanded_worktrees"].clear()
        self.module["worktrees_cmd"]({"key": "limit", "value": 0})
        self.module["worktrees_cmd"]({"key": "changed_only", "value": True})
        self.assertEqual(worktrees(), ["wt:/w/wt-dirty", "wt:/w/wt-ahead"])
        self.module["worktrees_cmd"]({"key": "changed_only", "value": False})
        self.module["worktrees_cmd"]({"key": "hide_detached", "value": True})
        self.assertEqual(worktrees(), ["wt:/w/wt-dirty", "wt:/w/wt-ahead", "wt:/w/wt-clean"])
        self.assertEqual(self.ext.value(prefs)["worktrees"], {"limit": 0, "changed_only": False, "hide_detached": True})
        with self.assertRaises(vqro_ext.VqroError):
            self.module["worktrees_cmd"]({"key": "limit", "value": -1})
        self.assertIn({"text": "↑2", "style": "accent"}, find(self.build(), "wt:/w/wt-ahead")["badges"])

    def test_show_popup_and_settings_page_are_the_same_rows_over_one_state(self):
        self.module["preset"]({"show": {"tabs": False}})
        self.module["refresh"]()
        published = dict(self.ext.published)
        page, popup = published["settings.page.sidebar"], published["sidebar.view"]
        self.assertEqual(page["title"], "Sidebar")
        self.assertEqual(page["items"], popup["items"][:len(page["items"])], "Settings shows exactly the popup's options")
        tabs = next(row for row in page["items"] if row.get("text") == "Tabs")
        self.assertEqual((tabs["checked"], tabs["command"], tabs["args"]), (False, "sidebar.preset", {"show": {"tabs": True}}))
        self.assertEqual(self.ext.value(self.module["PREFS"])["show"]["tabs"], False)

    def test_older_terminals_preference_still_hides_panes(self):
        self.ext.entries[self.module["PREFS"]] = {"value": {"show": {"terminals": False}}, "version": 1}
        self.assertFalse(self.module["show"]()["panes"])
        self.assertEqual(self.module["current_preset"](), None)

    def test_one_resource_in_two_tabs_is_two_rows_but_counted_once(self):
        layout = self.ext.value("layout/ws-a")
        layout["children"][1]["tabs"][1]["node"] = {"split": "v", "children": [{"view": "term-r2"}, {"view": "term-r1"}]}
        self.ext.entries["layout/ws-a"] = {"value": layout, "version": 2}
        self.ext.resources["r1"]["status"] = {"state": "blocked"}
        rows = self.build()
        ws = find(rows, "ws:" + WS)
        self.assertEqual(sum("r1" == row.get("args", {}).get("resource") for tab in ws["children"][:2] for row in tab["children"]), 2)
        self.assertIn({"text": "1 blocked", "style": "warn"}, ws["badges"])
        row_ids = []
        def walk(rs):
            for row in rs:
                if row.get("id"):
                    row_ids.append(row["id"])
                walk(row.get("children", []))
        walk(rows)
        self.assertEqual(len(row_ids), len(set(row_ids)), "row ids stay unique for client selection")

    def add_workspaces(self, n):
        data = self.ext.live["workspaces"]["data"]
        for i in range(n):
            data.append({"path": f"/w/extra{i}", "name": f"extra {i}", "layout": f"ws-x{i}", "worktrees": []})
            self.ext.resources[f"x{i}"] = term(f"x{i}", cwd=f"/w/extra{i}", name=f"extra shell {i}")

    def test_a_tree_that_fits_one_page_is_published_whole_and_without_a_pager(self):
        self.add_workspaces(5)
        self.module["refresh"]()
        content = dict(self.ext.published)["sidebar"]
        self.assertEqual(content["page"]["count"], 1)
        self.assertEqual(len(content["toolbar"]), 1, "no pager chips")
        self.assertTrue(all("children" in row for row in content["items"] if row.get("id", "").startswith("ws:")))

    def test_a_large_tree_collapses_places_that_are_not_open(self):
        g = self.module["build"].__globals__
        self.add_workspaces(30)
        g["PAGE_BYTES"] = 4000  # force compaction for this small fixture
        self.ext.entries["workspace/active"] = {"value": {"layout": "ws-a"}, "version": 1}
        rows = self.build()
        ws = {row["id"]: row for row in rows if row.get("id", "").startswith(("ws:", "layout:"))}
        self.assertIn("children", ws["ws:" + WS], "the default workspace stays open")
        self.assertNotIn("children", ws["ws:/w/extra3"])
        self.assertEqual(ws["ws:/w/extra3"]["menu"], [{"label": "Expand here", "command": "sidebar.expand", "args": {"id": "ws:/w/extra3"}}])
        self.assertIn({"label": "Collapse", "command": "sidebar.collapse", "args": {"id": "ws:" + WS}}, ws["ws:" + WS]["menu"])
        self.module["expand_cmd"]({"id": "ws:/w/extra3"})
        self.assertIn("children", find(self.build(), "ws:/w/extra3"))
        self.module["collapse_cmd"]({"id": "ws:" + WS})
        self.assertNotIn("children", find(self.build(), "ws:" + WS))
        # Going there (it becomes the default again) opens it.
        self.ext.entries["workspace/active"] = {"value": {"layout": "ws-x1"}, "version": 2}
        self.build()
        self.ext.entries["workspace/active"] = {"value": {"layout": "ws-a"}, "version": 3}
        rows = self.build()
        self.assertIn("children", find(rows, "ws:" + WS))
        self.assertIn("children", find(rows, "ws:/w/extra1"), "recently shown places stay open (bounded)")
        # A blocked terminal inside a collapsed place still counts in the status segment and its row's roll-up.
        self.ext.resources["x7"]["status"] = {"state": "blocked"}
        self.assertIn({"text": "1 blocked", "style": "warn"}, find(self.build(), "ws:/w/extra7")["badges"])
        self.assertEqual(self.module["status_counts"](), {"blocked": 1})
        g["PAGE_BYTES"] = 48 * 1024

    def test_search_covers_every_row_with_its_place(self):
        g = self.module["build"].__globals__
        self.add_workspaces(30)
        g["PAGE_BYTES"] = 4000
        set_show(self.module, self.ext, tabs=False)  # search ignores what is hidden or collapsed
        found, more = self.module["search_rows"]("EXTRA SHELL 2")
        self.assertEqual([row["args"]["resource"] for row in found], ["x2"] + [f"x{i}" for i in range(20, 30)])
        self.assertEqual(found[0]["description"], "extra 2 › Not in a tab")
        self.assertEqual(more, 0)
        found, _ = self.module["search_rows"]("run")
        self.assertEqual([(row["command"], row["description"]) for row in found], [("sidebar.tab", "proj")])
        self.assertEqual(self.module["search_cmd"]({"text": "shell"})["client"], {"popup": {"view": "sidebar.search", "width": 60}})
        published = dict(self.ext.published)["sidebar.search"]
        self.assertEqual(len([row for row in published["items"] if row.get("command")]), 30)
        g["PAGE_BYTES"] = 48 * 1024

    def test_without_workspace_data_layouts_are_the_places(self):
        del self.ext.live["workspaces"]
        rows = self.build()
        self.assertEqual(ids(rows)[:2], ["layout:main", "layout:ws-a"])
        self.assertEqual(rows[-1].get("args", {}).get("resource"), "r4")


if __name__ == "__main__":
    unittest.main()
