"""Sidebar coalescing without sockets, processes or background threads."""
import copy
import os
from pathlib import Path
import runpy
import sys
import threading
import types
import unittest
from unittest import mock

EXT = Path(os.environ.get("VQRO_TEST_EXTENSIONS_DIR", Path(__file__).resolve().parents[1] / "extensions"))


class CacheLock:
    held = False
    def __enter__(self): self.held = True
    def __exit__(self, *_): self.held = False


class Extension:
    def __init__(self, *_, **__):
        self.entries, self.live, self.providers = {}, {}, {}
        self.resources = {"r1": {"argv": ["sh"], "state": "live", "status": {"state": "idle"}}}
        self.cache_lock, self.published, self.logs = CacheLock(), [], []
        self.hook, self.fail = None, False
    def value(self, *_): return None
    def on_event(self, fn): return fn
    def command(self, *_): return lambda fn: fn
    def contribute(self, *_): pass
    def run(self): pass
    def publish(self, name, value):
        assert not self.cache_lock.held, "never hold the cache lock across publication"
        if self.fail:
            self.fail = False
            raise RuntimeError("fixture failure")
        self.published.append((name, copy.deepcopy(value)))
        if self.hook: self.hook(name)
    def log(self, *args): self.logs.append(args)


def load():
    sys.path.insert(0, str(EXT))
    from vqro_ext import views_in
    fake = types.SimpleNamespace(Extension=Extension, VqroError=RuntimeError, views_in=views_in)
    with mock.patch.dict(sys.modules, {"vqro_ext": fake}), mock.patch.object(threading, "Thread"):
        return runpy.run_path(str(EXT / "sidebar.py"))


class SidebarTests(unittest.TestCase):
    def test_tier1_status_shows_only_where_the_terminal_reports_none(self):
        module = load(); ext = module["ext"]
        ext.resources = {
            "r1": {"id": "r1", "kind": "terminal", "argv": ["claude"], "state": "live"},
            "r3": {"id": "r3", "kind": "terminal", "argv": ["pi"], "state": "live", "status": {"state": "idle"}}}
        ext.live["agents.terminal_status"] = {"data": {
            "r1": {"app": "claude", "state": "requires_action", "seq": 5, "status": {"state": "blocked", "app": "claude"}},
            "r3": {"app": "pi", "state": "running", "seq": 9, "status": {"state": "working", "app": "pi"}}}}
        rows = module["build"]()
        row = lambda rid: next(x for x in rows if x.get("args", {}).get("resource") == rid)
        self.assertIn({"text": "blocked", "style": "warn"}, row("r1")["badges"])
        self.assertIn({"text": "idle", "style": None}, row("r3")["badges"], "the terminal's own OSC 7501 status wins")

    def test_rows_use_the_meaning_roles(self):
        # Colour batch 3: working is `info`, blocked `warn`, a live agent's name `agent`; a plain shell stays plain.
        module = load(); ext = module["ext"]
        ext.resources = {
            "r1": {"id": "r1", "kind": "terminal", "argv": ["zsh"], "state": "live"},
            "r2": {"id": "r2", "kind": "terminal", "argv": ["pi"], "state": "live", "status": {"state": "working"}},
            "r3": {"id": "r3", "kind": "terminal", "argv": ["pi"], "state": "exited", "status": {"state": "done"}}}
        ext.live["agents.terminal_status"] = {"data": {}}
        rows = module["build"]()
        row = lambda rid: next(x for x in rows if x.get("args", {}).get("resource") == rid)
        self.assertEqual((row("r1")["style"], row("r1")["icon_style"]), (None, "muted"))
        self.assertEqual((row("r2")["style"], row("r2")["icon_style"]), ("agent", "info"))
        self.assertIn({"text": "working", "style": "info"}, row("r2")["badges"])
        self.assertEqual(row("r3")["style"], "dim", "an ended agent is dim, not coloured")
        self.assertEqual(module["git_badges"]({"branch": "main", "dirty": 3, "ahead": 2}),
                         [{"text": "main"}, {"text": "±3", "style": "changed"}, {"text": "↑2", "style": "info"}])

    def test_agents_group_their_bridge_and_ended_items_are_marked(self):
        module = load(); ext = module["ext"]
        ext.resources = {
            "r1": {"id": "r1", "kind": "terminal", "argv": ["python3"], "state": "live"},
            "r2": {"id": "r2", "kind": "agent", "name": "Other", "host": "r1", "state": "live"},
            "r3": {"id": "r3", "kind": "terminal", "argv": ["sh"], "state": "finished"}}
        rows = module["build"]()
        agent = next(row for row in rows if row.get("args", {}).get("resource") == "r2")
        self.assertEqual(agent["children"][0]["text"], "Other r2 bridge")
        self.assertFalse(any(row.get("args", {}).get("resource") == "r1" for row in rows))
        # Ended terminals no tab shows fold into one "N ended" row; opened by hand, it lists them.
        fold = next(row for row in rows if row.get("id") == "ended:stray")
        self.assertEqual((fold["text"], fold["children"], fold["expanded"]), ("1 ended", [], False))
        self.assertFalse(any(row.get("args", {}).get("resource") == "r3" for row in rows))
        module["ended_cmd"]({"id": "ended:stray"})
        fold = next(row for row in module["build"]() if row.get("id") == "ended:stray")
        dead = next(row for row in fold["children"] if row.get("args", {}).get("resource") == "r3")
        self.assertIn({"text": "ended", "style": "dim"}, dead["badges"])
        self.assertNotIn("_ended", dead, "internal marks are not published")
        module["ended_cmd"]({"id": "ended:stray"})
        with mock.patch.object(ext, "value", return_value={"hide_ended": True}):
            self.assertFalse(any(row.get("args", {}).get("resource") == "r3" for row in module["build"]()))

    def test_agent_rows_offer_settings_only_when_the_agent_declares_them(self):
        module = load()
        plain = {"id": "r2", "kind": "agent", "name": "A", "state": "live", "data": {}}
        declared = dict(plain, data={"agent": {"v": 0, "config": [{"id": "model", "type": "select", "value": "x", "rev": 1}], "commands": []}})
        menu = lambda r: [m["command"] for m in module["term_row"]("r2", r)["menu"]]
        self.assertNotIn("agents.config", menu(plain))
        self.assertIn("agents.config", menu(declared))
        self.assertNotIn("agents.config", menu(dict(declared, state="finished")))

    def test_agent_rows_offer_stop_and_say_why_they_are_unavailable(self):
        module = load(); ext = module["ext"]
        ext.resources = {
            "r1": {"id": "r1", "kind": "terminal", "argv": ["python3"], "state": "live", "provider": "herdr"},
            "r2": {"id": "r2", "kind": "agent", "name": "Other", "host": "r1", "state": "live", "available": False,
                   "provider": "pi-1"}}
        herdr = {"herdr": {"capabilities": {"terminal.kill": {}}}}
        ext.providers = {"pi-1": {"connected": False, "since": 1_800_000_000_000}, **herdr}
        agent = next(row for row in module["build"]() if row.get("args", {}).get("resource") == "r2")
        self.assertIn({"label": "Stop agent…", "command": "agents.stop", "args": {"resource": "r2"}}, agent["menu"])
        self.assertIn({"label": "Forget agent", "command": "agents.forget", "args": {"resource": "r2"}}, agent["menu"])
        self.assertEqual(len(agent["badges"]), 1)
        self.assertTrue(agent["badges"][0]["text"].startswith("unknown since "), agent["badges"])
        ext.providers = herdr
        agent = next(row for row in module["build"]() if row.get("args", {}).get("resource") == "r2")
        self.assertEqual(agent["badges"], [{"text": "unknown", "style": "warn"}])
        ext.providers = {}  # the host's provider declares no terminal.kill: no Stop
        agent = next(row for row in module["build"]() if row.get("args", {}).get("resource") == "r2")
        self.assertNotIn("agents.stop", str(agent["menu"]))
        del ext.resources["r2"]["host"]  # hostless: no Stop, Forget remains
        agent = next(row for row in module["build"]() if row.get("args", {}).get("resource") == "r2")
        self.assertEqual([item["label"] for item in agent["menu"]], ["Show", "Forget agent"])
        ext.resources["r2"]["host"] = "r1"
        ext.resources["r1"]["state"] = "finished"
        agent = next(row for row in module["build"]() if row.get("args", {}).get("resource") == "r2")
        self.assertEqual(agent["badges"], [{"text": "ended", "style": "dim"}])

    def scale_resources(self):
        resources = {f"r{i}": {"id": f"r{i}", "kind": "terminal", "argv": ["sh"], "state": "live",
                               "status": {"state": "idle"}} for i in range(600)}
        resources.update({f"r{600+i}": {"id": f"r{600+i}", "kind": "agent", "name": "Agent", "host": f"r{400+i}",
                                       "state": "live", "status": {"state": "idle"}} for i in range(200)})
        return resources

    def test_scale_sidebar_is_bounded_and_off_page_changes_do_not_republish_it(self):
        import json
        module = load(); ext = module["ext"]
        ext.resources = self.scale_resources()
        ext.published.clear(); module["refresh"]()
        content = next(value for name, value in ext.published if name == "sidebar")
        self.assertLess(len(json.dumps(content).encode()), 65536, "one status change must not broadcast every resource")
        ext.published.clear()
        ext.resources["r300"]["status"] = {"state": "error"}
        module["refresh"]()
        self.assertFalse(any(name == "sidebar" for name, _ in ext.published), "off-page detail does not invalidate the visible page")

    def test_pagination_keeps_every_resource_and_bridge_reachable_with_ancestor_context(self):
        import json
        module = load()
        kids = [{"id": f"term:r{i}", "text": f"resource {i}"} for i in range(150)]
        kids[70]["children"] = [{"id": "term:bridge", "text": "Bridge terminal"}]
        rows = [{"type": "header", "text": "Workspaces"}, {"id": "ws:root", "text": "Root", "children": kids}]
        self.assertEqual(module["paginate"](rows, 0)[1:4], (0, 1, 151), "a typical tree is one page: no pager")
        budget = 2000  # bytes: forces several pages
        _, _, pages, total = module["paginate"](rows, 0, budget)
        self.assertGreater(pages, 2)
        found = set()
        for page in range(pages):
            result, selected, _, _ = module["paginate"](rows, page, budget)
            self.assertEqual(selected, page)
            self.assertEqual(result[0]["text"], "Workspaces")
            self.assertEqual(result[1]["id"], "ws:root")
            self.assertLess(module["encoded"](result), budget + 200, "a page is about the budget, plus its ancestors")
            for row in result[1]["children"]:
                found.add(row["id"])
                found.update(child["id"] for child in row.get("children", []))
        self.assertEqual(found, {f"term:r{i}" for i in range(150)} | {"term:bridge"})
        self.assertEqual(module["paginate"](rows[:1], 99)[1:3], (0, 1), "shrinking inventory clamps the page")

    def test_off_page_blocked_state_still_updates_the_global_status_segment(self):
        module = load(); ext = module["ext"]
        ext.resources = self.scale_resources(); module["refresh"](); ext.published.clear()
        ext.resources["r300"]["status"] = {"state": "blocked"}
        module["refresh"]()
        self.assertEqual(ext.published, [("sidebar.status", {"label": "1 blocked", "icon": module["ICON"]["blocked"], "style": "warn"})])

    def test_status_burst_only_schedules_one_latest_publish(self):
        module = load(); ext = module["ext"]; ext.published.clear()
        for _ in range(500):
            ext.resources["r1"]["status"] = {"state": "working"}
            module["on_event"]("resource", {"id": "r1"})
        self.assertTrue(module["refresh_pending"].is_set())
        self.assertEqual(ext.published, [])
        module["refresh_pending"].clear(); module["refresh"]()
        self.assertEqual([name for name, _ in ext.published], ["sidebar"])
        module["refresh"]()
        self.assertEqual(len(ext.published), 1, "unchanged content is not republished")

    def test_event_during_publish_is_not_lost(self):
        module = load(); ext = module["ext"]; ext.published.clear()
        pending = module["refresh_pending"]
        ext.resources["r1"]["status"] = {"state": "working"}; pending.set()
        def during_publish(name):
            if name == "sidebar" and ext.resources["r1"]["status"]["state"] == "working":
                ext.resources["r1"]["status"] = {"state": "blocked"}; pending.set()
        ext.hook = during_publish
        calls = []
        class Done(Exception): pass
        def wait():
            if len(calls) == 2: raise Done()
            self.assertTrue(pending.is_set()); calls.append(True)
        with mock.patch.object(pending, "wait", side_effect=wait), mock.patch.object(module["time"], "sleep"):
            with self.assertRaises(Done): module["refresh_worker"]()
        self.assertEqual(sum(name == "sidebar" for name, _ in ext.published), 2)
        self.assertTrue(any(name == "sidebar.status" and value["label"] == "1 blocked" for name, value in ext.published))

    def test_failed_publication_does_not_mark_content_as_published(self):
        module = load(); ext = module["ext"]; ext.published.clear()
        ext.resources["r1"]["status"] = {"state": "working"}; ext.fail = True
        with self.assertRaisesRegex(RuntimeError, "fixture failure"): module["refresh"]()
        module["refresh"]()
        self.assertEqual([name for name, _ in ext.published], ["sidebar"])


if __name__ == "__main__": unittest.main()
