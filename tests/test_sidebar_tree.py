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

    def sidebar(self):
        """The sidebar content published last."""
        return [value for name, value in self.ext.published if name == "sidebar"][-1]

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
        self.assertEqual(worktrees(), ["wt:/w/wt-dirty", "wt:/w/wt-ahead", "more:" + WS], "a cut lists changed ones first")
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

    def test_ended_terminals_no_tab_shows_fold_into_one_row_so_daily_use_needs_no_pager(self):
        """relay-13: the human saw the pager "1/2" in daily use. Retention keeps up to 64 never-shown ended terminals,
        one row each (~350 bytes) pushed a normal sidebar past one page. They now fold into one "N ended" row whose
        rows are published only while it is opened."""
        for i in range(64):
            self.ext.resources[f"e{i}"] = term(f"e{i}", cwd=WS, state="finished", name=f"make target-{i}")
        self.module["refresh"]()
        content = dict(self.ext.published)["sidebar"]
        self.assertEqual(content["page"]["count"], 1)
        self.assertEqual(len(content["toolbar"]), 1, "no pager chips")
        fold = find(content["items"], "ended:" + WS)
        self.assertEqual((fold["text"], fold["children"]), ("64 ended", []))
        def walk(rows):
            for row in rows:
                yield row
                yield from walk(row.get("children") or [])
        self.assertFalse(any(row.get("args", {}).get("resource", "").startswith("e") for row in walk(content["items"])))
        self.module["ended_cmd"]({"id": "ended:" + WS})
        opened = find(self.build(), "ended:" + WS)
        self.assertEqual(len(opened["children"]), 64)
        self.assertEqual(opened["menu"][0]["label"], "Hide them")
        found, _ = self.module["search_rows"]("target-63")
        self.assertTrue(found, "ended terminals stay searchable while folded")

    def test_a_workspace_with_185_worktrees_needs_no_pager_and_a_page_click_publishes_at_once(self):
        """The human's sidebar (feedback batch 1): vqro has 185 git worktrees. Each was a ~460-byte row, so the
        published tree filled pages 1/3 and 2/3 behind a collapsed "▸ vqro" (the ▸ is the window's own: the
        published tree still held them). Now a workspace lists 10 (changed first) and "+175 more"."""
        self.ext.entries["workspace/active"] = {"value": {"layout": "ws-a"}, "version": 1}
        w = self.ext.live["workspaces"]["data"][0]
        w["worktrees"] = [{"path": f"/w/wt{i}", "branch": f"b{i}", "dirty": 3 if i in (150, 160) else 0, "layout": f"wt-{i}"}
                          for i in range(185)]
        self.module["refresh"]()
        content = dict(self.ext.published)["sidebar"]
        self.assertEqual(content["page"]["count"], 1)
        self.assertEqual(len(content["toolbar"]), 1, "no pager chips")
        ws = find(content["items"], "ws:" + WS)
        wts = [r["id"] for r in ws["children"] if r["id"].startswith("wt:")]
        self.assertEqual(wts[:2], ["wt:/w/wt150", "wt:/w/wt160"], "the changed worktrees come first")
        self.assertEqual(len(wts), 10)
        self.assertEqual(find(content["items"], "more:" + WS)["text"], "+175 more worktrees")
        # "+N more" opens the rest (a tree this large pages again until collapse reaches the extension); a page
        # click publishes in its own call, not after the worker's coalescing wait.
        self.module["worktrees_more"]({"path": WS})
        content = self.sidebar()
        self.assertGreater(content["page"]["count"], 1)
        before = len(self.ext.published)
        self.module["page_cmd"]({"delta": 1})
        self.assertGreater(len(self.ext.published), before, "published at once")
        self.assertEqual(self.sidebar()["page"]["number"], 2)

    def test_a_tree_between_a_page_and_the_whole_budget_is_still_one_page(self):
        g = self.module["build"].__globals__
        row = lambda i: {"id": f"term:r{i}", "text": "x" * 400}
        rows = [{"id": "ws:root", "text": "Root", "children": [row(i) for i in range(130)]}]  # about 55 KB
        self.assertGreater(self.module["encoded"](rows), g["PAGE_BYTES"])
        self.assertLess(self.module["encoded"](rows), g["WHOLE_BYTES"])
        self.assertEqual(self.module["paginate"](rows, 0)[2], 1, "published whole: no pager")
        more = [{"id": "ws:root", "text": "Root", "children": [row(i) for i in range(170)]}]  # over WHOLE_BYTES
        self.assertGreater(self.module["paginate"](more, 0)[2], 1, "pages stay the last resort")

    def test_a_large_tree_collapses_places_that_are_not_open(self):
        g = self.module["build"].__globals__
        self.add_workspaces(30)
        g["PAGE_BYTES"] = g["WHOLE_BYTES"] = 4000  # force compaction for this small fixture
        self.ext.entries["workspace/active"] = {"value": {"layout": "ws-a"}, "version": 1}
        rows = self.build()
        ws = {row["id"]: row for row in rows if row.get("id", "").startswith(("ws:", "layout:"))}
        self.assertIn("children", ws["ws:" + WS], "the default workspace stays open")
        self.assertNotIn("children", ws["ws:/w/extra3"])
        # A lazy row (its ▸ asks for the contents); clients without lazy_children ignore those fields and open it
        # with "Expand here".
        lazy = ws["ws:/w/extra3"]
        self.assertEqual((lazy["has_children"], lazy["expanded"]), (True, False))
        self.assertEqual(lazy["on_toggle"], {"command": "sidebar.place", "args": {"id": "ws:/w/extra3"}})
        self.assertEqual(lazy["menu"], [{"label": "Expand here", "command": "sidebar.expand", "args": {"id": "ws:/w/extra3"}}])
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
        g["PAGE_BYTES"], g["WHOLE_BYTES"] = 48 * 1024, 60 * 1024

    def test_a_place_closed_by_its_triangle_is_published_as_one_row_so_hidden_rows_never_page(self):
        """Human feedback (batch 1): pages 1/3 and 2/3 showed only a collapsed "▸ vqro", because the ▸ was the
        window's own and the published tree still held vqro's rows. A client with lazy_children reports the ▸
        (sidebar.place): the place is then published as one lazy row, at any size, and opening it brings it back."""
        self.ext.entries["workspace/active"] = {"value": {"layout": "ws-a"}, "version": 1}
        w = self.ext.live["workspaces"]["data"][0]
        w["worktrees"] = [{"path": f"/w/wt{i}", "branch": f"b{i}", "dirty": 0, "layout": f"wt-{i}"} for i in range(185)]
        self.module["worktrees_more"]({"path": WS})  # every worktree listed: 112 KB, pages
        self.assertGreater(self.sidebar()["page"]["count"], 1)
        open_row = find(self.build(), "ws:" + WS)
        self.assertEqual(open_row["on_toggle"], {"command": "sidebar.place", "args": {"id": "ws:" + WS}}, "an open place reports its ▸ too")
        self.module["toggle_cmd"]({"id": "ws:" + WS, "open": False})
        content = self.sidebar()
        self.assertEqual(content["page"]["count"], 1, "no pager: the closed place's rows are not published")
        closed = find(content["items"], "ws:" + WS)
        self.assertNotIn("children", closed)
        self.assertTrue(closed["has_children"])
        self.module["toggle_cmd"]({"id": "ws:" + WS, "open": True})
        self.assertIn("children", find(self.sidebar()["items"], "ws:" + WS))

    def test_search_covers_every_row_with_its_place(self):
        g = self.module["build"].__globals__
        self.add_workspaces(30)
        g["PAGE_BYTES"] = g["WHOLE_BYTES"] = 4000
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
        g["PAGE_BYTES"], g["WHOLE_BYTES"] = 48 * 1024, 60 * 1024

    def test_without_workspace_data_layouts_are_the_places(self):
        del self.ext.live["workspaces"]
        rows = self.build()
        self.assertEqual(ids(rows)[:2], ["layout:main", "layout:ws-a"])
        self.assertEqual(rows[-1].get("args", {}).get("resource"), "r4")


if __name__ == "__main__":
    unittest.main()
