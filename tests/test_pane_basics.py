"""Tab and pane rearranging (tabs.move, panes.swap/rotate/break/move/resize/layout). No sockets/processes."""
import unittest

from test_workspace_tabs import Core, load


def leaf(v):
    return {"view": v}


class PaneBasics(unittest.TestCase):
    def setUp(self):
        self.core = Core()
        self.core.resources["r0"] = {"id": "r0", "kind": "terminal", "argv": ["sh"], "state": "live"}
        views = ["side", "a", "b", "c", "d"]
        ops = [{"op": "set", "key": f"view/{v}", "value": {"kind": "terminal", "resource": "r0"}} for v in views]
        # The sidebar beside a tab group: tab 1 holds a | (b / c), tab 2 holds d.
        root = {"split": "h", "sizes": [1, 4], "children": [leaf("side"), {"tabs": [
            {"title": "one", "node": {"split": "h", "sizes": [3, 1], "children": [leaf("a"), {"split": "v", "children": [leaf("b"), leaf("c")]}]}},
            {"title": "two", "node": leaf("d")}], "active": 0}]}
        ops += [{"op": "set", "key": "layout/main", "value": root}, {"op": "set", "key": "workspace/active", "value": {"layout": "main"}}]
        self.core.apply(ops, "setup")
        self.mods = load(self.core, "tabs", "panes")

    def root(self):
        return self.core.value("layout/main")

    def tabs(self):
        return self.root()["children"][1]

    def ctx(self, path=None, view=None, tab=0, **extra):
        c = {"layout": "main", "layout_version": self.core.version("layout/main"), "tab_path": [1], "tab_index": tab, **extra}
        if path is not None:
            c.update(path=path, view=view, title=view)
        return c

    def run_(self, name, args=None, **ctx):
        return self.core.handlers[name](dict(args or {}, context=self.ctx(**ctx)))

    # ---- tabs.move ----

    def test_tab_moves_left_right_and_to_a_number_keeping_the_shown_tab(self):
        self.run_("tabs.move", {"to": "right"})
        self.assertEqual([t["title"] for t in self.tabs()["tabs"]], ["two", "one"])
        self.assertEqual(self.tabs()["active"], 1, "the shown tab stays shown")
        self.assertIsNone(self.run_("tabs.move", {"to": "right"}, tab=1), "already at the end: nothing to do")
        self.run_("tabs.move", {"text": "1"}, tab=1)
        self.assertEqual([t["title"] for t in self.tabs()["tabs"]], ["one", "two"])
        with self.assertRaises(Exception):
            self.run_("tabs.move", {"to": "9"})

    def test_a_tab_drag_drop_index_moves_the_dragged_tab(self):
        self.run_("tabs.move", tab=0, drop_index=1)
        self.assertEqual([t["title"] for t in self.tabs()["tabs"]], ["two", "one"])

    # ---- swap / rotate ----

    def test_swap_and_rotate_stay_in_the_tab_and_focus_follows(self):
        out = self.run_("panes.swap", {"dir": "next"}, path=[1, 0, 0], view="a")
        self.assertEqual(out, {"client": {"focus": {"view": "a"}}})
        one = self.tabs()["tabs"][0]["node"]
        self.assertEqual(one["children"][0], leaf("b"))
        self.assertEqual(one["children"][1]["children"], [leaf("a"), leaf("c")])
        self.assertEqual(one["sizes"], [3, 1], "sizes stay with the slots")
        self.run_("panes.rotate", {"dir": "next"}, path=[1, 0, 1, 0], view="a")
        one = self.tabs()["tabs"][0]["node"]
        self.assertEqual([one["children"][0]] + one["children"][1]["children"], [leaf("c"), leaf("b"), leaf("a")])
        with self.assertRaises(Exception):
            self.run_("panes.swap", {}, path=[1, 1], view="d", tab=1)  # the only pane of tab 2

    def test_a_stale_layout_version_changes_nothing(self):
        before = self.root()
        with self.assertRaises(Exception):
            self.core.handlers["panes.swap"]({"context": dict(self.ctx(path=[1, 0, 0], view="a"), layout_version=0)})
        self.assertEqual(self.root(), before)

    # ---- break / move ----

    def test_break_moves_a_pane_to_a_new_tab_after_its_own(self):
        self.run_("panes.break", path=[1, 0, 1, 1], view="c")
        tabs = self.tabs()
        self.assertEqual([t["title"] for t in tabs["tabs"]], ["one", "c", "two"])
        self.assertEqual(tabs["active"], 1)
        self.assertEqual(tabs["tabs"][0]["node"]["children"], [leaf("a"), leaf("b")], "the one-child split collapsed")
        self.assertEqual(tabs["tabs"][1]["node"], leaf("c"))
        with self.assertRaises(Exception):
            self.run_("panes.break", path=[1, 1], view="c", tab=1)  # already a whole tab

    def test_move_puts_a_pane_beside_another_tabs_panes(self):
        self.run_("panes.move", {"text": "2"}, path=[1, 0, 0], view="a")
        tabs = self.tabs()
        self.assertEqual(tabs["tabs"][0]["node"], {"split": "v", "children": [leaf("b"), leaf("c")]})
        self.assertEqual(tabs["tabs"][1]["node"], {"split": "h", "children": [leaf("d"), leaf("a")], "sizes": [1, 1]})
        self.assertEqual(tabs["active"], 1)

    def test_moving_a_whole_tab_pane_removes_that_tab(self):
        self.run_("panes.move", {"to": 1}, path=[1, 1], view="d", tab=1)
        tabs = self.tabs()
        self.assertEqual(len(tabs["tabs"]), 1)
        self.assertEqual(tabs["tabs"][0]["node"]["children"][1], leaf("d"))
        self.assertEqual(tabs["active"], 0)

    # ---- resize ----

    def test_resize_moves_the_nearest_divider_on_that_axis(self):
        self.run_("panes.resize", {"dir": "right"}, path=[1, 0, 0], view="a")
        self.assertEqual(self.tabs()["tabs"][0]["node"]["sizes"], [800, 200], "small weights rescale; 5% of the total moves")
        self.run_("panes.resize", {"dir": "left", "by": 10}, path=[1, 0, 1, 0], view="b")  # b is the right child: its left edge
        self.assertEqual(self.tabs()["tabs"][0]["node"]["sizes"], [700, 300])
        self.run_("panes.resize", {"dir": "down"}, path=[1, 0, 1, 0], view="b")
        self.assertEqual(self.tabs()["tabs"][0]["node"]["children"][1]["sizes"], [550, 450])
        with self.assertRaises(Exception):
            self.run_("panes.resize", {"dir": "up"}, path=[1, 0, 0], view="a")  # no stacked split above a in its tab
        for _ in range(30):
            self.run_("panes.resize", {"dir": "right", "by": 50}, path=[1, 0, 0], view="a")
        self.assertEqual(self.tabs()["tabs"][0]["node"]["sizes"], [950, 50], "never below 5%")

    # ---- presets ----

    def test_layout_presets_rebuild_the_tab_from_its_panes_and_cycle(self):
        self.run_("panes.layout", {"preset": "even-h"}, path=[1, 0, 0], view="a")
        node = self.tabs()["tabs"][0]["node"]
        self.assertEqual(node, {"split": "h", "children": [leaf("a"), leaf("b"), leaf("c")], "sizes": [1, 1, 1], "preset": "even-h"})
        self.run_("panes.layout", {}, path=[1, 0, 0], view="a")
        self.assertEqual(self.tabs()["tabs"][0]["node"]["preset"], "even-v")
        self.run_("panes.layout", {"preset": "main-v"}, path=[1, 0, 0], view="a")
        node = self.tabs()["tabs"][0]["node"]
        self.assertEqual(node["children"], [leaf("a"), {"split": "v", "children": [leaf("b"), leaf("c")], "sizes": [1, 1]}])
        self.run_("panes.layout", {"preset": "tiled"}, path=[1, 0, 0], view="a")
        node = self.tabs()["tabs"][0]["node"]
        self.assertEqual(node["split"], "v")
        self.assertEqual(node["children"][0]["children"], [leaf("a"), leaf("b")])
        self.assertEqual(node["children"][1], leaf("c"))
        self.assertEqual(self.root()["children"][0], leaf("side"), "outside the tab nothing moves")

    def test_program_titles_is_a_tabs_setting(self):
        self.core.handlers["tabs.set"]({"field": "program_titles", "value": False})
        self.assertEqual(self.core.value("doc/settings/tabs"), {"program_titles": False})
        with self.assertRaises(Exception):
            self.core.handlers["tabs.set"]({"field": "program_titles", "value": "off"})
        self.core.handlers["tabs.settings"]({})
        row = next(i for i in self.core.live["tabs-settings"]["items"] if i["text"] == "Program titles")
        self.assertIs(row["value"], False)

    def test_bindings_are_declared(self):
        keys = {v["key"]: v["command"] for k, v in self.core.entries.items() if k.startswith("binding/") for v in [v["value"]]}
        for key, command in {"r": "tabs.rename", "<": "tabs.move", ">": "tabs.move", ".": "tabs.move", ";": "client.focus",
                             "{": "panes.swap", "}": "panes.swap", "C-o": "panes.rotate", "b": "panes.break", "M": "panes.move",
                             "Space": "panes.layout", "C-Left": "panes.resize", "C-Down": "panes.resize"}.items():
            self.assertEqual(keys.get(key), command, key)


if __name__ == "__main__":
    unittest.main()
