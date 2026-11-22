"""Close ends what it closes (human feedback batch 6): panes.close and tabs.close stop the programs they close, ask
first while something runs, and leave alone what another pane still shows or what has ended. No sockets."""
import unittest

from test_resource_views import load
from vqro_ext import VqroError


def kills(core):
    return [p["resource"] for m, p in core.calls if m == "resource.kill"]


def stops(core):
    return [a["resource"] for m, a in core.calls if m == "agents.stop"]


def mark_once(core, note):
    """Before the first write with `note`, another writer (the tabs extension marking an ended tab) changes the
    layout, so that write's expected version is stale once."""
    apply = core.apply
    def racing(ops, why, *rest, **kw):
        if why == note and not racing.done:
            racing.done = True
            apply([{"op": "set", "key": "layout/main", "value": core.value("layout/main")}], "tabs: ended")
        return apply(ops, why, *rest, **kw)
    racing.done = False
    core.apply = racing


class ClosePane(unittest.TestCase):
    def setUp(self):
        self.module, self.core = load("panes")
        self.core.resources = {
            "r1": {"id": "r1", "kind": "terminal", "argv": ["/usr/bin/zsh"], "state": "live"},
            "r3": {"id": "r3", "kind": "terminal", "argv": ["cargo", "test"], "state": "live"},
            "r4": {"id": "r4", "kind": "terminal", "argv": ["vim"], "state": "finished"},
            "r5": {"id": "r5", "kind": "agent", "name": "Pi", "state": "live", "status": {"state": "working"},
                   "data": {"agent": {"caps": {"agent.stop": {}}, "queue": [{"text": "a"}, {"text": "b"}]}}},
            "r6": {"id": "r6", "kind": "agent", "name": "Codex", "state": "live", "status": {"state": "idle"}, "data": {}},
        }
        views = {"term-r1": "r1", "term-r3": "r3", "term-r4": "r4"}
        self.core.apply([{"op": "set", "key": f"view/{v}", "value": {"kind": "terminal", "resource": r}} for v, r in views.items()]
                        + [{"op": "set", "key": "layout/main", "value": {"split": "h", "children": [
                            {"view": "term-r1"}, {"view": "term-r3"}, {"view": "term-r4"}, {"view": "agents-r5"}, {"view": "agents-r6"}]}}],
                       "fixture")

    def close(self, index, view, **args):
        ctx = {"layout": "main", "layout_version": self.core.version("layout/main"), "path": [index], "view": view}
        return self.module["close"](dict(args, context=ctx))

    def views(self):
        return [c["view"] for c in self.core.value("layout/main")["children"]]

    def test_a_plain_shell_stops_and_closes_without_asking(self):
        self.assertIsNone(self.close(0, "term-r1"))
        self.assertEqual(kills(self.core), ["r1"])
        self.assertNotIn("term-r1", self.views())

    def test_a_running_program_asks_first_then_each_choice_does_what_it_says(self):
        reply = self.close(1, "term-r3")
        self.assertEqual(reply, {"client": {"popup": {"view": "panes.close-confirm", "width": 64}}})
        self.assertEqual((kills(self.core), self.views()[1]), ([], "term-r3"), "asking changes nothing")
        popup = self.core.live["panes.close-confirm"]
        self.assertEqual(popup["title"], "Close pane?")
        self.assertEqual(popup["items"][0]["text"], "cargo is still running.")
        choices = {i["text"]: i["args"] for i in popup["items"] if "command" in i}
        self.assertEqual(list(choices), ["Close and stop it", "Keep it running, close the pane", "Cancel"])
        self.assertTrue(all("input" not in i for i in popup["items"]), "mouse-only")
        self.assertEqual(self.close(1, "term-r3", **choices["Cancel"]), {"client": {"close": True}})
        self.assertEqual((kills(self.core), self.views()[1]), ([], "term-r3"))
        self.assertEqual(self.close(1, "term-r3", **choices["Keep it running, close the pane"]), {"client": {"close": True}})
        self.assertEqual(kills(self.core), [])
        self.assertNotIn("term-r3", self.views())

    def test_stop_and_close(self):
        self.close(1, "term-r3")
        self.assertEqual(self.close(1, "term-r3", stop=True), {"client": {"close": True}})
        self.assertEqual(kills(self.core), ["r3"])
        self.assertNotIn("term-r3", self.views())

    def test_a_layout_change_from_the_stop_itself_does_not_make_the_close_conflict(self):
        # Gate-found (e2e-agent-stop): killing the program makes the tabs extension mark its tab ended, a layout write
        # between the caller's version and this close.
        # The exit is seen later: the tabs extension's write lands between this close reading the layout and writing.
        mark_once(self.core, "panes: close")
        self.assertIsNone(self.close(0, "term-r1"))
        self.assertEqual(kills(self.core), ["r1"])
        self.assertNotIn("term-r1", self.views())

    def test_an_ended_program_just_closes(self):
        self.assertIsNone(self.close(2, "term-r4"))
        self.assertEqual(kills(self.core), [])
        self.assertNotIn("term-r4", self.views())

    def test_a_program_another_pane_shows_keeps_running(self):
        self.core.apply([{"op": "set", "key": "layout/other", "value": {"view": "term-r3"}}], "fixture")
        self.assertIsNone(self.close(1, "term-r3"), "no question: it stays shown elsewhere")
        self.assertEqual(kills(self.core), [])

    def test_busy_agents_ask_in_the_agreed_words_and_stop_through_the_agents_extension(self):
        # agent-standard-coord's wording; the human's choice (a): only busy agents ask.
        reply = self.close(3, "agents-r5")
        self.assertIn("popup", reply["client"])
        popup = self.core.live["panes.close-confirm"]
        self.assertEqual(popup["items"][0]["text"], "Pi r5 is working on a reply. Closing stops it; 2 queued messages will not be sent.")
        self.assertEqual([i["text"] for i in popup["items"] if "command" in i], ["Stop and close", "Keep running, just close the pane", "Cancel"])
        self.close(3, "agents-r5", stop=True)
        self.assertEqual([a for m, a in self.core.calls if m == "agents.stop"], [{"resource": "r5", "confirm": True, "how": "auto"}])
        # The human's choice (a): an idle agent stops and closes without asking.
        self.assertIsNone(self.close(3, "agents-r6"), "after r5's pane closed, r6's is the fourth")
        self.assertEqual(stops(self.core), ["r5", "r6"])
        self.core.resources["r6"]["status"]["state"] = "blocked"
        self.core.apply([{"op": "set", "key": "layout/main", "value": {"split": "h", "children": [{"view": "term-r1"}, {"view": "agents-r6"}]}}], "fixture")
        self.close(1, "agents-r6")
        self.assertEqual(self.core.live["panes.close-confirm"]["items"][0]["text"],
                         "Codex r6 is blocked. Closing stops it. vqro will kill its terminal.", "no graceful stop declared")

    def test_a_program_that_will_not_stop_keeps_its_pane_and_says_why(self):
        def refuse(name, args=None):
            self.core.calls.append((name, args))
            raise VqroError("no graceful stop", "unsupported")
        self.core.invoke = refuse
        reply = self.close(4, "agents-r6", stop=True)
        self.assertIn("popup", reply["client"])
        self.assertIn("agents-r6", self.views())
        items = self.core.live["panes.close-confirm"]["items"]
        self.assertEqual(items[0]["text"], "Couldn't stop it: no graceful stop")
        self.assertEqual([i["text"] for i in items if "command" in i], ["Keep running, just close the pane", "Cancel"])


class CloseTab(unittest.TestCase):
    def setUp(self):
        self.module, self.core = load("tabs")
        self.core.resources = {
            "r1": {"id": "r1", "kind": "terminal", "argv": ["bash"], "state": "live"},
            "r3": {"id": "r3", "kind": "terminal", "argv": ["cargo", "test"], "state": "live"},
            "r8": {"id": "r8", "kind": "terminal", "argv": ["bash"], "state": "live"},
        }
        self.core.apply([{"op": "set", "key": f"view/term-{r}", "value": {"kind": "terminal", "resource": r}} for r in ("r1", "r3", "r8")]
                        + [{"op": "set", "key": "layout/main", "value": {"tabs": [
                            {"title": "build", "node": {"split": "h", "children": [{"view": "term-r1"}, {"view": "term-r3"}]}},
                            {"title": "edit", "node": {"view": "term-r8"}}], "active": 0}}], "fixture")

    def close(self, **args):
        ctx = {"layout": "main", "layout_version": self.core.version("layout/main"), "tab_path": [], "tab_index": 0}
        return self.module["close_tab"](dict(args, context=ctx))

    def test_one_popup_lists_what_runs_and_stop_ends_every_program(self):
        reply = self.close()
        self.assertEqual(reply, {"client": {"popup": {"view": "tabs.close-confirm", "width": 64}}})
        popup = self.core.live["tabs.close-confirm"]
        self.assertEqual(popup["title"], 'Close tab "build"?')
        self.assertEqual([i["text"] for i in popup["items"] if "command" not in i], ["cargo is still running."])
        self.assertEqual(kills(self.core), [])
        self.close(stop=True)
        self.assertEqual(sorted(kills(self.core)), ["r1", "r3"])
        self.assertEqual([t["title"] for t in self.core.value("layout/main")["tabs"]], ["edit"])

    def test_a_layout_change_from_the_stops_does_not_make_the_close_conflict(self):
        mark_once(self.core, "tabs: close")
        self.close(stop=True)
        self.assertEqual([t["title"] for t in self.core.value("layout/main")["tabs"]], ["edit"])

    def test_keep_closes_the_tab_and_leaves_its_programs_running(self):
        self.close(keep=True)
        self.assertEqual(kills(self.core), [])
        self.assertEqual([t["title"] for t in self.core.value("layout/main")["tabs"]], ["edit"])

    def test_a_tab_of_plain_shells_closes_without_asking(self):
        self.core.resources["r3"]["state"] = "finished"
        self.assertNotIn("popup", (self.close() or {}).get("client", {}))
        self.assertEqual(kills(self.core), ["r1"], "the ended program is left alone")


if __name__ == "__main__":
    unittest.main()
