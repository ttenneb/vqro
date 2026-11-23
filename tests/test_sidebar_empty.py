"""Opening sidebar needs no workspaces extension, layout or backend."""
import unittest

from test_workspace_tabs import Core, load


class EmptySidebar(unittest.TestCase):
    def core(self):
        core = Core()
        def reset(args):
            return core.apply([
                {"op": "set", "key": "view/vqro-empty", "value": {"kind": "panel", "items": [{"text": "No terminals yet."}]}},
                {"op": "set", "key": "layout/main", "value": {"tabs": [{"title": "vqro", "node": {"view": "vqro-empty"}}], "active": 0}},
                {"op": "set", "key": "workspace/active", "value": {"layout": "main"}}], "reset")
        core.handlers["vqro.layout.reset"] = reset
        load(core, "sidebar")
        return core

    def test_empty_start_uses_reset_and_shows_sidebar_without_spawn(self):
        core = self.core()
        reply = core.handlers["sidebar.toggle"]({})
        self.assertEqual(reply["client"]["layout"], "main")
        self.assertEqual(core.value("workspace/active"), {"layout": "main"})
        self.assertEqual(core.value("layout/main")["children"][0], {"view": "sidebar"})
        self.assertEqual([m for m, _ in core.calls].count("resource.spawn"), 0)
        self.assertEqual([p["name"] for m, p in core.calls if m == "command.invoke"], ["vqro.layout.reset"])
        core.handlers["sidebar.toggle"]({})
        self.assertNotIn("split", core.value("layout/main"))

    def test_existing_main_is_selected_and_shown_without_reset(self):
        for visible in (False, True):
            with self.subTest(visible=visible):
                core = self.core()
                core.apply([{"op": "set", "key": "view/user", "value": {"kind": "panel"}},
                            {"op": "set", "key": "layout/main", "value": {"split": "h", "children": [{"view": "sidebar"}, {"view": "user"}]} if visible else {"view": "user"}}], "main only")
                reply = core.handlers["sidebar.toggle"]({"context": {"layout": "deleted"}})
                self.assertEqual(reply["client"]["layout"], "main")
                self.assertEqual(core.value("workspace/active"), {"layout": "main"})
                self.assertEqual(core.value("layout/main")["children"][0], {"view": "sidebar"})
                self.assertEqual([p["name"] for m, p in core.calls if m == "command.invoke"], [])
                self.assertEqual([m for m, _ in core.calls].count("resource.spawn"), 0)
