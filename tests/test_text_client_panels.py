"""The text client's panel listing: facet/sort lines, columns and their commands (no socket, no core)."""
import os
import sys
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(ROOT, "extensions", "clients"))
sys.path.insert(1, os.path.join(ROOT, "extensions"))
import text  # noqa: E402
import vqro_client_model as model  # noqa: E402


def client(panel):
    c = text.TextClient.__new__(text.TextClient)
    c.out, c.lock, c.state, c.popup, c.focus = None, __import__("threading").RLock(), {}, None, 0
    c.ws, c.window, c.shown, c.listed, c.menu = model.Workspace(), model.Window(), None, [], []
    c.ws.entries["view/p"] = dict(panel, kind="panel")
    c.composers = {}
    return c


PANEL = {"title": "Provides", "facets": [{"key": "kind", "label": "Show"}], "sorts": [{"key": "name", "label": "name"}],
         "columns": [{"label": "kind", "width": 8}, {"label": "name", "width": "fill"}],
         "items": [{"text": "command · b", "columns": ["command", "b.cmd"], "facets": {"kind": "command"}, "sort": {"name": "b"}, "command": "x"},
                   {"text": "view · a", "columns": ["view", "a-*"], "facets": {"kind": "view"}, "sort": {"name": "a"}, "command": "x"}]}


class TextPanels(unittest.TestCase):
    def test_facets_sort_and_columns(self):
        c = client(PANEL)
        lines = c.handle("view p")
        self.assertIn("Show (kind): [All 2]*  [command 1]  [view 1]   (facet kind <value|all>)", lines)
        self.assertIn("sort: [0:given]*  [1:name]   (sort <n>)", lines)
        self.assertIn("       kind     name", lines)
        self.assertTrue(any(l.startswith("  1. command  b.cmd") for l in lines), lines)
        lines = c.handle("facet kind view")
        self.assertIn("Show (kind): [All 2]  [command 1]  [view 1]*   (facet kind <value|all>)", lines)
        self.assertEqual([l for l in lines if l.startswith("  1.")], ["  1. view     a-*"])
        c.handle("facet kind all")
        lines = c.handle("sort 1")
        self.assertEqual([l[:12] for l in lines if l[2:4] in ("1.", "2.")], ["  1. view   ", "  2. command"])
        self.assertEqual(c.handle("sort 9"), ["sort 0-1 (0: given order)"])
        self.assertEqual(c.handle("facet nope x"), ["facets here: kind"])


if __name__ == "__main__":
    unittest.main()
