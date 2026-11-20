"""The text client's panel listing: facet/sort lines, columns and their commands (no socket, no core)."""
import os
import sys
import threading
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


class PopupAfterReply(unittest.TestCase):
    def test_a_popup_shows_what_its_command_published_even_before_the_event_arrives(self):
        """settings.open publishes fresh content, then replies with a popup; the view.content event can arrive
        after the reply (and is applied only after the command line). The popup must show the fresh content."""
        c = client({"title": "Settings", "items": [{"text": "stale row", "command": "x"}]})
        c.ws.epoch, c.ws.seq = "e", 5
        c.window = model.Window()
        fresh = {"epoch": "e", "seq": 7, "entries": {"view/p": {"version": 2, "value": {"kind": "panel", "title": "Settings", "items": []}}},
                 "live": {"p": {"items": [{"text": "fresh row", "command": "x"}]}}, "resources": []}
        calls = []

        class Core:
            def call(self, method, params=None):
                calls.append(method)
                if method == "command.invoke":
                    return {"client": {"popup": {"view": "p"}}}
                if method == "workspace.snapshot":
                    return fresh
                raise AssertionError(method)
        c.vqro, c.safe_selected, c.info = Core(), set(), {}
        lines = c.handle("run settings.open")
        self.assertTrue(any("fresh row" in l for l in lines), lines)
        self.assertFalse(any("stale row" in l for l in lines), lines)
        self.assertEqual(calls, ["command.invoke", "workspace.snapshot"])
        # The event that carried the same content arrives late: already covered by the snapshot, so ignored.
        self.assertEqual(c.ws.apply({"epoch": "e", "seq": 6, "type": "view.content", "data": {"view": "p", "content": {"items": [{"text": "stale row"}]}}}), [])
        self.assertEqual(c.ws.live["p"]["items"][0]["text"], "fresh row")

    def test_an_older_snapshot_never_replaces_a_newer_one(self):
        """A background resync's snapshot (seq 6) that returns after a command's refresh (seq 8) is dropped, so the
        popup's fresh content stays (found by the secondary lead's shadow review)."""
        started, release = threading.Event(), threading.Event()

        def snap(seq, label):
            return {"epoch": "e", "seq": seq, "entries": {"view/p": {"version": seq, "value": {"kind": "panel", "items": []}}},
                    "live": {"p": {"items": [{"text": label, "command": "x"}]}}, "resources": []}

        class Core:
            def call(self, method, params=None):
                if method == "command.invoke":
                    return {"client": {"popup": {"view": "p"}}}
                if method == "workspace.snapshot":
                    if threading.current_thread().name == "old-resync":
                        started.set()
                        self_test.assertTrue(release.wait(5))
                        return snap(6, "older resync content")
                    return snap(8, "fresh post-command content")
                if method == "events.subscribe":
                    return {}
                raise AssertionError(method)
        self_test = self
        c = client({"title": "Settings", "items": [{"text": "initial", "command": "x"}]})
        c.ws.epoch, c.ws.seq = "e", 5
        c.vqro, c.safe_selected, c.info = Core(), set(), {}
        old = threading.Thread(target=c.resync, name="old-resync")
        old.start()
        self.assertTrue(started.wait(5))
        try:
            lines = c.handle("run settings.open")
        finally:
            release.set()
        old.join(5)
        self.assertFalse(old.is_alive())
        self.assertTrue(any("fresh post-command content" in l for l in lines), lines)
        self.assertEqual((c.ws.seq, c.ws.live["p"]["items"][0]["text"]), (8, "fresh post-command content"))
        # A newer epoch (a restarted core) always replaces the cache, whatever its seq.
        c.vqro.call = lambda method, params=None: {"epoch": "f", "seq": 1, "entries": {}, "live": {}, "resources": []}
        c.load_snapshot()
        self.assertEqual((c.ws.epoch, c.ws.seq), ("f", 1))


if __name__ == "__main__":
    unittest.main()
