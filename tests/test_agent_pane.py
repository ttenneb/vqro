"""The agent pane's conversation from an item stream (extensions/agent_transcript.py, agents.py; no core)."""
import json
import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "extensions"))
sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from agents import Agents
import agents as agents_module
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
    ("tool", "t1"): {"position": 3, "title": "read", "status": "completed", "input": {"path": "ci.yml"},
                     "started_ms": 1000, "ended_ms": 1200, "locations": [{"path": "/w/ci.yml", "line": 4}]},
    ("tool", "t2"): {"position": 4, "title": "bash", "status": "failed", "input": {"command": "npm test", "timeout": 10000},
                     "content": text("\n".join(f"line {i} of the test log" for i in range(30))),
                     "started_ms": 1200, "ended_ms": 5800},
    ("message", "m3"): {"position": 5, "role": "user", "content": text("Bump the lockfile too."), "final": True,
                        "origin": {"kind": "agent", "agent": "r7", "principal": "p:lead"}, "quoted": {"label": "alice"}},
    ("message", "m4"): {"position": 6, "role": "thought", "content": text("Config change, not code."), "final": True},
    ("message", "m5"): {"position": 7, "role": "assistant", "content": text("Fixing"), "final": False},
}


def base(rows, features=()):
    """What a client with only `features` shows (core-protocol "Fallbacks"): a row needing a missing feature becomes
    its fallback (a row or a list of rows)."""
    out = []
    for row in rows:
        if any(f not in features for f in row.get("requires", [])) and "fallback" in row:
            out += base(row["fallback"] if isinstance(row["fallback"], list) else [row["fallback"]], features)
        else:
            out.append(row)
    return out


class TranscriptRows(unittest.TestCase):
    def rows(self, entities=CONVERSATION, coverage=None):
        names = agent_transcript.Names({"r2": {"name": "Pi"}, "r7": {"name": "vqro-lead", "kind": "agent", "principal": "p:lead"}}, "r2")
        return base(agent_transcript.rows({k: (1, v) for k, v in entities.items()}, names, coverage))

    def test_senders_come_from_the_recorded_origin_in_order(self):
        headers = [r["text"] for r in self.rows() if str(r.get("id", "")).startswith("m:")]
        self.assertEqual(headers[0].split(" · ")[0], "Human (undeclared)", "as recorded, never inferred to be the viewer")
        self.assertIn("in the conversation", headers[0], "the delivery's receipt stage")
        self.assertEqual(headers[1:], ["Pi", "Message from vqro-lead (agent r7)", "Pi · writing…"], "thinking has no label of its own")
        self.assertIn("┊ Config change, not code.", [r.get("text") for r in self.rows()])
        texts = [r.get("text") for r in self.rows()]
        self.assertIn("quoting alice (claimed)", texts)
        self.assertLess(texts.index("Let me look."), texts.index("First the CI log."), "one row per line")

    def test_you_only_for_the_owner_principal_and_the_actor_is_shown(self):
        names = agent_transcript.Names({}, "r2", owner="p:owner")
        own = {"kind": "human", "principal": "p:owner"}
        via = {"kind": "human", "principal": "undeclared", "actor": "agents"}
        self.assertEqual(names.sender({"role": "user", "origin": own})[0], "You")
        self.assertEqual(names.sender({"role": "user", "origin": via})[0], "Human (via agents UI, undeclared)")

    def test_senders_are_named_from_the_core_recorded_principal(self):
        """C2: the sender's principal (core-recorded) names it via the resource its launcher tagged; claims say so."""
        names = agent_transcript.Names({"r7": {"name": "vqro-lead", "kind": "agent", "principal": "p:lead"},
                                        "r8": {"name": "codex", "kind": "agent"}}, "r2", owner="me")
        sender = lambda origin: names.sender({"role": "user", "origin": origin})[0]
        self.assertEqual(sender({"kind": "human", "principal": "me"}), "You")
        self.assertEqual(sender({"kind": "agent", "principal": "p:lead"}), "Message from vqro-lead (agent r7)")
        self.assertEqual(sender({"kind": "agent", "principal": "undeclared", "agent": "r8"}), "Message from codex (claimed agent r8)")
        self.assertEqual(sender({"kind": "human", "principal": "alice"}), "Message from alice")
        self.assertEqual(sender({"kind": "agent", "principal": "p:lead", "agent": "r8"}), "Message from vqro-lead (agent r7)",
                         "the recorded principal wins over a claimed agent id")

    def test_a_shared_launcher_tag_names_nobody_and_no_owner_means_no_you(self):
        """Review P2-2 and P2-1: an ambiguous tag shows the principal; without a valid owner nobody is "You"."""
        names = agent_transcript.Names({"r7": {"name": "a", "kind": "agent", "principal": "p:host"},
                                        "r8": {"name": "b", "kind": "agent", "principal": "p:host"},
                                        "r9": {"name": "term", "kind": "terminal", "principal": "p:solo"},
                                        "r10": {"name": "solo", "kind": "agent", "principal": "p:solo"}}, "r2", owner=None)
        sender = lambda origin: names.sender({"role": "user", "origin": origin})[0]
        self.assertEqual(sender({"kind": "agent", "principal": "p:host"}), "Message from p:host (claimed agent)")
        self.assertEqual(sender({"kind": "agent", "principal": "p:solo"}), "Message from solo (agent r10)", "an agent over its terminal")
        self.assertEqual(sender({"kind": "human", "principal": "owner"}), "Message from owner")

    def test_incomplete_coverage_is_always_disclosed_even_when_empty(self):
        """#67 (vqro-lead-secondary): state_only and an empty tail are never shown as complete ("No messages yet")."""
        names = agent_transcript.Names({}, "r2")
        self.assertEqual([r["text"] for r in agent_transcript.rows({}, names, "state_only")],
                         ["The agent sent only its current state, not its conversation."])
        self.assertEqual([r["text"] for r in agent_transcript.rows({}, names, "tail")], ["No messages in the recent history this agent reported."])
        self.assertEqual(agent_transcript.rows({}, names, "all_retained"), [], "complete and empty: the pane says No messages yet")

    def test_a_finished_empty_message_shows_no_bare_label(self):
        empty = {("message", "s1"): {"position": 1, "role": "system", "content": [], "final": True},
                 ("message", "m1"): {"position": 2, "role": "assistant", "content": [], "final": False}}
        texts = [r.get("text") for r in self.rows(empty)]
        self.assertNotIn("System", texts)
        sent = {**empty, ("message", "u1"): {"position": 3, "role": "user", "content": [], "final": True, "delivery": "d:1"},
                ("receipt", "d:1"): {"delivery": "d:1", "stage": "inserted"}}
        self.assertTrue(any("in the conversation" in (t or "") for t in [r.get("text") for r in self.rows(sent)]),
                        "an empty message with a delivery receipt keeps its line")
        self.assertIn("Pi · writing…", texts, "a message still arriving keeps its line")

    def test_no_origin_is_never_you_and_another_human_is_named(self):
        bare = {("message", "m"): {"position": 1, "role": "user", "content": text("hi"), "final": True}}
        self.assertEqual(self.rows(bare)[0]["text"], "User")
        other = {("message", "m"): {"position": 1, "role": "user", "content": text("hi"), "final": True,
                                    "origin": {"kind": "human", "principal": "alice"}}}
        self.assertEqual(self.rows(other)[0]["text"], "Message from alice")

    def test_tool_calls_are_one_line_each_under_one_collapsed_activity_line(self):
        """Redesign (mockup rev 3, reply-13): Activity collapsed, one line per tool, readable arguments, output cut."""
        group = next(r for r in self.rows() if r.get("text", "").startswith("Activity"))
        self.assertEqual((group["text"], group["expanded"]), ("Activity · 2 tools · 4.8s · 1 failed", False))
        self.assertEqual([b["text"] for b in group["badges"]], ["✓ read", "✗ bash"])
        read, test = group["children"]
        self.assertEqual((read["text"], [b["text"] for b in read["badges"]], read["expanded"]), ("✓ read   ci.yml", ["0.2s"], False))
        self.assertIn("/w/ci.yml:4", [c["text"] for c in read["children"]])
        self.assertEqual((test["text"], test["style"], test["description"]), ("✗ bash   npm test", "error", "line 0 of the test log"), "a failure says why")
        self.assertEqual(agent_transcript.tool_row("f", {"title": "read", "status": "failed"})["description"], "failed")
        texts = [c["text"] for c in test["children"]]
        self.assertEqual(texts[0], "timeout 10000", "other arguments as name value, never JSON")
        self.assertEqual(texts[-1], "… 18 more lines", "output cut at 12 lines")
        self.assertEqual(len(test["children"][-1]["children"]), 18)

    def test_a_file_a_tool_names_opens_in_an_editor_only_from_its_menu(self):
        """Step 5 without row actions yet: the tool row's menu offers links.open for the files it names (AS10e)."""
        read = agent_transcript.tool_row("r", {"title": "read", "status": "completed", "input": {"path": "src/a.rs", "offset": 40, "limit": 10},
                                               "locations": [{"path": "/w/b.rs", "line": 4}]}, cwd="/w")
        opens = [m for m in read["menu"] if m["command"] == "links.open"]
        self.assertEqual([m["args"]["link"] for m in opens],
                         [{"kind": "path", "text": "/w/b.rs", "line": 4}, {"kind": "path", "text": "/w/src/a.rs", "line": 40}])
        self.assertEqual(read["menu"][-1], {"label": "Copy input", "command": "client.copy"})
        self.assertEqual(agent_transcript.tool_row("e", {"title": "edit", "input": {"path": "/w/src/b.rs"}}, cwd="/w")["text"],
                         "? edit   src/b.rs", "inside the agent's folder: relative")
        self.assertEqual(agent_transcript.tool_row("b", {"title": "bash", "input": {"command": "/w/deploy.sh --prod"}}, cwd="/w")["text"],
                         "? bash   /w/deploy.sh --prod", "a command is never rewritten")
        self.assertEqual(read["actions"], [{"text": "open", "command": "links.open", "args": opens[0]["args"]},
                                           {"text": "copy", "command": "client.copy"}], "visible [open] [copy]")
        bash = agent_transcript.tool_row("b", {"title": "bash", "input": {"command": "ls"}, "content": text("a\nb")})
        self.assertEqual(bash["actions"], [{"text": "copy", "command": "client.copy"}], "the client copies the row's copy field")
        self.assertEqual(bash["menu"], [{"label": "Copy output", "command": "client.copy"}])
        self.assertEqual(opens[1]["label"], "Open a.rs:40 in editor")
        opened = lambda row: [m for m in row.get("menu", []) if m["command"] == "links.open"]
        self.assertEqual(opened(agent_transcript.tool_row("r", {"title": "read", "input": {"path": "src/a.rs"}})), [],
                         "a relative path with no reported folder is never guessed")
        self.assertEqual(opened(agent_transcript.tool_row("r", {"title": "write", "input": {"path": "/w/x\nevil"}}, cwd="/w")), [])
        many = agent_transcript.tool_row("r", {"title": "edit", "locations": [{"path": f"/w/{i}"} for i in range(9)]})
        self.assertEqual(len([m for m in many["menu"] if m["command"] == "links.open"]), agent_transcript.OPEN_ITEMS)

    def test_notices_and_other_kinds_show_what_they_say(self):
        """K3 bug (vqro-lead-secondary, part-a-94 O12): an entity of kind notice rendered nothing at all."""
        say = {("message", "m"): {"position": 1, "role": "assistant", "final": True, "content": text("partial answer")},
               ("notice", "n"): {"value_is": "state", "severity": "warn", "text": "assistant output omitted: too large"},
               ("notice", "hidden"): {"native": {"reason": "output omitted"}, "id": "truncated"},
               ("x.acme.step", "s"): {"position": 2, "content": text("indexing the repo")},
               ("_ui", "u"): {"position": 3, "text": "an implementation's own bookkeeping"},
               ("state", "current"): {"state": "idle", "text": "facts, shown elsewhere"}}
        rows = self.rows(say)
        texts = [r.get("text") for r in rows]
        self.assertEqual(texts, ["Pi", "partial answer", "", "· Pi x.acme.step: indexing the repo", "",
                                 "· Pi notice: assistant output omitted: too large"],
                         "in timeline order when positioned, after it when not; hidden metadata never shown")
        self.assertEqual(rows[-1]["style"], "warn", "severity styles it")

    def test_a_severity_of_any_shape_never_breaks_a_notice(self):
        """vqro-lead-secondary (shadow-notices-97-review.md): a list or object severity raised TypeError in both renderers."""
        for odd in (["warn"], {"level": "warn"}):
            row = agent_transcript.entity_rows("notice", "n", {"severity": odd, "text": "still shown"}, "Pi")[0]
            self.assertEqual((row["text"], row["style"]), ("· Pi notice: still shown", "dim"), odd)
            live = agent_transcript.notice_rows([(None, {"severity": odd, "text": "still shown"})], "Pi")[0]
            self.assertEqual((live["text"], live["style"]), ("· Pi notice: still shown", "dim"), odd)
        self.assertEqual(agent_transcript.entity_rows("notice", "n", {"severity": "warn", "text": "w"})[0]["style"], "warn")
        self.assertEqual(agent_transcript.entity_rows("notice", "n", {"severity": "warning", "text": "w"})[0]["style"], "dim",
                         "only the spec's values: an unknown severity is as if absent")

    def test_an_empty_system_message_leaves_no_bare_label(self):
        """The human's 16:44 screenshot: a stray "System" label at the top (Pi's system message, an empty text block)."""
        empty = {("message", "s"): {"position": 1, "role": "system", "final": True, "content": text("")},
                 ("message", "u"): {"position": 2, "role": "user", "final": True, "content": text("hello")}}
        self.assertNotIn("System", [r.get("text") for r in self.rows(empty)])

    def test_the_agents_work_sits_in_its_labelled_block(self):
        """Mockup Screens A and C: an Activity continues the agent's block, or the reply's label moves above it."""
        texts = [r.get("text") for r in self.rows()]
        i = texts.index("Let me look.")
        self.assertEqual(texts[i:i + 3], ["Let me look.", "First the CI log.", "Activity · 2 tools · 4.8s · 1 failed"], "no break")
        work_first = {("tool", "t1"): dict(CONVERSATION[("tool", "t1")], position=1),
                      ("message", "a"): {"position": 2, "role": "assistant", "final": True, "content": text("Done.")}}
        self.assertEqual([r.get("text") for r in self.rows(work_first)], ["Pi", "Activity · 1 tool · 0.2s", "Done."])
        again = {**work_first, ("tool", "t2"): dict(CONVERSATION[("tool", "t1")], position=3),
                 ("message", "b"): {"position": 4, "role": "assistant", "final": True, "content": text("Next.")}}
        self.assertEqual([r.get("text") for r in self.rows(again)],
                         ["Pi", "Activity · 1 tool · 0.2s", "Done.", "Activity · 1 tool · 0.2s", "", "Pi", "Next."],
                         "each reply keeps one label; later work continues the earlier reply's block")

    def test_another_senders_message_folds_after_4_lines_and_blocks_are_spaced(self):
        long = {("message", "a"): {"position": 1, "role": "user", "final": True, "origin": {"kind": "agent", "agent": "r7", "principal": "p:lead"},
                                   "content": text("\n".join(f"point {i}" for i in range(9)))},
                ("message", "b"): {"position": 2, "role": "assistant", "final": True, "content": text("ok")}}
        rows = self.rows(long)
        texts = [r.get("text") for r in rows]
        self.assertEqual(texts[:6], ["Message from vqro-lead (agent r7)", "point 0", "point 1", "point 2", "point 3", "5 more lines"],
                         "clients without the frame: the same fold, on plain rows")
        names = agent_transcript.Names({"r2": {"name": "Pi"}, "r7": {"name": "vqro-lead", "kind": "agent", "principal": "p:lead"}}, "r2")
        framed = agent_transcript.rows({k: (1, v) for k, v in long.items()}, names)[0]
        self.assertEqual({k: framed[k] for k in ("type", "title", "fold", "requires")},
                         {"type": "message", "title": "Message from vqro-lead (agent r7)", "fold": 4, "requires": ["message", "fold"]})
        self.assertEqual(framed["body"], "\n".join(f"point {i}" for i in range(9)))
        quoted = agent_transcript.rows({k: (1, v) for k, v in CONVERSATION.items() if k == ("message", "m3")}, names)[0]
        self.assertEqual(quoted["title"], "Message from vqro-lead (agent r7) · quoting alice (claimed)")
        self.assertEqual([r["text"] for r in base([quoted])], ["Message from vqro-lead (agent r7)", "quoting alice (claimed)", "Bump the lockfile too."],
                         "the quote never counts toward the 4 lines")
        self.assertEqual(texts[6:8], ["", "Pi"], "a blank line between blocks")

    def test_one_huge_tool_output_never_crowds_out_the_conversation(self):
        """Review P2-1: the rest of an output is bounded, so the panel budget keeps the older conversation."""
        huge = dict(CONVERSATION)
        huge[("tool", "t2")] = dict(huge[("tool", "t2")], content=text("a b\n" * 16000))
        rows = agent_transcript.rows({k: (1, v) for k, v in huge.items()},
                                     agent_transcript.Names({"r2": {"name": "Pi"}}, "r2"))
        self.assertLessEqual(len(json.dumps(rows).encode()), agent_transcript.BUDGET)
        self.assertIn("Why is the build red?", [r.get("text") for r in rows], "the older conversation survives")
        group = next(r for r in rows if r.get("text", "").startswith("Activity"))
        rest = group["children"][1]["children"][-1]["children"]
        self.assertEqual(len(rest), agent_transcript.HIDDEN_ROWS + 1)
        self.assertTrue(rest[-1]["text"].endswith("more lines not shown here"))

    def test_one_oversized_activity_keeps_its_newest_steps_within_the_budget(self):
        """vqro-lead-secondary #74 (probe-74-transcript-controls.py): 40 legal reads in one Activity rendered 525 KB, 3x
        BUDGET, because one row can't be trimmed. The Activity keeps its newest steps and says how many it left out."""
        names = agent_transcript.Names({"r2": {"name": "Pi", "kind": "agent"}}, "r2")
        reads = {("tool", f"t{i}"): {"position": i, "title": "read", "status": "completed", "input": {"path": f"file-{i}.txt"},
                                     "content": text(("x" * 1000 + "\n") * 12)} for i in range(40)}
        rows = agent_transcript.rows({k: (1, v) for k, v in reads.items()}, names, "all_retained")
        self.assertLessEqual(len(json.dumps(rows, ensure_ascii=False).encode()), agent_transcript.BUDGET)
        group = next(r for r in rows if r.get("text", "").startswith("Activity · 40 tools"))
        self.assertRegex(group["children"][0]["text"], r"^… \d+ earlier steps not shown here \(pane limit\)$")
        self.assertEqual(group["children"][-1]["text"], "✓ read   file-39.txt", "the newest step is kept")

    def test_one_tool_with_thousands_of_arguments_stays_within_the_budget(self):
        """vqro-lead-secondary #74 (probe-74-single-step-budget.py): a 35 KB entity with 3000 arguments rendered 200 KB."""
        names = agent_transcript.Names({"r2": {"name": "Pi", "kind": "agent"}}, "r2")
        for count in (10, 3000):
            value = {"position": 1, "title": "custom_tool", "status": "completed", "input": {f"k{i}": 0 for i in range(count)}}
            rows = agent_transcript.rows({("tool", "one"): (1, value)}, names, "all_retained")
            self.assertLessEqual(len(json.dumps(rows, ensure_ascii=False).encode()), agent_transcript.BUDGET, count)
            step = rows[0]["children"][0]
            if count == 10:
                self.assertEqual(len(step["children"]), 10, "small: every argument shown")
            else:
                self.assertRegex(step["children"][-1]["text"], r"^… \d+ more arguments not shown here \(pane limit\)$")
        big = agent_transcript.tool_row("b", {"title": "custom", "status": "completed", "content": text("THE OUTPUT"),
                                              "input": {f"a{i}": "z" * 900 for i in range(40)}})
        self.assertIn("THE OUTPUT", [c.get("text") for c in big["children"]], "many big arguments never push the output out")

    def test_hiding_tools_never_hides_the_thinking_between_them(self):
        """vqro-lead-secondary #74: Tools=hide with Thinking=show erased an intervening thought with no disclosure."""
        names = agent_transcript.Names({"r2": {"name": "Pi", "kind": "agent"}}, "r2")
        steps = {("message", "s"): {"position": 1, "role": "assistant", "final": True, "content": [{"type": "thought", "text": "KEEP_THIS"}]},
                 ("tool", "c"): {"position": 2, "title": "read", "status": "completed", "input": {"path": "/w/x"}}}
        for thinking, tools, shown in (("show", "show", True), ("show", "hide", True), ("show", "summary", True),
                                       ("hide", "hide", False), ("hide", "show", False)):
            rows = agent_transcript.rows({k: (1, v) for k, v in steps.items()}, names, "all_retained",
                                         agent_transcript.view_settings({"thinking": thinking, "tools": tools}))
            flat = json.dumps(rows, ensure_ascii=False)
            self.assertEqual("KEEP_THIS" in flat, shown, (thinking, tools))
            if tools == "hide":
                self.assertIn("· hidden: 1 tools", [r.get("text") for r in rows], "the hidden tool is still disclosed")

    def test_a_long_thinking_step_inside_activity_stays_readable(self):
        """Review P2-2: a thought longer than its one line is all one expand away."""
        long = "x" * 999
        step = agent_transcript.thought_step("s", long)
        self.assertEqual(len(step["text"].encode()), len(("┊ " + agent_transcript.clip(long, agent_transcript.STEP_BYTES)).encode()))
        self.assertEqual(step["children"][0]["text"], "┊ " + long)
        self.assertNotIn("children", agent_transcript.thought_step("s", "short"), "nothing more to open")
        self.assertEqual([c["text"] for c in agent_transcript.thought_step("s", "one\ntwo")["children"]], ["┊ one", "┊ two"])

    def test_short_output_is_packed_and_placeholders_never_appear(self):
        listing = agent_transcript.output_rows("/w\n" + "\n".join(f"file{i}.md" for i in range(20)))
        self.assertEqual(listing[0]["text"], "/w")
        self.assertTrue(len(listing) <= 3 and "file19.md" in listing[-1]["text"], "a file listing packs several per row")
        pi = {("message", "m"): {"position": 1, "role": "assistant", "final": True, "content": [
            {"type": "thought", "text": "first\nsecond\nthird"}, {"type": "toolCall", "name": "bash", "arguments": {}},
            {"type": "text", "text": "done"}]}}
        texts = [r.get("text") for r in self.rows(pi)]
        self.assertFalse(any("content]" in (t or "") for t in texts), "no [thought content] / [toolCall content]")
        self.assertEqual(texts[1:4], ["┊ first", "┊ second", "more"], "thinking: the first 2 lines, then more")
        self.assertEqual(texts[-1], "done")

    def test_tail_coverage_says_so_and_the_budget_keeps_the_newest(self):
        note = {"type": "text", "style": "dim", "text": "Showing the recent history this agent reported."}
        self.assertEqual(self.rows(coverage="tail")[0], note, "declared tail: disclosed, neutral, never shown complete")
        later = {k: dict(v, position=v.get("position", 0) + 5) if k[0] != "receipt" else v for k, v in CONVERSATION.items()}
        self.assertEqual(self.rows(later, coverage="tail")[0], note, "positions are no evidence of a cut: same words")
        self.assertNotEqual(self.rows(later, coverage="all_retained")[0].get("style"), "dim", "complete: no note")

        many = {("message", f"m{i}"): {"position": i, "role": "assistant", "content": text(f"{i} " + "x" * 2000), "final": True}
                for i in range(200)}
        rows = self.rows(many)
        self.assertLessEqual(len(json.dumps(rows, ensure_ascii=False).encode()), agent_transcript.BUDGET + 200)
        self.assertIn("earlier conversation not shown", rows[0]["text"])
        self.assertTrue(rows[-1]["text"].startswith("199 "), "the newest message is kept")
        turn = {("message", "go"): {"position": 0, "role": "user", "final": True, "content": text("go")}}
        for r in range(10):  # one agent turn: ten replies, each followed by five tools with 3 KB of output
            turn[("message", f"r{r}")] = {"position": 10 * r + 1, "role": "assistant", "final": True, "content": text(f"round {r}")}
            for i in range(5):
                turn[("tool", f"t{r}.{i}")] = {"position": 10 * r + 2 + i, "title": "bash", "status": "completed",
                                               "input": {"command": "x"}, "content": text("y" * 3000)}
        texts = [r.get("text") for r in self.rows(turn)]
        self.assertLessEqual(len(json.dumps(self.rows(turn), ensure_ascii=False).encode()), agent_transcript.BUDGET + 200)
        self.assertIn("round 9", texts, "the turn that is larger than the room left keeps its newest rows, not nothing")
        self.assertNotIn("go", texts)

    def test_text_is_control_safe_and_content_blocks_are_typed(self):
        odd = {("message", "m"): {"position": 1, "role": "assistant", "final": True,
                                  "content": [{"type": "text", "text": "a\x1b[31mb"}, {"type": "image", "mime": "image/png"}]}}
        texts = [r["text"] for r in self.rows(odd)]
        self.assertIn("a\\x1b[31mb", texts)
        self.assertIn("[image: image/png]", texts)


class ViewSettings(unittest.TestCase):
    """AS10: nothing hidden by default; each kind shown, summarised (expandable) or hidden with a count left."""

    def rows(self, **view):
        names = agent_transcript.Names({"r2": {"name": "Pi"}, "r7": {"name": "vqro-lead", "kind": "agent", "principal": "p:lead"}}, "r2")
        entities = dict(CONVERSATION)
        entities[("message", "m4")] = dict(entities[("message", "m4")], at_ms=1_763_600_060_000,
                                           usage={"tokens": {"output": 1200}})
        return base(agent_transcript.rows({k: (1, v) for k, v in entities.items()}, names, None,
                                          agent_transcript.view_settings(view)))

    def find(self, rows, prefix):
        return next(r for r in rows if r.get("text", "").startswith(prefix))

    def test_defaults_show_everything(self):
        self.assertEqual(agent_transcript.view_settings(), agent_transcript.DEFAULT_VIEW)
        self.assertIn("┊ Config change, not code.", [r.get("text") for r in self.rows()], "thinking shown by default")

    def test_summary_replaces_a_kind_with_a_configurable_expandable_line(self):
        rows = self.rows(thinking="summary", tools="summary")
        thinking = self.find(rows, "Thinking ·")
        self.assertEqual(thinking["text"], "Thinking · " + agent_transcript.when(1_763_600_060_000) + " · 1.2k tokens")
        self.assertFalse(thinking["expanded"])
        self.assertIn("┊ Config change, not code.", [c.get("text") for c in thinking["children"]], "one expand away")
        tools = self.find(rows, "Activity ·")
        self.assertEqual((tools["text"], tools["expanded"]), ("Activity · 4.8s · 2 tools", False))
        lines = self.rows(thinking="summary", summary=["lines", "words"])
        self.assertEqual(self.find(lines, "Thinking ·")["text"], "Thinking · 1 lines · Config change, not code.…")

    def test_hide_leaves_a_count_and_senders_can_be_folded(self):
        rows = self.rows(thinking="hide", tools="hide")
        texts = [r.get("text") for r in rows]
        self.assertIn("· hidden: 2 tools", texts)
        self.assertIn("· hidden: 1 thinking", texts)
        self.assertFalse(any(t and t.startswith("Activity") for t in texts))
        folded = self.find(self.rows(hidden_senders=["principal:p:lead"]), "Message from vqro-lead")
        self.assertFalse(folded["expanded"])

    def test_compact_collapses_tools_folds_long_messages_and_drops_repeat_times(self):
        names = agent_transcript.Names({}, "r2")
        long = {("message", f"m{i}"): {"position": i, "role": "user", "at_ms": 1_763_600_000_000, "final": True,
                                       "origin": {"kind": "human"}, "content": text("\n".join(f"line {n}" for n in range(60)))}
                for i in range(2)}
        rows = agent_transcript.rows({k: (1, v) for k, v in long.items()}, names, None, agent_transcript.view_settings({"density": "compact"}))
        headers = [r["text"] for r in rows if str(r.get("id", "")).startswith("m:")]
        self.assertEqual(headers, ["Human (undeclared) · " + agent_transcript.when(1_763_600_000_000), "Human (undeclared)"],
                         "time only when the sender changes")
        fold = next(r for r in rows if r.get("text") == "20 more lines")
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
        self.core.resources["r7"] = {"id": "r7", "kind": "agent", "provider": "other", "name": "vqro-lead", "state": "live", "principal": "p:lead",
                                     "available": True, "data": {}}
        self.agents = Agents(self.core)
        self.timers = []  # delayed reattaches, fired by elapse() instead of real timers
        self.agents.later = lambda delay, rid: self.timers.append((delay, rid))
        self.agents.flush_conversations()

    def elapse(self):
        """The backoff waits pass: fire every pending delayed reattach."""
        timers, self.timers = self.timers, []
        for _, rid in timers:
            self.agents.conversations[rid].retry_at = 0.0
            self.agents.wake(rid, reattach=True)
        self.agents.flush_conversations()
        return [delay for delay, _ in timers]

    def attaches(self):
        return [p for m, p in self.core.calls if m == "resource.attach"]

    def feed(self, data):
        stream = self.agents.conversations["r2"].stream
        self.core.sinks[stream].data(data)
        self.agents.flush_conversations()

    def texts(self):
        return [r.get("text") for r in base(self.core.live["agents-r2"]["items"])]

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

    def test_an_empty_state_only_pane_says_so_instead_of_no_messages(self):
        self.feed(snapshot({}, coverage="state_only"))
        self.assertIn("The agent sent only its current state, not its conversation.", self.texts())
        self.assertNotIn("No messages yet.", self.texts())

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

    def gear(self):
        """Open the pane's ⚙ popup; its rows."""
        chip = next(r for r in self.core.live["agents-r2"]["items"] if r.get("id") == "gear")["chips"][0]
        reply = self.core.handlers[chip["command"]](dict(chip["args"]))
        return self.core.live[reply["client"]["popup"]["view"]]["items"]

    def test_view_filters_in_the_gear_write_this_panes_settings_and_make_default_copies_them(self):
        self.feed(snapshot(CONVERSATION))
        toolbar = self.gear()
        thinking = next(c for c in toolbar if c.get("text") == "Thinking: show")
        self.core.handlers["agents.view"](dict(thinking["args"], value="hide"))
        self.assertEqual(self.core.value("doc/agents/view/r2"), {"thinking": "hide"})
        senders = next(c for c in toolbar if c.get("text") == "Senders: all")
        self.assertIn("principal:p:lead", [o["value"] for o in senders["options"]])
        self.core.handlers["agents.view"](dict(senders["args"], value="principal:p:lead"))
        self.assertEqual(self.core.value("doc/agents/view/r2")["hidden_senders"], ["principal:p:lead"])
        self.agents.event("entries", {"changes": [{"key": "doc/agents/view/r2"}]})
        self.assertIn("· hidden: 1 thinking", self.texts())
        self.assertIn("Thinking: hide", [r.get("text") for r in self.core.live["agents-pane-r2"]["items"]], "the open ⚙ follows")
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

    def test_no_header_line_one_gear_button_and_the_state_in_the_frame(self):
        """relay-11 (the human): no name line inside the pane, one small ⚙ for settings, bridge terminal and Stop; the
        frame carries name · folder and the state."""
        self.core.resources["r9"] = {"id": "r9", "kind": "terminal", "name": "pi-bridge-1", "state": "live", "provider": "herdr"}
        self.core.providers["herdr"] = {"name": "herdr", "capabilities": {"terminal.kill": {}}}
        self.core.resources["r2"]["host"] = "r9"
        self.core.resources["r2"]["data"] = {"agent": {"v": 0, "state": "idle", "lifecycle": "live", "session": {"cwd": "~/Projects/site"}}}
        self.agents.event("resource", {"id": "r2"})
        self.feed(snapshot(CONVERSATION))
        pane = self.core.live["agents-r2"]
        items = pane["items"]
        self.assertEqual((pane["title"], pane["title_right"], pane["title_right_style"]), ("Pi r2 · ~/Projects/site", "● idle", "ok"))
        self.assertNotIn("toolbar", pane, "no top bar at all (the human, 17:00)")
        self.assertEqual(self.core.entries["menu/agents.r2.pane"]["value"]["command"], "agents.pane", "⚙ also in the pane menu (D15)")
        gear = {"text": "", "icon": {"name": "settings", "text": "⚙"}, "command": "agents.pane", "args": {"resource": "r2"}}
        row = next(r for r in items if r.get("id") == "gear")
        self.assertEqual((row["chips"], row["pin"], "requires_absent" in row), ([gear], "bottom", False),
                         "no message box here: the ⚙ is a bottom row for every client")
        self.assertFalse(any(r.get("text") in ("Bridge terminal", "Agent settings…") for r in items), "no old panel rows")
        gear = self.gear()
        self.assertEqual([r.get("text") for r in gear], ["View", "Thinking: show", "Tools: show", "System: show", "Senders: all",
                                                         "Layout: detailed", "Summary: time, tokens, tools",
                                                         "Make these the default for agent panes", "Agent",
                                                         "Bridge terminal (pi-bridge-1)", "Stop agent…"])
        self.core.calls.clear()
        bridge, stop = gear[-2], gear[-1]
        self.assertEqual(self.core.handlers[bridge["command"]](dict(bridge["args"]))["client"], {"close": True})
        self.assertEqual(self.core.calls[-1], ("tabs.show", {"resource": "r9"}))
        reply = self.core.handlers[stop["command"]](dict(stop["args"]))
        self.assertTrue(reply["client"]["popup"]["view"].startswith("agents-stop-"), "Stop still asks first")

    def test_the_composer_offers_auto_and_the_declared_modes_and_sends(self):
        items = self.make_v0()
        composer = next(r for r in items if r.get("type") == "composer")
        self.assertEqual((composer["pin"], composer["autofocus"], composer["submit"], composer["args"]),
                         ("bottom", True, "Enter", {"resource": "r2", "caps_rev": 2}))
        self.assertEqual((composer["controls"][0]["type"], composer["controls"][0]["value"]), ("select", "auto"), "one input-settings button")
        self.assertEqual(composer["controls"][1], {"type": "button", "text": "", "icon": {"name": "settings", "text": "⚙"},
                                                   "command": "agents.pane", "args": {"resource": "r2"}}, "⚙ beside Input")
        stand_in = next(r for r in items if r.get("id") == "gear")
        self.assertEqual(stand_in["requires_absent"], ["composer_buttons"], "older clients: a ⚙ row instead, never both")
        self.assertIs(composer["send_button"], False, "no Send button: Enter sends")
        self.assertEqual([c["value"] for c in composer["controls"][0]["options"]], ["auto", "boundary", "settle"], "steer before follow-up")
        self.core.handlers["agents.send"](dict(composer["args"], mode="settle", text="later"))  # declared modes still send
        self.assertEqual(composer["fallback"]["multiline"], True)
        self.assertEqual(items[-1]["pin"], "bottom", "the facts line comes last")
        self.core.handlers["agents.send"](dict(composer["args"], mode="auto", text="go"))
        sent = [p for m, p in self.core.calls if m == "resource.invoke"][-1]["args"]
        self.assertEqual((sent["mode"], sent["content"]), ("start", [{"type": "text", "text": "go"}]))
        self.assertEqual(sent["render"], ["user", "attributed_user"], "plain user words when the owner is proven")

    def test_an_admit_only_agent_is_still_reachable_from_the_composer(self):
        """prim-opus-review: with admit hidden from the chips, auto on an admit-only agent (Codex) asks, and the choice
        still offers admit, labelled weaker."""
        self.core.providers["items"]["capabilities"].update({"agent.send": {"modes": {"admit": {}}}})
        self.core.resources["r2"]["data"] = {"agent": {"v": 0, "state": "idle", "lifecycle": "live", "epoch": "e1",
                                                       "caps": {"rev": 1, "agent.send": {"modes": {"admit": {}}}}}}
        self.agents.event("resource", {"id": "r2"})
        self.feed(snapshot(CONVERSATION))
        composer = next(r for r in self.core.live["agents-r2"]["items"] if r.get("type") == "composer")
        self.assertEqual([c["value"] for c in composer["controls"][0]["options"]], ["auto"])
        self.assertEqual(composer["rows"], 8, "grows up to about a third of the pane")
        reply = self.core.handlers["agents.send"](dict(composer["args"], mode="auto", text="hi"))
        rows = [r for r in self.core.live[reply["client"]["popup"]["view"]]["items"] if r.get("command") == "agents.send"]
        self.assertEqual([(r["args"]["mode"], "weaker" in r.get("description", "")) for r in rows], [("admit", True)])
        self.core.handlers["agents.send"](rows[0]["args"])
        self.assertEqual([p for m, p in self.core.calls if m == "resource.invoke"][-1]["args"]["mode"], "admit")

    def test_auto_on_a_busy_agent_asks_and_sends_nothing(self):
        items = self.make_v0(state="running")
        composer = next(r for r in items if r.get("type") == "composer")
        before = len([1 for m, _ in self.core.calls if m == "resource.invoke"])
        reply = self.core.handlers["agents.send"](dict(composer["args"], mode="auto", text="go"))
        self.assertEqual(reply["client"]["popup"]["view"], "agents-send-r2")
        self.assertEqual(len([1 for m, _ in self.core.calls if m == "resource.invoke"]), before)

    def test_live_notices_are_shown_newest_last_and_bounded(self):
        """agent-v0 section 4 notice records (live, no seq) were dropped by the pane."""
        self.feed(snapshot(CONVERSATION))
        for i in range(5):
            self.feed(line({"op": "notice", "value": {"text": f"rate limited {i}", "severity": "warn"}}))
        shown = [t for t in self.texts() if t and "rate limited" in t]
        self.assertEqual(len(shown), 3, "the newest three")
        self.assertTrue(shown[-1].startswith("· Pi notice · ") and shown[-1].endswith(": rate limited 4"), shown[-1])
        self.feed(snapshot(CONVERSATION).replace(b'"epoch": "e1"', b'"epoch": "e2"'))
        self.assertFalse(any(t and "rate limited" in t for t in self.texts()), "a new epoch clears earlier live notices")

    def test_live_notices_follow_record_order_not_byte_chunks(self):
        """#97 fix-forward (vqro-lead-secondary, notices-97-epoch-finding.md): an old notice, a complete new-epoch
        snapshot and a new notice kept the old notice when they arrived in one chunk, but not when split."""
        from agents import Conversation

        def epoch_snapshot(epoch):
            return line({"op": "snapshot", "phase": "begin", "epoch": epoch, "coverage": "all_retained"}) + \
                line({"op": "snapshot", "phase": "end", "seq": 0})
        old, new = (line({"op": "notice", "value": {"text": t}}) for t in ("old-session-only", "new-session-only"))

        def observe(parts):
            c = Conversation("r2", lambda *a, **k: None, {})
            c.data(epoch_snapshot("e1"))
            for part in parts:
                c.data(part)
            return c.view.epoch, [v["text"] for _, v in c.notices]
        stream = old + epoch_snapshot("e2") + new
        split = observe([old, epoch_snapshot("e2"), new])
        self.assertEqual(split, ("e2", ["new-session-only"]))
        self.assertEqual(observe([stream]), split, "the same records in one chunk")
        self.assertEqual(observe([stream[:7], stream[7:40], stream[40:]]), split, "or cut anywhere")
        self.assertEqual(observe([old + epoch_snapshot("e2")]), ("e2", []), "an old notice never outlives the new cut")
        self.assertEqual(observe([old, new]), ("e1", ["old-session-only", "new-session-only"]), "same epoch: all kept")

    def test_one_agents_render_failure_never_stalls_the_others(self):
        """prim-opus-review: the publish loops had no per-agent guard, so one agent's render bug stopped every pane."""
        import contextlib, io
        self.core.resources["r5"] = dict(self.core.resources["r2"], id="r5", name="Other")
        self.agents.event("resource", {"id": "r5"})
        self.agents.flush_conversations()
        self.agents.strict = False  # production behaviour (the fixture is strict so other tests catch masked bugs)
        real = self.agents.panel
        self.agents.panel = lambda r: (_ for _ in ()).throw(KeyError("boom")) if r["id"] == "r2" else real(r)
        with contextlib.redirect_stderr(io.StringIO()) as err:
            self.agents.publish_panels()
            self.agents.wake("r2"), self.agents.wake("r5")
            self.agents.flush_conversations()
        broken = self.core.live["agents-r2"]["items"][0]
        self.assertEqual(broken["style"], "error")
        self.assertIn("couldn't be drawn (KeyError: 'boom')", broken["text"])
        self.assertNotIn("couldn't be drawn", json.dumps(self.core.live["agents-r5"]), "the other agent's pane is drawn")
        self.assertEqual(err.getvalue().count("pane failed for r2"), 1, "logged once per distinct error, not per redraw")
        self.agents.panel = real
        self.agents.wake("r2")
        self.agents.flush_conversations()
        self.assertNotIn("couldn't be drawn", json.dumps(self.core.live["agents-r2"]), "it redraws on the next update")
        self.assertNotIn(("r2", "pane"), self.agents.logged, "a good draw forgets the logged error")

    def test_logged_failures_are_pruned_with_the_agent_and_capped(self):
        import contextlib, io
        with contextlib.redirect_stderr(io.StringIO()) as err:
            for i in range(agents_module.LOGGED_MAX + 10):
                self.agents.log_failure(f"r{i}", "pane", KeyError("boom"))
            self.assertEqual(len(self.agents.logged), agents_module.LOGGED_MAX, "capped, oldest dropped")
            self.assertNotIn(("r0", "pane"), self.agents.logged)
            self.agents.log_failure("r300", "pane", KeyError("boom"))
            self.agents.log_failure("r300", "attach", KeyError("boom"))
            self.agents.publish_gone("r300")
        self.assertFalse([k for k in self.agents.logged if k[0] == "r300"], "an agent's entries go with it")
        self.assertEqual(err.getvalue().count("failed for r300"), 2)

    def test_the_waiting_line_and_the_runtimes_queue_are_pinned(self):
        items = self.make_v0(requests=[{"request": "q1", "revision": 1, "method": "confirm", "title": "Run git push?", "params": {}}],
                             queue=[{"mode": "settle", "preview": "bump the lockfile"}])
        waiting = next(r for r in items if r.get("id") == "waiting")
        self.assertEqual((waiting["command"], waiting["pin"]), ("agents.answer", "bottom"))
        self.assertIn("Run git push?", waiting["text"])
        self.assertEqual([a["text"] for a in waiting["actions"]], ["Answer…"], "a visible [Answer…] (mockup Screen D)")
        self.assertEqual(self.core.live["agents-r2"]["title_right_style"], "warn", "waiting for you: the frame's state warns")
        queue = next(r for r in items if r.get("id") == "queue")
        self.assertEqual(queue["text"], "Messages · 1 held by Pi")
        self.assertEqual(queue["menu"][0]["command"], "agents.clear_queue")

    def test_provider_bounds_are_clamped_and_an_overflowing_history_stops_instead_of_looping(self):
        """Review P2-2: a provider's declaration is taken only up to the viewer's own ceiling."""
        self.core.providers["items"]["capabilities"]["items.stream"] = {"entities": 10 ** 9, "entity_bytes": 10 ** 10}
        self.core.resources["r3"] = dict(self.core.resources["r2"], id="r3")
        self.agents.event("resource", {"id": "r3"})
        c = self.agents.conversations["r3"]
        self.assertEqual((c.view.max_entities, c.view.max_entity_bytes, c.clamped), (4096, 4 * 1024 * 1024, True))
        many = {("message", f"m{i}"): {"position": i, "role": "assistant", "content": text("x"), "final": True} for i in range(65)}
        for attempt in range(3):  # the same overflowing snapshot on every reattach: back off twice, then stop
            self.feed(snapshot(many))  # r2 keeps 64 entities: this snapshot overflows
            self.assertIn("Incompatible stream", " ".join(self.texts()))
            self.assertIn("64 items", " ".join(self.texts()), "the limit this view ran into")
            self.assertIsNone(self.agents.conversations["r2"].stream, "the rejected stream is closed, not read on")
            if attempt < 2:
                self.assertEqual([round(d, 1) for d in self.elapse()], [(2.0, 10.0)[attempt]], "one delayed reattach, never a tight loop")
        attaches = len(self.attaches())
        self.assertIn("stopped updating", " ".join(self.texts()))
        self.agents.flush_conversations()
        self.assertEqual((len(self.attaches()), self.timers), (attaches, []), "no reattach loop")
        self.assertIsNone(self.agents.conversations["r2"].stream)
        self.assertIn("Show the agent again", " ".join(self.texts()))
        self.core.handlers["agents.show"]({"resource": "r2"})  # what the advice says: it retries
        self.agents.flush_conversations()
        self.assertFalse(self.agents.conversations["r2"].too_big)
        self.assertEqual(self.attaches()[-1], {"resource": "r2", "items": {}}, "a fresh attach")

    def bad_record_storm(self, record, rounds=50):
        """The producer repeats the rejected record on every attach (agent-v0 section 4). Returns the attach cursors."""
        self.feed(snapshot(CONVERSATION, seq=4))
        for _ in range(rounds):
            if self.agents.conversations["r2"].stream is not None:
                self.feed(line(record))
            self.agents.flush_conversations()
            self.elapse()
        return [a["items"].get("after") for a in self.attaches()]

    def test_a_repeated_bad_record_backs_off_then_stops_instead_of_a_reopen_storm(self):
        """Part A O14 (regrade54: 64 Pi / 66 Codex reopens in 10 s): a record this view can't read, repeated on every
        continue, gets two delayed retries from the last good cursor and then stops with a visible report."""
        cursors = self.bad_record_storm({"op": "put", "seq": 5, "kind": "message"})  # no id: unreadable
        self.assertEqual(len(cursors), 3, "the first attach plus two backed-off retries, not a storm")
        self.assertEqual(cursors[1:], [{"epoch": "e1", "seq": 4}] * 2, "never skips the record or advances the cursor")
        note = " ".join(self.texts())
        self.assertIn("Incompatible stream", note)
        self.assertIn("can't read (bad_record", note)
        self.assertIn("at seq 5", note)
        self.assertIn("stopped updating after 3 identical attempts", note)
        self.assertIn("Fixing", note, "the last good conversation is kept, nothing partial installed")
        self.assertNotIn("Reconnecting", note, "a stopped conversation does not claim to be reconnecting")
        self.core.handlers["agents.show"]({"resource": "r2"})  # an explicit retry
        self.agents.flush_conversations()
        self.assertEqual(len(self.attaches()), 4)

    def test_an_unknown_newer_op_is_reported_the_same_way(self):
        cursors = self.bad_record_storm({"op": "x.future", "seq": 5, "kind": "message", "id": "m9", "rev": 1})
        self.assertEqual(len(cursors), 3)
        note = " ".join(self.texts())
        self.assertIn('unknown operation "x.future" at seq 5, perhaps from a newer agent', note)
        self.assertIn("stopped updating", note)

    def test_backoff_is_visible_while_waiting(self):
        self.feed(snapshot(CONVERSATION, seq=4))
        self.feed(line({"op": "put", "seq": 5, "kind": "message"}))
        self.assertIsNone(self.agents.conversations["r2"].stream)
        self.assertEqual([(round(d, 1), rid) for d, rid in self.timers], [(2.0, "r2")])
        self.assertIn("Retrying after a pause (attempt 1 of 3)", " ".join(self.texts()))
        self.agents.flush_conversations()  # any number of wakes before the wait passes: no attach
        self.agents.wake("r2", reattach=True)
        self.agents.flush_conversations()
        self.assertEqual(len(self.attaches()), 1)

    def test_progress_starts_a_new_count_and_ordinary_loss_still_reattaches_at_once(self):
        self.feed(snapshot(CONVERSATION, seq=4))
        self.feed(line({"op": "put", "seq": 5, "kind": "message"}))
        self.elapse()
        self.feed(line({"op": "append", "seq": 5, "kind": "message", "id": "m5", "base": 1, "rev": 2,
                        "field": "/content/0/text", "text": "!"}))  # the producer moved on: progress
        self.feed(line({"op": "put", "seq": 6, "kind": "message"}))  # a new rejection at a new position
        self.assertEqual(self.agents.conversations["r2"].rejections, 1, "unchanged rejection is not progress; this is new")
        self.elapse()
        attaches = len(self.attaches())
        self.feed(line({"op": "append", "seq": 9, "kind": "message", "id": "m5", "base": 2, "rev": 3,
                        "field": "/content/0/text", "text": "?"}))  # ordinary loss (seq 7-8 missing): not incompatible
        self.assertEqual(len(self.attaches()), attaches + 1, "a data loss reattaches at once from a fresh snapshot")

    def test_an_over_bound_report_names_the_limit_the_agent_declared(self):
        self.core.providers["items"]["capabilities"]["items.stream"] = {"entities": 64, "entity_bytes": 3000}
        self.core.resources["r4"] = dict(self.core.resources["r2"], id="r4")
        self.agents.event("resource", {"id": "r4"})
        self.agents.flush_conversations()
        c = self.agents.conversations["r4"]
        c.data(snapshot({("message", "huge"): {"position": 1, "role": "assistant", "content": text("x" * 9000), "final": True}}))
        self.agents.flush_conversations()
        self.assertIn("3000 bytes", agents_module.incompatible_note(c))

    def test_a_change_in_the_declared_bounds_retries_a_stopped_conversation_with_the_new_limits(self):
        """spike-panel-1 P2-2: the retry after a compatibility change runs with the NEW declaration, so the snapshot
        that was over the old limits now installs."""
        self.core.providers["items"]["capabilities"]["items.stream"] = {"entities": 64, "entity_bytes": 3000}
        self.core.resources["r4"] = dict(self.core.resources["r2"], id="r4")
        self.agents.event("resource", {"id": "r4"})
        self.agents.flush_conversations()
        big = snapshot({("message", "huge"): {"position": 1, "role": "assistant", "content": text("x" * 9000), "final": True}})
        for _ in range(3):
            c = self.agents.conversations["r4"]
            self.core.sinks[c.stream].data(big)
            self.agents.flush_conversations()
            self.elapse()
        self.assertTrue(self.agents.conversations["r4"].incompatible)
        attaches = len(self.attaches())
        self.core.providers["items"]["capabilities"]["items.stream"] = {"entities": 128, "entity_bytes": 65536}
        self.agents.event("resource", {"id": "r4"})
        self.agents.flush_conversations()
        c = self.agents.conversations["r4"]
        self.assertEqual(len(self.attaches()), attaches + 1, "a compatibility change retries")
        self.assertEqual((c.view.max_entities, c.view.max_entity_bytes), (128, 65536), "with the new declaration")
        self.core.sinks[c.stream].data(big)
        self.agents.flush_conversations()
        self.assertEqual((c.view.snapshots, c.incompatible, c.rejections), (1, None, 0), "the snapshot now installs")

    def test_show_again_uses_the_current_declaration(self):
        self.bad_record_storm({"op": "put", "seq": 5, "kind": "message"}, rounds=6)
        self.core.providers["items"]["capabilities"]["items.stream"] = {"entities": 99, "entity_bytes": 77777}
        self.core.handlers["agents.show"]({"resource": "r2"})
        c = self.agents.conversations["r2"]
        self.assertEqual((c.view.max_entities, c.view.max_entity_bytes, c.incompatible), (99, 77777, None))

    def test_the_warning_clears_once_the_stream_moves_past_the_rejection(self):
        """spike-panel-1 P2-1: one rejection, then real progress: no stale 'Incompatible stream' over a healthy pane."""
        self.feed(snapshot(CONVERSATION, seq=4))
        self.feed(line({"op": "put", "seq": 5, "kind": "message"}))
        self.assertIn("Incompatible stream", " ".join(self.texts()))
        self.elapse()
        self.feed(line({"op": "append", "seq": 5, "kind": "message", "id": "m5", "base": 1, "rev": 2,
                        "field": "/content/0/text", "text": " on"}))  # the producer moved on
        self.assertNotIn("Incompatible stream", " ".join(self.texts()))
        self.assertEqual(self.agents.conversations["r2"].rejections, 0)

    def test_a_reset_snapshot_at_the_same_cut_is_not_progress(self):
        self.feed(snapshot(CONVERSATION, seq=4))
        self.feed(line({"op": "put", "seq": 5, "kind": "message"}))
        self.elapse()
        self.feed(snapshot(CONVERSATION, seq=4))  # back at the same cut, not past it
        self.assertEqual(self.agents.conversations["r2"].rejections, 1, "still counting")
        self.feed(line({"op": "put", "seq": 5, "kind": "message"}))
        self.assertEqual(self.agents.conversations["r2"].rejections, 2)

    def test_a_timer_firing_slightly_early_still_leads_to_a_retry(self):
        """spike-panel-1 P3 (probe_rv_timer): the timer callback clears the pending flag, so a timer that fires a hair
        before retry_at reschedules instead of stalling with neither a stream nor a timer."""
        self.feed(snapshot(CONVERSATION, seq=4))
        self.feed(line({"op": "put", "seq": 5, "kind": "message"}))
        self.assertEqual(len(self.timers), 1)
        self.timers.clear()
        c = self.agents.conversations["r2"]
        c.retry_at = agents_module.time.monotonic() + 0.5  # the wait has not quite passed when the timer fires
        self.agents.retry_due("r2")
        self.agents.flush_conversations()
        self.assertIsNone(c.stream)
        self.assertEqual(len(self.timers), 1, "a new timer, never a stall")

    def test_one_oversized_record_is_one_attempt_however_it_is_chunked(self):
        """vqro-lead-secondary #104 finding: 1,539 bytes at a 512-byte record bound counted once in one callback but
        three times (sticky stop, no reattach) in three 513-byte callbacks. One rejected stream is one attempt."""
        from agents import Conversation
        def row(value): return (json.dumps(value) + "\n").encode()
        begin = row({"op": "snapshot", "phase": "begin", "epoch": "e1", "coverage": "all_retained"}) + row({"op": "snapshot", "phase": "end", "seq": 0})
        chunk = b"x" * 513
        def observe(parts):
            c = Conversation("r2", lambda *a, **k: None, {"max_record_bytes": 512})
            c.data(begin)
            for part in parts:
                c.data(part)
            return (c.rejections, c.incompatible, c.view.after())
        one, three = observe([chunk * 3]), observe([chunk] * 3)
        self.assertEqual(one, three)
        self.assertEqual(three[:2], (1, None), "one attempt, not a sticky stop without any reattach")
        self.assertEqual(three[2], {"epoch": "e1", "seq": 0}, "the cursor never moves")

    def test_one_oversized_record_at_the_default_limit_is_one_attempt(self):
        """The same at the default 65,536-byte record bound (vqro-lead-secondary backoff-104/default-oversize.py):
        196,611 bytes in one callback or in three 65,537-byte callbacks is one attempt, with no sticky stop."""
        from agents import Conversation
        def row(value): return (json.dumps(value) + "\n").encode()
        begin = row({"op": "snapshot", "phase": "begin", "epoch": "e1", "coverage": "all_retained"}) + row({"op": "snapshot", "phase": "end", "seq": 0})
        part = b"x" * 65537
        def observe(parts):
            c = Conversation("r2", lambda *a, **k: None, {})
            self.assertEqual(c.view.max_record_bytes, 65536)
            c.data(begin)
            for chunk in parts:
                c.data(chunk)
            return (c.rejections, c.incompatible, c.generation, c.view.after())
        one, three = observe([part * 3]), observe([part] * 3)
        self.assertEqual(one, three)
        self.assertEqual(three, (1, None, 0, {"epoch": "e1", "seq": 0}))

    def test_the_reattach_budget_still_stops_on_the_third_stream(self):
        """Counting once per stream keeps the genuine budget: three reattached streams each oversized, then stop."""
        self.agents.conversations["r2"].view.max_record_bytes = 512
        for attempt in range(3):
            c = self.agents.conversations["r2"]
            for _ in range(3):  # each stream delivers its oversized record in pieces
                if c.stream is not None:
                    self.core.sinks[c.stream].data(b"x" * 513)
            self.agents.flush_conversations()
            if attempt < 2:
                self.elapse()
        self.assertIsNotNone(self.agents.conversations["r2"].incompatible)
        self.assertEqual(len(self.attaches()), 3, "the first attach plus two backed-off retries")

    def test_one_pending_backoff_timer_however_many_wakes(self):
        """spike-panel-1 P3: wakes during a backoff never pile up timers."""
        self.feed(snapshot(CONVERSATION, seq=4))
        self.feed(line({"op": "put", "seq": 5, "kind": "message"}))
        for _ in range(20):
            self.agents.wake("r2", reattach=True)
            self.agents.flush_conversations()
        self.assertEqual(len(self.timers), 1)

    def test_pane_keys_copy_fields_and_search(self):
        """S3/S4: keys without the prefix (scoped to agent panes), copy fields, search over collapsed rows."""
        keys = {k: self.core.value(k) for k in self.core.entries if k.startswith("binding/agents.") and k != "binding/agents.list"}
        scoped = {v["key"]: (v.get("scope"), v["command"], v.get("args")) for v in keys.values() if v.get("scope")}
        self.assertEqual(scoped["y"], ("pane", "client.copy", None))
        self.assertEqual(scoped["i"], ("pane", "client.composer", {"action": "focus"}))
        self.assertEqual(scoped["t"], ("pane", "agents.view", {"set": "thinking", "value": "cycle"}))
        self.assertEqual(scoped["M-s"], ("composer", "client.composer", {"action": "send", "set": {"mode": "boundary"}}))
        self.assertTrue(all(v["when"] == {"resource_kind": "agent"} for v in keys.values() if v.get("scope")))
        self.feed(snapshot(CONVERSATION))
        content = self.core.live["agents-r2"]
        self.assertTrue(content["search"])
        sender = next(r for r in content["items"] if r.get("id") == "m:m2")
        self.assertEqual((sender["focusable"], sender["copy"]), (True, "Let me look.\nFirst the CI log."))
        group = next(r for r in content["items"] if str(r.get("id", "")).startswith("g:"))
        self.assertEqual(group["children"][1]["copy"].splitlines()[0], "line 0 of the test log", "a tool row copies its output")
        self.assertEqual(json.loads(group["children"][0]["copy"]), {"path": "ci.yml"}, "or its input when there is no output")
        colours = agent_transcript.tool_row("t9", {"status": "completed", "content": text("\x1b[31mred\x1b[0m \x1b]0;title\x07ok")})
        self.assertEqual(colours["copy"], "red ok", "copied without terminal colour codes")
        for expected in ("summary", "hide", "show"):
            self.core.handlers["agents.view"]({"context": {"view": "agents-r2"}, "set": "thinking", "value": "cycle"})
            self.assertEqual(self.agents.view_of("r2")["thinking"], expected)

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
