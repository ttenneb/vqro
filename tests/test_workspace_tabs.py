"""Workspaces own their tabs: one layout per workspace, layout settings, sidebar tabs. No sockets/processes."""
import copy
import os
from pathlib import Path
import runpy
import sys
import tempfile
import threading
import types
import unittest
from unittest import mock

EXT = Path(__file__).resolve().parents[1] / "extensions"
sys.path.insert(0, str(EXT))
import vqro_ext  # noqa: E402
from vqro_ext import VqroError  # noqa: E402


class Core:
    """The public surface the extensions use, over an in-memory workspace with the store's reference checks."""

    def __init__(self, *_, **__):
        self.entries, self.live, self.resources, self.handlers = {}, {}, {}, {}
        self.cache_lock = threading.RLock()
        self.calls, self.notes, self.next_id = [], [], 1
        self.vqro, self.name = self, "fake"

    # caches and helpers, as vqro_ext.Extension
    value = vqro_ext.Extension.value
    version = vqro_ext.Extension.version
    active_layout = vqro_ext.Extension.active_layout
    layout_settings = vqro_ext.Extension.layout_settings
    layout_folder = vqro_ext.Extension.layout_folder

    def apply(self, ops, note):
        candidate = copy.deepcopy(self.entries)
        for op in ops:
            if "expect" in op and op["expect"] != candidate.get(op["key"], {}).get("version", 0):
                raise VqroError("changed", "conflict")
            if op["op"] == "delete":
                candidate.pop(op["key"], None)
            else:
                candidate[op["key"]] = {"value": copy.deepcopy(op["value"]), "version": candidate.get(op["key"], {}).get("version", 0)+1}
        for key, e in candidate.items():
            if key.startswith("layout/"):
                missing = [v for v in vqro_ext.views_in(e["value"]) if "view/"+v not in candidate]
                if missing:
                    raise VqroError(f"{key} references missing view {missing}", "invalid")
        active = candidate.get("workspace/active")
        if active and "layout/"+active["value"]["layout"] not in candidate:
            raise VqroError("active layout missing", "invalid")
        self.entries = candidate
        self.notes.append(note)
        return {"changes": [{"key": op["key"]} for op in ops]}

    def call(self, method, params=None):
        self.calls.append((method, copy.deepcopy(params)))
        if method == "resource.spawn":
            rid = f"r{self.next_id}"; self.next_id += 1
            self.resources[rid] = {"id": rid, "kind": "terminal", "argv": ["sh"], "state": "live", **params}
            return {"outcome": "observed", "resource": dict(self.resources[rid])}
        if method == "resource.kill":
            raise AssertionError("never kills")
        return {}

    def retrying(self, fn, attempts=3):
        return fn()

    def fresh(self): pass
    def command(self, name, title=None): return lambda fn: self.handlers.setdefault(name, fn)
    def on_event(self, fn): return fn
    def publish(self, view, content): self.live[view] = copy.deepcopy(content)
    def notify(self, *_): pass
    def log(self, *_): pass
    def run(self): pass

    def contribute(self, entries, exclusive=None):
        self.apply([{"op": "set", "key": k, "value": dict(v, owner=self.name)} for k, v in entries.items()], "contribute")

    def invoke(self, name, args=None):
        self.calls.append(("command.invoke", {"name": name, "args": copy.deepcopy(args or {})}))
        return self.handlers[name](args or {})


def load(core, *names):
    """Run extensions against one shared fake core (they meet only through it)."""
    fake = types.SimpleNamespace(**{k: getattr(vqro_ext, k) for k in ("VqroError", "node_at", "replace_at", "first_tabs", "views_in")},
                                 Extension=lambda *a, **k: core)
    modules = {}
    with mock.patch.dict(sys.modules, {"vqro_ext": fake}), mock.patch.object(threading, "Thread"):
        for name in names:
            modules[name] = runpy.run_path(str(EXT / f"{name}.py"))
    return modules


def active(core):
    return core.value("workspace/active")["layout"]


def tab_titles(core, layout):
    """The resource each tab shows (its first view's)."""
    root = core.value(f"layout/{layout}")
    tabs = next(n for n in walk(root) if "tabs" in n)
    return [core.value("view/"+vqro_ext.views_in(t["node"])[0])["resource"] for t in tabs["tabs"]]


def walk(n):
    yield n
    for c in n.get("children", []):
        yield from walk(c)
    for t in n.get("tabs", []):
        yield from walk(t["node"])


class WorkspaceTabs(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.a, self.b = (os.path.join(self.dir.name, n) for n in ("alpha", "beta"))
        os.mkdir(self.a); os.mkdir(self.b)
        self.core = Core()
        # An existing main layout with the sidebar beside its tabs and a terminal split next to them.
        self.core.apply([
            {"op": "set", "key": "view/sidebar", "value": {"kind": "panel", "items": []}},
            {"op": "set", "key": "view/term-r0", "value": {"kind": "terminal", "resource": "r0"}},
            {"op": "set", "key": "view/term-side", "value": {"kind": "terminal", "resource": "r0"}},
            {"op": "set", "key": "layout/main", "value": {"split": "h", "sizes": [1, 3, 2], "children": [
                {"view": "sidebar"}, {"tabs": [{"title": "sh", "node": {"view": "term-r0"}}], "active": 0}, {"view": "term-side"}]}},
            {"op": "set", "key": "workspace/active", "value": {"layout": "main"}}], "setup")
        self.mods = load(self.core, "tabs", "panes", "workspaces", "sidebar")
        self.ws = self.mods["workspaces"]

    def tearDown(self):
        self.dir.cleanup()

    def spawns(self):
        return [p for m, p in self.core.calls if m == "resource.spawn"]

    def test_each_workspace_gets_its_own_tabs_started_in_its_folder(self):
        self.core.handlers["workspaces.open"]({"path": self.a})
        la = self.ws["layout_name"](self.a)
        self.assertEqual(active(self.core), la)
        self.assertEqual(self.spawns(), [{"cwd": self.a}])
        self.assertEqual(self.core.value(f"doc/layout/{la}"), {"cwd": self.a, "title": "alpha"})
        # The sidebar panel carries over; the main layout's terminal beside the tabs does not.
        root = self.core.value(f"layout/{la}")
        self.assertEqual(root["split"], "h"); self.assertEqual(root["children"][0], {"view": "sidebar"})
        self.assertEqual(root["sizes"], [1, 3]); self.assertNotIn("term-side", vqro_ext.views_in(root))
        # A new tab (and a split) in this workspace starts in its folder; main keeps its own tabs.
        self.core.handlers["tabs.open"]({})
        self.core.handlers["panes.split"]({"context": {"layout": la, "layout_version": self.core.version(f"layout/{la}"), "path": [1, 0]}})
        self.assertEqual([s.get("cwd") for s in self.spawns()], [self.a, self.a, self.a])
        self.assertEqual(tab_titles(self.core, "main"), ["r0"])
        # Second workspace: separate tabs; switching back restores the first one's tabs without spawning.
        self.core.handlers["workspaces.open"]({"path": self.b})
        lb = self.ws["layout_name"](self.b)
        self.assertNotEqual(la, lb); self.assertEqual(active(self.core), lb)
        self.assertEqual(len(tab_titles(self.core, lb)), 1)
        before = len(self.spawns())
        self.core.handlers["workspaces.open"]({"path": self.a})
        self.assertEqual(active(self.core), la); self.assertEqual(len(self.spawns()), before)
        self.assertEqual(len(tab_titles(self.core, la)), 2)
        # The layout name is stable for the folder, however it is written.
        self.assertEqual(self.ws["layout_name"](self.a + "/"), la)

    def test_shell_adds_a_tab_main_switches_back_and_status_names_the_workspace(self):
        self.core.handlers["workspaces.shell"]({"path": self.a})  # creates: one shell, not two
        la = self.ws["layout_name"](self.a)
        self.assertEqual(len(tab_titles(self.core, la)), 1)
        self.core.handlers["workspaces.shell"]({"path": self.a})
        self.assertEqual(len(tab_titles(self.core, la)), 2)
        self.ws["publish_status"]()
        self.assertEqual(self.core.live["workspaces.current"]["label"], "alpha")
        self.core.handlers["workspaces.main"]({})
        self.assertEqual(active(self.core), "main")
        self.assertEqual(len(self.spawns()), 2)

    def test_main_is_created_with_a_shell_when_missing(self):
        self.core.handlers["workspaces.open"]({"path": self.a})
        self.core.apply([{"op": "delete", "key": "layout/main"}], "drop main")
        self.core.handlers["workspaces.main"]({})
        self.assertEqual(active(self.core), "main")
        self.assertEqual(self.spawns()[-1], {})
        self.assertIsNone(self.core.value("doc/layout/main"))

    def test_remove_drops_the_layout_and_views_but_never_programs(self):
        self.core.handlers["workspaces.add"]({"text": self.a})
        self.core.handlers["workspaces.open"]({"path": self.a})
        la = self.ws["layout_name"](self.a)
        view = next(v for v in vqro_ext.views_in(self.core.value(f"layout/{la}")) if v.startswith("term-"))
        self.core.handlers["workspaces.remove"]({"path": self.a})
        self.assertEqual(active(self.core), "main")
        for key in (f"layout/{la}", f"doc/layout/{la}", f"view/{view}"):
            self.assertIsNone(self.core.value(key), key)
        self.assertIsNotNone(self.core.value("view/sidebar"))
        self.assertEqual(self.core.value("doc/workspaces/list"), {"items": []})
        self.assertFalse(any(m == "resource.kill" for m, _ in self.core.calls))

    def test_sidebar_lists_tabs_switches_and_works_without_workspace_data(self):
        sb = self.mods["sidebar"]
        self.core.handlers["workspaces.open"]({"path": self.a})
        self.core.handlers["tabs.open"]({})
        la = self.ws["layout_name"](self.a)
        # Without the workspaces extension's data: every layout with its tabs, titled by its settings.
        self.core.live.pop("workspaces", None)
        rows = sb["build"]()
        layouts = {r["args"]["layout"]: r for r in rows if r.get("id", "").startswith("layout:")}
        self.assertEqual(set(layouts), {"main", la})
        self.assertEqual(layouts[la]["text"], "alpha")
        self.assertEqual(layouts[la]["badges"], [{"text": "current", "style": "accent"}])
        tabs = layouts[la]["children"]
        self.assertEqual(len(tabs), 2); self.assertEqual(tabs[1]["style"], "accent")
        # Click main's tab: main becomes active, that tab shown.
        main_tab = layouts["main"]["children"][0]
        self.core.handlers[main_tab["command"]](main_tab["args"])
        self.assertEqual(active(self.core), "main")
        focus = self.core.handlers["sidebar.tab"](dict(tabs[0]["args"]))["client"]["focus"]["view"]
        self.assertEqual(active(self.core), la)
        self.assertEqual(focus, vqro_ext.views_in(next(n for n in walk(self.core.value(f"layout/{la}")) if "tabs" in n)["tabs"][0]["node"])[0])
        self.assertEqual(next(n for n in walk(self.core.value(f"layout/{la}")) if "tabs" in n)["active"], 0)
        # With workspace data: the workspace row holds its tabs; only main stays under Layouts.
        self.core.live["workspaces"] = {"data": [{"path": self.a, "name": "alpha", "layout": la}]}
        rows = sb["build"]()
        ws = next(r for r in rows if r.get("id") == "ws:"+self.a)
        self.assertEqual([r["id"] for r in ws["children"][:2]], [f"tab:{la}:0", f"tab:{la}:1"])
        self.assertEqual([r["args"]["layout"] for r in rows if r.get("id", "").startswith("layout:")], ["main"])

    def test_sidebar_toggle_applies_to_every_layout(self):
        self.core.handlers["workspaces.open"]({"path": self.a})
        la = self.ws["layout_name"](self.a)
        has = self.mods["sidebar"]["has_sidebar"]
        self.assertTrue(has(self.core.value(f"layout/{la}")))
        self.core.handlers["sidebar.toggle"]({})  # hidden everywhere; main's 3-way split keeps its other two
        self.assertFalse(has(self.core.value(f"layout/{la}"))); self.assertFalse(has(self.core.value("layout/main")))
        self.assertEqual(self.core.value("layout/main")["sizes"], [3, 2])
        self.core.handlers["sidebar.toggle"]({})  # shown everywhere again
        self.assertTrue(has(self.core.value(f"layout/{la}"))); self.assertTrue(has(self.core.value("layout/main")))

    def test_tabs_keep_working_on_main_without_workspaces(self):
        core = Core()
        mods = load(core, "tabs")
        core.handlers["tabs.open"]({})
        core.handlers["tabs.open"]({"cwd": "/given"})
        self.assertEqual(active(core), "main")
        self.assertEqual([p for m, p in core.calls if m == "resource.spawn"], [{}, {"cwd": "/given"}])
        self.assertEqual(len(tab_titles(core, "main")), 2)
        self.assertIn("tabs.open", mods["tabs"]["ext"].handlers)


if __name__ == "__main__":
    unittest.main()
