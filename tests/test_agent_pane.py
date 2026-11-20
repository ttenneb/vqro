"""The agent pane's conversation from an item stream (extensions/agent_transcript.py, agents.py; no core)."""
import json
import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "extensions"))
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from agents import Agents
import agent_transcript
from agent_fixture import FakeCore


def line(record):
    return (json.dumps(record, ensure_ascii=False) + "\n").encode()


def snapshot(entities, coverage="all_retained", seq=0):
    out = line({"op": "snapshot", "phase": "begin", "epoch": "e1", "coverage": coverage})
    for (kind, eid), value in entities.items():
        out += line({"op": "put", "kind": kind, "id": eid, "rev": 1, "value": value})
    return out + line({"op": "snapshot", "phase": "end", "seq": seq})


def text(t, **extra):
    return [{"type": "text", "text": t, **extra}]


CONVERSATION = {
    ("message", "m1"): {"position": 1, "role": "user", "content": text("Why is the build red?"), "final": True,
                        "at_ms": 1_763_600_000_000, "delivery": "d:1", "origin": {"kind": "human", "principal": "undeclared"}},
    ("receipt", "d:1"): {"delivery": "d:1", "stage": "inserted"},
    ("message", "m2"): {"position": 2, "role": "assistant", "content": text("Let me look.\nFirst the CI log."), "final": True},
    ("tool", "t1"): {"position": 3, "title": "Read ci.yml", "status": "completed", "input": {"path": "ci.yml"},
                     "started_ms": 1000, "ended_ms": 1200, "locations": [{"path": "/w/ci.yml", "line": 4}]},
    ("tool", "t2"): {"position": 4, "title": "npm test", "status": "failed", "content": text("\n".join(f"l{i}" for i in range(30))),
                     "started_ms": 1200, "ended_ms": 5800},
    ("message", "m3"): {"position": 5, "role": "user", "content": text("Bump the lockfile too."), "final": True,
                        "origin": {"kind": "agent", "agent": "r7", "principal": "p:lead"}, "quoted": {"label": "alice"}},
    ("message", "m4"): {"position": 6, "role": "thought", "content": text("Config change, not code."), "final": True},
    ("message", "m5"): {"position": 7, "role": "assistant", "content": text("Fixing"), "final": False},
}


class TranscriptRows(unittest.TestCase):
    def rows(self, entities=CONVERSATION, coverage=None):
        names = agent_transcript.Names({"r2": {"name": "Pi"}, "r7": {"name": "vqro-lead"}}, "r2")
        return agent_transcript.rows({k: (1, v) for k, v in entities.items()}, names, coverage)

    def test_senders_come_from_the_recorded_origin_in_order(self):
        headers = [r["text"] for r in self.rows() if r.get("type") == "header"]
        self.assertEqual(headers[0].split(" · ")[0], "You")
        self.assertIn("in the conversation", headers[0], "the delivery's receipt stage")
        self.assertEqual(headers[1:], ["Pi", "Message from vqro-lead (agent r7)", "Thinking", "Pi · writing…"])
        texts = [r.get("text") for r in self.rows()]
        self.assertIn("quoting alice (claimed)", texts)
        self.assertLess(texts.index("Let me look."), texts.index("First the CI log."), "one row per line")

    def test_no_origin_is_never_you_and_another_human_is_named(self):
        bare = {("message", "m"): {"position": 1, "role": "user", "content": text("hi"), "final": True}}
        self.assertEqual(self.rows(bare)[0]["text"], "User")
        other = {("message", "m"): {"position": 1, "role": "user", "content": text("hi"), "final": True,
                                    "origin": {"kind": "human", "principal": "alice"}}}
        self.assertEqual(self.rows(other)[0]["text"], "Message from alice")

    def test_tool_calls_group_with_status_duration_location_and_bounded_output(self):
        group = next(r for r in self.rows() if r.get("text", "").startswith("Activity"))
        self.assertEqual(group["text"], "Activity · 2 tools · 4.8s")
        self.assertEqual(group["description"], "✓1 ✗1")
        read, test = group["children"]
        self.assertEqual((read["text"], read["description"]), ("✓ Read ci.yml", "completed · 0.2s"))
        self.assertIn("/w/ci.yml:4", [c["text"] for c in read["children"]])
        self.assertEqual((test["style"], test["children"][-1]["text"]), ("error", "… 10 more lines"))
        self.assertTrue(group["expanded"] and test["expanded"], "nothing hidden by default")

    def test_tail_coverage_says_so_and_the_budget_keeps_the_newest(self):
        self.assertIn("Earlier history isn't available", self.rows(coverage="tail")[0]["text"])
        many = {("message", f"m{i}"): {"position": i, "role": "assistant", "content": text(f"{i} " + "x" * 2000), "final": True}
                for i in range(200)}
        rows = self.rows(many)
        self.assertLessEqual(len(json.dumps(rows, ensure_ascii=False).encode()), agent_transcript.BUDGET + 200)
        self.assertIn("earlier blocks not shown", rows[0]["text"])
        self.assertTrue(rows[-1]["text"].startswith("199 "), "the newest message is kept")

    def test_text_is_control_safe_and_content_blocks_are_typed(self):
        odd = {("message", "m"): {"position": 1, "role": "assistant", "final": True,
                                  "content": [{"type": "text", "text": "a\x1b[31mb"}, {"type": "image", "mime": "image/png"}]}}
        texts = [r["text"] for r in self.rows(odd)]
        self.assertIn("a\\x1b[31mb", texts)
        self.assertIn("[image: image/png]", texts)


class ViewSettings(unittest.TestCase):
    """AS10: nothing hidden by default; each kind shown, summarised (expandable) or hidden with a count left."""

    def rows(self, **view):
        names = agent_transcript.Names({"r2": {"name": "Pi"}, "r7": {"name": "vqro-lead"}}, "r2")
        entities = dict(CONVERSATION)
        entities[("message", "m4")] = dict(entities[("message", "m4")], at_ms=1_763_600_060_000,
                                           usage={"tokens": {"output": 1200}})
        return agent_transcript.rows({k: (1, v) for k, v in entities.items()}, names, None,
                                     agent_transcript.view_settings(view))

    def find(self, rows, prefix):
        return next(r for r in rows if r.get("text", "").startswith(prefix))

    def test_defaults_show_everything(self):
        self.assertEqual(agent_transcript.view_settings(), agent_transcript.DEFAULT_VIEW)
        self.assertTrue(any(r.get("type") == "header" and r["text"].startswith("Thinking · ") for r in self.rows()))

    def test_summary_replaces_a_kind_with_a_configurable_expandable_line(self):
        rows = self.rows(thinking="summary", tools="summary")
        thinking = self.find(rows, "▸ Thinking")
        self.assertEqual(thinking["text"], "▸ Thinking · " + agent_transcript.when(1_763_600_060_000) + " · 1.2k tokens")
        self.assertFalse(thinking["expanded"])
        self.assertIn("Config change, not code.", [c.get("text") for c in thinking["children"]], "one expand away")
        tools = self.find(rows, "▸ Activity")
        self.assertEqual((tools["text"], tools["expanded"]), ("▸ Activity · 4.8s · 2 tools", False))
        lines = self.rows(thinking="summary", summary=["lines", "words"])
        self.assertEqual(self.find(lines, "▸ Thinking")["text"], "▸ Thinking · 1 lines · Config change, not code.…")

    def test_hide_leaves_a_count_and_senders_can_be_folded(self):
        rows = self.rows(thinking="hide", tools="hide")
        texts = [r.get("text") for r in rows]
        self.assertIn("· hidden: 2 tools", texts)
        self.assertIn("· hidden: 1 thinking", texts)
        self.assertFalse(any(t and t.startswith("Activity") for t in texts))
        folded = self.find(self.rows(hidden_senders=["agent:r7"]), "▸ Message from vqro-lead")
        self.assertFalse(folded["expanded"])

    def test_compact_collapses_tools_folds_long_messages_and_drops_repeat_times(self):
        names = agent_transcript.Names({}, "r2")
        long = {("message", f"m{i}"): {"position": i, "role": "user", "at_ms": 1_763_600_000_000, "final": True,
                                       "origin": {"kind": "human"}, "content": text("\n".join(f"line {n}" for n in range(60)))}
                for i in range(2)}
        rows = agent_transcript.rows({k: (1, v) for k, v in long.items()}, names, None, agent_transcript.view_settings({"density": "compact"}))
        headers = [r["text"] for r in rows if r.get("type") == "header"]
        self.assertEqual(headers, ["You · " + agent_transcript.when(1_763_600_000_000), "You"], "time only when the sender changes")
        fold = next(r for r in rows if r.get("text") == "… 20 more lines")
        self.assertEqual((fold["expanded"], len(fold["children"])), (False, 20))

    def test_invalid_settings_fall_back(self):
        view = agent_transcript.view_settings({"thinking": "explode", "summary": ["time", "bogus"], "density": 3})
        self.assertEqual((view["thinking"], view["summary"], view["density"]), ("show", ["time"], "detailed"))


class FormattingAndFacts(unittest.TestCase):
    def test_only_blocks_marked_markdown_are_formatted_and_they_stay_whole(self):
        names = agent_transcript.Names({}, "r2")
        entities = {("message", "m"): (1, {"position": 1, "role": "assistant", "final": True, "content": [
            {"type": "text", "text": "# Plan\n- one\n- two", "format": "markdown"}, {"type": "text", "text": "# not\nmarkdown"}]})}
        rows = agent_transcript.rows(entities, names)
        self.assertEqual(rows[1], {"type": "text", "wrap": True, "focusable": True, "format": "markdown", "text": "# Plan\n- one\n- two"})
        self.assertEqual([r["text"] for r in rows[2:]], ["# not", "markdown"], "plain is the default")
        self.assertNotIn("format", rows[2])
        big = agent_transcript.markdown_rows("\n".join("x" * 1000 for _ in range(200)))
        self.assertTrue(len(big) > 1 and all(len(r["text"].encode()) <= agent_transcript.MARKDOWN_ROW_BYTES for r in big))

    def test_the_footer_shows_only_reported_facts(self):
        footer = agent_transcript.facts_footer({
            "session": {"cwd": "~/Projects/site", "model": {"id": "claude-opus-5-5", "effort": "medium"}},
            "usage": {"context": {"used": 73_000, "window": 1_000_000}, "tokens": {"input": 234, "output": 37_000},
                      "cost": {"amount": 5.217, "currency": "USD", "basis": "reported", "plan": "sub"}}}, "Pi r32")
        self.assertEqual(footer["text"], "~/Projects/site · Pi r32 · claude-opus-5-5 (medium) · ctx 7% 73k/1.0M · ↑234 ↓37k · $5.22 (sub)")
        self.assertEqual(footer["pin"], "bottom")
        bare = agent_transcript.facts_footer({"usage": {"context": {"window": 200_000},
                                                        "cost": {"amount": 1, "currency": "EUR", "basis": "estimated"}}}, "Codex")
        self.assertEqual(bare["text"], "Codex · ctx ?/200k · ~1.00 EUR", "unknown occupancy is never derived")


class AgentPane(unittest.TestCase):
    def setUp(self):
        self.core = FakeCore()
        self.core.providers["items"] = {"name": "items", "capabilities": {"items.stream": {"entities": 64, "entity_bytes": 65536}}}
        self.core.resources["r2"] = {"id": "r2", "kind": "agent", "provider": "items", "name": "Pi", "state": "live",
                                     "available": True, "data": {}}
        self.core.resources["r7"] = {"id": "r7", "kind": "agent", "provider": "other", "name": "vqro-lead", "state": "live",
                                     "available": True, "data": {}}
        self.agents = Agents(self.core)
        self.agents.flush_conversations()

    def attaches(self):
        return [p for m, p in self.core.calls if m == "resource.attach"]

    def feed(self, data):
        stream = self.agents.conversations["r2"].stream
        self.core.sinks[stream].data(data)
        self.agents.flush_conversations()

    def texts(self):
        return [r.get("text") for r in self.core.live["agents-r2"]["items"]]

    def test_an_agent_with_an_item_stream_shows_its_conversation_others_keep_todays_panel(self):
        self.assertEqual(self.attaches(), [{"resource": "r2", "items": {}}], "only the agent whose runtime streams items")
        self.assertIn("Loading the conversation…", self.texts())
        self.feed(snapshot(CONVERSATION))
        content = self.core.live["agents-r2"]
        self.assertTrue(content["follow"], "the pane follows new output")
        self.assertIn("Message from vqro-lead (agent r7)", self.texts())
        self.assertNotIn("follow", self.core.live["agents-r7"])

    def test_streamed_text_updates_the_pane_and_lost_records_reattach_from_the_cursor(self):
        self.feed(snapshot(CONVERSATION, seq=4))
        self.feed(line({"op": "append", "seq": 5, "kind": "message", "id": "m5", "base": 1, "rev": 2,
                        "field": "/content/0/text", "text": " both now"}))
        self.assertIn("Fixing both now", self.texts())
        old = self.agents.conversations["r2"].stream
        self.feed(line({"op": "append", "seq": 9, "kind": "message", "id": "m5", "base": 2, "rev": 3,
                        "field": "/content/0/text", "text": "!"}))  # seq 6–8 never arrived
        self.assertIn(("stream.close", {"stream": old}), self.core.calls)
        self.assertEqual(self.attaches()[-1], {"resource": "r2", "items": {}}, "after a loss: a fresh snapshot")
        self.feed(snapshot({("message", "m5"): {"position": 7, "role": "assistant", "content": text("Fixed."), "final": True}}, seq=9))
        self.assertIn("Fixed.", self.texts())
        self.assertNotIn("Fixing both now!", self.texts())

    def test_a_closed_stream_reattaches_and_an_unavailable_agent_keeps_its_last_state(self):
        self.feed(snapshot(CONVERSATION, seq=4))
        stream = self.agents.conversations["r2"].stream
        self.core.sinks.pop(stream).ended("closed", {"reason": "provider_disconnected"})
        self.agents.flush_conversations()
        self.assertEqual(self.attaches()[-1]["items"], {"after": {"epoch": "e1", "seq": 4}})
        self.core.resources["r2"]["available"] = False
        self.agents.event("resource", {"id": "r2"})
        self.agents.flush_conversations()
        self.assertIsNone(self.agents.conversations["r2"].stream)
        self.assertIn("Message from vqro-lead (agent r7)", self.texts(), "last known conversation still shown")
        del self.core.resources["r2"]
        self.agents.event("resource.removed", {"id": "r2"})
        self.assertNotIn("r2", self.agents.conversations)

    def test_view_chips_write_this_panes_settings_and_make_default_copies_them(self):
        self.feed(snapshot(CONVERSATION))
        toolbar = self.core.live["agents-r2"]["toolbar"][0]["chips"]
        thinking = next(c for c in toolbar if c["text"] == "Thinking: show")
        self.core.handlers["agents.view"](dict(thinking["args"], value="hide"))
        self.assertEqual(self.core.value("doc/agents/view/r2"), {"thinking": "hide"})
        senders = next(c for c in toolbar if c["text"] == "Senders: all")
        self.assertIn("agent:r7", [o["value"] for o in senders["options"]])
        self.core.handlers["agents.view"](dict(senders["args"], value="agent:r7"))
        self.assertEqual(self.core.value("doc/agents/view/r2")["hidden_senders"], ["agent:r7"])
        self.agents.event("entries", {"changes": [{"key": "doc/agents/view/r2"}]})
        self.assertIn("· hidden: 1 thinking", self.texts())
        self.core.handlers["agents.view"]({"resource": "r2", "set": "defaults"})
        self.assertEqual(self.core.value("doc/settings/agents")["view"]["thinking"], "hide")
        self.assertNotIn("hidden_senders", self.core.value("doc/settings/agents")["view"], "senders are per pane")
        with self.assertRaises(Exception):
            self.core.handlers["agents.view"]({"resource": "r2", "set": "thinking", "value": "explode"})

    def make_v0(self, state="idle", **facts):
        self.core.providers["items"]["capabilities"].update({"agent.send": {"modes": {"start": {}, "settle": {}, "boundary": {}}},
                                                             "agent.respond": {}, "agent.queue": {}})
        self.core.resources["r2"]["data"] = {"agent": {"v": 0, "state": state, "lifecycle": "live", "epoch": "e1", "caps": {
            "rev": 2, "agent.send": {"modes": {"start": {}, "settle": {}, "boundary": {}}}, "agent.respond": {},
            "agent.queue": {"actions": ["list", "clear"]}}, **facts}}
        self.agents.event("resource", {"id": "r2"})
        self.feed(snapshot(CONVERSATION))
        return self.core.live["agents-r2"]["items"]

    def test_the_composer_offers_auto_and_the_declared_modes_and_sends(self):
        items = self.make_v0()
        composer = next(r for r in items if r.get("type") == "composer")
        self.assertEqual((composer["pin"], composer["autofocus"], composer["submit"], composer["args"]),
                         ("bottom", True, "Enter", {"resource": "r2", "caps_rev": 2}))
        self.assertEqual([c["value"] for c in composer["controls"][0]["chips"]], ["auto", "boundary", "settle"], "steer before follow-up")
        self.assertEqual(composer["fallback"]["multiline"], True)
        self.assertEqual(items[-1]["pin"], "bottom", "the facts line comes last")
        self.core.handlers["agents.send"](dict(composer["args"], mode="auto", text="go"))
        sent = [p for m, p in self.core.calls if m == "resource.invoke"][-1]["args"]
        self.assertEqual((sent["mode"], sent["content"]), ("start", [{"type": "text", "text": "go"}]))

    def test_auto_on_a_busy_agent_asks_and_sends_nothing(self):
        items = self.make_v0(state="running")
        composer = next(r for r in items if r.get("type") == "composer")
        before = len([1 for m, _ in self.core.calls if m == "resource.invoke"])
        reply = self.core.handlers["agents.send"](dict(composer["args"], mode="auto", text="go"))
        self.assertEqual(reply["client"]["popup"]["view"], "agents-send-r2")
        self.assertEqual(len([1 for m, _ in self.core.calls if m == "resource.invoke"]), before)

    def test_the_waiting_line_and_the_runtimes_queue_are_pinned(self):
        items = self.make_v0(requests=[{"request": "q1", "revision": 1, "method": "confirm", "title": "Run git push?", "params": {}}],
                             queue=[{"mode": "settle", "preview": "bump the lockfile"}])
        waiting = next(r for r in items if r.get("id") == "waiting")
        self.assertEqual((waiting["command"], waiting["pin"]), ("agents.answer", "bottom"))
        self.assertIn("Run git push?", waiting["text"])
        queue = next(r for r in items if r.get("id") == "queue")
        self.assertEqual(queue["text"], "Messages · 1 held by Pi")
        self.assertEqual(queue["menu"][0]["command"], "agents.clear_queue")

    def test_a_failed_attach_is_retried_later_not_in_a_loop(self):
        self.core.fail_attach = True
        self.core.resources["r3"] = dict(self.core.resources["r2"], id="r3")
        self.agents.event("resource", {"id": "r3"})
        before = len(self.attaches())
        self.agents.flush_conversations()
        self.assertEqual(len(self.attaches()), before + 1)
        self.agents.flush_conversations()
        self.assertEqual(len(self.attaches()), before + 1, "the retry waits for its timer")


if __name__ == "__main__":
    unittest.main()
