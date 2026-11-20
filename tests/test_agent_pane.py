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
