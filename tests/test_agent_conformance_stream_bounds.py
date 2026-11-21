"""Self-tests for the Part A O12/O14 checkers: each must reject the false-pass counterexamples."""
import importlib
import unittest

import agent_conformance.cases  # noqa: F401  (load the shared registry first; then the row module)

m = importlib.import_module("agent_conformance.stream_bound_cases")


class NoReattachLoop(unittest.TestCase):
    def test_bounded_backoff_passes(self):
        self.assertTrue(m.no_reattach_loop([0.0, 2.0, 6.0]))
        self.assertTrue(m.no_reattach_loop([0.0]))

    def test_immediate_reattach_loop_fails(self):
        self.assertFalse(m.no_reattach_loop([0.0, 0.05, 0.1, 0.15, 0.2]), "a tight loop")
        self.assertFalse(m.no_reattach_loop([round(0.15 * i, 2) for i in range(150)]), "the observed production loop")
        self.assertFalse(m.no_reattach_loop([0.0, 1.5, 3.0, 4.5]), "too many opens in the window")
        self.assertTrue(m.no_reattach_loop([0.0, 0.15]), "one immediate retry then a stop is not a loop")
        self.assertFalse(m.no_reattach_loop([]), "no open at all is not evidence")


class CursorNeverAdvanced(unittest.TestCase):
    def test_same_or_reset_passes(self):
        self.assertTrue(m.cursor_never_advanced([None, {"epoch": "e", "seq": 1}], 1, "e"))

    def test_skipping_the_bad_record_fails(self):
        self.assertFalse(m.cursor_never_advanced([{"epoch": "e", "seq": 2}], 1, "e"), "advanced past the bad record")
        self.assertFalse(m.cursor_never_advanced([{"epoch": "other", "seq": 1}], 1, "e"), "foreign epoch")
        self.assertFalse(m.cursor_never_advanced([{"epoch": "e"}], 1, "e"), "malformed cursor")


class IncompatibilityReported(unittest.TestCase):
    def test_named_cause_and_limit_passes(self):
        content = [{"type": "text", "text": "This agent's history is larger than this pane can hold (4096 items, 4 MiB), so the conversation stopped updating."}]
        self.assertTrue(m.incompatibility_reported(content, ["larger|over_bound"], ["4096|4 mib"]))

    def test_silent_or_causeless_fails(self):
        self.assertFalse(m.incompatibility_reported([{"type": "text", "text": "Loading the conversation…"}], ["bad_record|malformed"]))
        self.assertFalse(m.incompatibility_reported([{"type": "text", "text": "The conversation stopped updating."}], ["bad_record|malformed"]),
                         "incompatible but no cause")
        self.assertFalse(m.incompatibility_reported([{"type": "text", "text": "stopped updating: too large"}], ["larger|too large"], ["4096"]),
                         "a bound cause without the limit")


class NoPartialCut(unittest.TestCase):
    def test_marker_absent_passes_present_fails(self):
        self.assertTrue(m.no_partial_cut([{"text": "O14-last-good"}], ["O14-PARTIAL-CUT-"]))
        self.assertFalse(m.no_partial_cut([{"text": "O14-PARTIAL-CUT-xxxx"}], ["O14-PARTIAL-CUT-"]))


class AdapterPreservesOrMarks(unittest.TestCase):
    def setUp(self):
        import hashlib
        self.text = "0:" + "x" * 70000
        self.bytes, self.sha = len(self.text.encode()), hashlib.sha256(self.text.encode()).hexdigest()

    def test_whole_or_split_or_marked_passes(self):
        self.assertTrue(m.adapter_preserves_or_marks([self.text], self.bytes, self.sha))
        self.assertTrue(m.adapter_preserves_or_marks([self.text[:40000], self.text[40000:]], self.bytes, self.sha))
        self.assertTrue(m.adapter_preserves_or_marks([self.text[:60000], "[message truncated: too large]"], self.bytes, self.sha))

    def test_silent_truncation_fails(self):
        self.assertFalse(m.adapter_preserves_or_marks([self.text[:60000]], self.bytes, self.sha), "silently cut at the bound")
        self.assertFalse(m.adapter_preserves_or_marks([self.text[:-1] + "y"], self.bytes, self.sha), "same length, altered text")
        self.assertFalse(m.adapter_preserves_or_marks([], self.bytes, self.sha), "nothing at all")


class ReviewCounterexamples(unittest.TestCase):
    """prim-agent-ux HOLD on dff775ae: each false green / false red is now rejected / accepted."""

    def test_over_bound_rejected_cut_is_atomic(self):
        grade = m.SCENARIO_GRADES["o14-over-bound"]
        # A viewer that installs the rejected snapshot's first entity and then continues after seq 1.
        pane = {"items": [{"type": "text", "text": "O14-REJECTED-CUT-SIBLING"}]}
        self.assertFalse(m.no_partial_cut(pane, grade["forbidden"]), "sibling of the rejected cut installed")
        self.assertFalse(m.cursor_never_advanced([None, {"epoch": "o14-e1", "seq": 1}], grade["last_good_seq"], "o14-e1"),
                         "continued past a snapshot that was never committed")
        self.assertTrue(m.cursor_never_advanced([None, None], grade["last_good_seq"], "o14-e1"))
        sibling = [r for r in m.O14_SCENARIOS["o14-over-bound"]["snapshot"] if r["id"] == "m1"][0]
        self.assertIn("O14-REJECTED-CUT-SIBLING", sibling["value"]["content"][0]["text"], "every rejected record is forbidden")

    def test_hidden_diagnostic_is_not_a_visible_report(self):
        hidden = {"items": [{"type": "text", "text": "Loading conversation",
                             "hidden_diagnostic": "incompatible bad_record unknown entity larger than 3000"}],
                  "copy": "incompatible"}
        for grade in m.SCENARIO_GRADES.values():
            self.assertFalse(m.incompatibility_reported(hidden, grade["cause"], grade["limit"]))
        shown = {"items": [{"type": "text", "style": "warn",
                            "text": "Incompatible stream: an entity is larger than 3000 bytes"}]}
        g = m.SCENARIO_GRADES["o14-over-bound"]
        self.assertTrue(m.incompatibility_reported(shown, g["cause"], g["limit"]))

    def test_label_on_a_text_row_is_not_visible(self):
        g = m.SCENARIO_GRADES["o14-over-bound"]
        labelled = {"items": [{"type": "text", "text": "Loading conversation",
                               "label": "Incompatible stream: an entity larger than 3000 bytes"}]}
        self.assertFalse(m.incompatibility_reported(labelled, g["cause"], g["limit"]))
        described = {"items": [{"type": "text", "text": "Loading conversation",
                                "description": "Incompatible stream: an entity larger than 3000 bytes"}]}
        self.assertFalse(m.incompatibility_reported(described, g["cause"], g["limit"]), "text rows draw no description")
        action = {"items": [{"type": "action", "text": "Incompatible stream",
                             "description": "an entity larger than 3000 bytes"}]}
        self.assertTrue(m.incompatibility_reported(action, g["cause"], g["limit"]), "non-text rows draw description")

    def test_o12_hidden_metadata_is_not_a_marker(self):
        from types import SimpleNamespace
        import hashlib
        text = "0:" + "O12-" + "x" * 70000
        n, sha = len(text.encode()), hashlib.sha256(text.encode()).hexdigest()
        entities = {("message", "a"): {"rev": 1, "value": {"content": [{"type": "text", "text": text[:60000]}],
                                                           "native": {"note": "tail omitted: too large"}}},
                    ("notice", "n"): {"rev": 1, "value": {"native": {"reason": "output omitted"}, "id": "truncated"}}}
        texts, others = m.message_texts(SimpleNamespace(entities=entities), "O12-", {})
        self.assertFalse(m.adapter_preserves_or_marks(texts, n, sha, other_texts=others), "hidden metadata only")

    def test_o12_separate_marker_block_and_omission_entity_pass(self):
        from types import SimpleNamespace
        import hashlib
        text = "0:" + "O12-" + "x" * 70000
        n, sha = len(text.encode()), hashlib.sha256(text.encode()).hexdigest()
        def reducer(entities): return SimpleNamespace(entities=entities)
        body = {"role": "assistant", "content": [{"type": "text", "text": text[:60000]},
                                                 {"type": "text", "text": "[message truncated: too large]"}]}
        texts, others = m.message_texts(reducer({("message", "a"): {"rev": 1, "value": body}}), "O12-", {})
        self.assertTrue(m.adapter_preserves_or_marks(texts, n, sha, other_texts=others), "truncated body + marker block")
        omitted = {("message", "a"): {"rev": 1, "value": {"content": [{"type": "text", "text": text[:60000]}]}},
                   ("notice", "n"): {"rev": 1, "value": {"content": [{"type": "text", "text": "assistant output omitted: too large"}]}},
                   ("message", "old"): {"rev": 1, "value": {"content": [{"type": "text", "text": "too large earlier"}]}}}
        texts, others = m.message_texts(reducer(omitted), "O12-", {("message", "old"): ["too large earlier"]})
        self.assertTrue(m.adapter_preserves_or_marks(texts, n, sha, other_texts=others), "omission-only entity")
        silent = {("message", "a"): {"rev": 1, "value": {"content": [{"type": "text", "text": text[:60000]}]}},
                  ("message", "old"): {"rev": 1, "value": {"content": [{"type": "text", "text": "too large earlier"}]}}}
        texts, others = m.message_texts(reducer(silent), "O12-", {("message", "old"): ["too large earlier"]})
        self.assertFalse(m.adapter_preserves_or_marks(texts, n, sha, other_texts=others), "old marker is not this refusal")


class ViewerPin(unittest.TestCase):
    """Only a viewer bound to trusted source is conformance authority: viewer_revision is a full commit reachable from
    trusted main and every copied *.py equals both its pin and its blob at that commit. Any refusal leaves nothing
    copied or enabled."""
    REV = "fc916ddd329fdde8433970cc421cd3573abb11ed"  # on main; its extensions/ is the real production viewer

    def run_install(self, edit=None, **manifest):
        import asyncio, hashlib, tempfile
        from pathlib import Path
        from types import SimpleNamespace
        from agent_conformance import evidence
        from agent_conformance.results import HarnessGap
        with tempfile.TemporaryDirectory() as tmp:
            source, root = Path(tmp) / "viewer", Path(tmp) / "root"
            source.mkdir(); (root / "ext").mkdir(parents=True)
            try:
                names = evidence.git(evidence.HARNESS_REPO, "ls-tree", "--name-only", self.REV, "extensions/").decode().split()
            except HarnessGap:
                # A source archive without Git cannot hold a trusted revision; the handler itself then reports a
                # HarnessGap, which these tests cannot exercise from here.
                self.skipTest("trusted Git revision " + self.REV[:12] + " is not available in this source tree")
            for path in names:
                if path.endswith(".py"):
                    (source / Path(path).name).write_bytes(evidence.git(evidence.HARNESS_REPO, "show", f"{self.REV}:{path}"))
            if edit: edit(source)
            pins = {p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in source.glob("*.py")}
            fields = {"viewer_extensions": str(source), "viewer_revision": self.REV, "viewer_sha256": pins}
            fields.update({key: value(pins) if callable(value) else value for key, value in manifest.items()})
            fields = {key: value for key, value in fields.items() if value is not None}
            calls = []
            async def call(method, params): calls.append(method); return {}
            ctx = SimpleNamespace(root=root, manifest=fields, evidence=[], peer=SimpleNamespace(call=call))
            try:
                asyncio.run(m.install_production_viewer(ctx))
                result = "installed"
            except HarnessGap as gap:
                result = "gap: " + str(gap)
            return result, calls, sorted(p.name for p in (root / "ext").iterdir()), len(pins)

    def test_viewer_bound_to_trusted_revision_installs(self):
        result, calls, copied, count = self.run_install()
        self.assertEqual((result, calls), ("installed", ["workspace.apply"]))
        self.assertIn("agents.py", copied); self.assertEqual(len(copied), count)
        self.assertFalse([name for name in copied if name.endswith(".o14-staged")])

    def test_untrusted_or_edited_viewer_is_refused_with_nothing_copied(self):
        from pathlib import Path
        def edit(source): (source / "agents.py").write_bytes(b"print('caller edited, hashes re-pinned')\n")
        def extra(source): (source / "zz_extra.py").write_bytes(b"EXTRA = 1\n")
        cases = {
            "no pins": dict(viewer_sha256=None),
            "no revision": dict(viewer_revision=None),
            "short revision": dict(viewer_revision=self.REV[:8]),
            "revision not on trusted main": dict(viewer_revision="0" * 40),
            "edited file with self-consistent re-pinned hashes": dict(edit=edit),
            "extra file not in the revision": dict(edit=extra),
            "file differs from its pin": dict(viewer_sha256=lambda pins: dict(pins, **{"agents.py": "0" * 64})),
        }
        for name, kwargs in cases.items():
            with self.subTest(name):
                result, calls, copied, _ = self.run_install(**kwargs)
                self.assertTrue(result.startswith("gap"), result)
                self.assertEqual((calls, copied), ([], []), "nothing copied or enabled")


class WholeHandlerControls(unittest.TestCase):
    """Complete-handler controls (prim-agent-ux's four from the dff775ae HOLD, plus more): the real O14/O12 handlers
    run against mocked transport, provider log and pane, and accept exactly the right outcomes."""

    def o14(self, rows_for, pane_for):
        import asyncio
        from pathlib import Path
        from types import SimpleNamespace
        from unittest.mock import AsyncMock, patch
        refs = list(m.SCENARIO_GRADES); ids = {ref: "r" + str(i) for i, ref in enumerate(refs)}
        class Watch:
            def on(self, method, fn): self.fn = fn
            async def call(self, method, params):
                for ref in refs: self.fn({"type": "view.content", "data": {"view": "agents-" + ids[ref], "content": pane_for(ref)}})
                return {}
        async def call(method, params): return {"resources": [{"backend_ref": ref, "id": rid} for ref, rid in ids.items()]}
        ctx = SimpleNamespace(evidence=[], peer=SimpleNamespace(call=call), viewer=AsyncMock(return_value=Watch()))
        rows = [row for ref in refs for row in rows_for(ref)]
        with patch.object(m, "install_production_viewer", AsyncMock(return_value=Path("/unused"))), \
             patch.object(m, "start_scripted_provider", AsyncMock(return_value=(object(), "/unused"))), \
             patch.object(m, "stop_provider", lambda ctx, process: None), \
             patch.object(m, "provider_log", lambda path: rows), patch.object(m.asyncio, "sleep", AsyncMock()):
            asyncio.run(m.incompatible_viewers(ctx))

    @staticmethod
    def report(ref):
        cause = {"o14-bad-record": "a bad_record", "o14-unknown-op": "an unknown operation"}.get(ref, "an entity larger than 3000 bytes")
        return {"items": [{"type": "text", "style": "warn", "text": "Incompatible stream: " + cause}]}

    def accepts(self, fn):
        from agent_conformance.results import CheckFailed
        try: fn(); return True
        except CheckFailed: return False

    def test_o14_controls(self):
        reset_once = lambda ref: [{"kind": "open", "ref": ref, "after": None, "t": 0.0}]
        def advanced(ref):
            return reset_once(ref) + ([{"kind": "open", "ref": ref, "after": {"epoch": "o14-e1", "seq": 1}, "t": 2.0}]
                                      if ref == "o14-over-bound" else [])
        def sibling(ref):
            pane = self.report(ref)
            if ref == "o14-over-bound": pane["items"].append({"type": "text", "text": "O14-REJECTED-CUT-SIBLING"})
            return pane
        hidden = lambda ref: {"items": [{"type": "text", "text": "Loading conversation",
                                         "hidden_diagnostic": self.report(ref)["items"][0]["text"]}]}
        label = lambda ref: {"items": [{"type": "text", "text": "Loading conversation",
                                        "label": self.report(ref)["items"][0]["text"]}]}
        loop = lambda ref: [{"kind": "open", "ref": ref, "after": None, "t": 0.15 * i} for i in range(60)]
        separator = lambda ref: {"items": [{"type": "separator", "text": self.report(ref)["items"][0]["text"]}]}
        chips_row = lambda ref: {"items": [{"type": "chips", "text": self.report(ref)["items"][0]["text"],
                                            "chips": [{"text": "Loading"}]}]}
        default_desc = lambda ref: {"items": [{"text": "Incompatible stream",
                                               "description": self.report(ref)["items"][0]["text"]}]}
        collapsed = lambda ref: {"items": [{"type": "header", "text": "Status", "expanded": False,
                                            "children": [self.report(ref)["items"][0]]}]}
        empty_message = lambda ref: {"items": [{"type": "message", "title": "", "body": "",
                                                "text": self.report(ref)["items"][0]["text"]}]}
        typed_select = lambda ref: {"items": [{"type": "select", "text": "Status", "value": 1, "options": [
            {"value": True, "label": self.report(ref)["items"][0]["text"]}, {"value": 1, "label": "Loading"}]}]}
        message_fallback = lambda ref: {"items": [{"type": "message", "text": self.report(ref)["items"][0]["text"]}]}
        empty_title_visible_body = lambda ref: {"items": [{"type": "message", "title": "", "text": "Loading",
                                                           "body": self.report(ref)["items"][0]["text"]}]}
        typed_select_visible = lambda ref: {"items": [{"type": "select", "text": "Status", "value": 1, "options": [
            {"value": True, "label": "Loading"}, {"value": 1, "label": self.report(ref)["items"][0]["text"]}]}]}
        null_select = lambda ref: {"items": [{"type": "select", "text": "Status", "value": None, "options": [
            {"label": "Loading"}, {"value": None, "label": self.report(ref)["items"][0]["text"]}]}]}
        missing_value_select = lambda ref: {"items": [{"type": "select", "text": "Status", "options": [
            "not-an-object", {"value": None, "label": self.report(ref)["items"][0]["text"]}]}]}
        null_select_visible = lambda ref: {"items": [{"type": "select", "text": "Status", "value": None, "options": [
            {"label": self.report(ref)["items"][0]["text"]}, {"value": None, "label": "Loading"}]}]}
        warn = lambda ref: self.report(ref)["items"][0]["text"]
        replaced_by_fallback = lambda ref: {"items": [{"type": "x.future", "requires": ["x.future"], "text": warn(ref),
                                                       "fallback": {"type": "text", "text": "Loading"}}]}
        dropped_stand_in = lambda ref: {"items": [{"type": "text", "requires_absent": ["composer_buttons"], "text": warn(ref)}]}
        fallback_visible = lambda ref: {"items": [{"type": "x.future", "requires": ["x.future"], "text": "Loading",
                                                   "fallback": {"type": "text", "text": warn(ref)}}]}
        unrenderable_panel = lambda ref: {"requires": ["x.future-panel"], "items": [{"type": "text", "text": warn(ref)}]}
        controls = {
            "valid back-off and visible cause/limit": (reset_once, self.report, True),
            "raw warning the client replaces with its Loading fallback": (reset_once, replaced_by_fallback, False),
            "stand-in warning the client drops (requires_absent)": (reset_once, dropped_stand_in, False),
            "warning visible only through the client's fallback": (reset_once, fallback_visible, True),
            "panel whose own requires the client lacks": (reset_once, unrenderable_panel, False),
            "select null: an option without value matches first (missing reads as Null)": (reset_once, null_select, False),
            "select without value: a non-object option matches first": (reset_once, missing_value_select, False),
            "select null: the first (valueless) option shows the cause/limit": (reset_once, null_select_visible, True),
            "message with empty title and the cause/limit in its body": (reset_once, empty_title_visible_body, True),
            "select whose typed-equal option shows the cause/limit": (reset_once, typed_select_visible, True),
            "message with empty title and body hides its text (Option::or)": (reset_once, empty_message, False),
            "select picks the typed-equal option, not true == 1": (reset_once, typed_select, False),
            "message without title or body shows its text": (reset_once, message_fallback, True),
            "cause/limit in a default item's description (drawn)": (reset_once, default_desc, True),
            "cause/limit only in a separator's text (never drawn)": (reset_once, separator, False),
            "cause/limit only in a chips row's own text (only chips drawn)": (reset_once, chips_row, False),
            "cause/limit only in a collapsed row's children": (reset_once, collapsed, False),
            "rejected reset continued after seq 1": (advanced, self.report, False),
            "rejected reset's sibling entity installed": (reset_once, sibling, False),
            "cause/limit only in hidden metadata": (reset_once, hidden, False),
            "cause/limit only in a text row's label (never drawn)": (reset_once, label, False),
            "immediate reattach loop": (loop, self.report, False),
        }
        for name, (rows, pane, want) in controls.items():
            with self.subTest(name):
                self.assertEqual(self.accepts(lambda: self.o14(rows, pane)), want)

    def o12(self, new_entities, old_entities=None):
        import asyncio, hashlib
        from types import SimpleNamespace
        from unittest.mock import AsyncMock, patch
        from agent_conformance import evidence
        from agent_conformance.results import CheckFailed
        text = "0:" + "".join("O12-" + "x" * 1020 for _ in range(70))
        reducer = SimpleNamespace(snapshots=1, entities=dict(old_entities or {}))
        class Native:
            def mark(self): return 0
            async def call(self, *args, **kw): reducer.entities.update(new_entities(text))
            def records(self, *args, **kw):
                return [{"type": "message_end", "role": "assistant", "text_bytes": len(text.encode()),
                         "text_sha256": hashlib.sha256(text.encode()).hexdigest()}]
        async def wait_view(view, predicate, **kw):
            if not predicate(view.reducer): raise CheckFailed("not reached")
        ctx = SimpleNamespace(evidence=[], native=Native(), wait_view=wait_view,
                              attach=AsyncMock(return_value=SimpleNamespace(initial="reset", reducer=reducer)))
        # Inherited producer evidence is stubbed ONLY to isolate the adapter half; it proves nothing here.
        with patch.dict(evidence.REGRESSIONS, {"O12": {}}), patch.object(evidence, "component", lambda *a: ({}, None, None, None)), \
             patch.object(evidence, "regressions", lambda *a: None), patch.object(evidence, "inherited", lambda *a: None):
            asyncio.run(m.producer_append_bounds(ctx))

    def test_o12_controls(self):
        def message(rev, position, *texts):
            return {"rev": rev, "value": {"role": "assistant", "position": position,
                                          "content": [{"type": "text", "text": t} for t in texts]}}
        old_marker = {("message", "old"): message(1, 1, "[an earlier message was truncated: too large]")}
        def old_output(t):  # an earlier O12-prefixed body and marker, already present before this emission
            return {("message", "prev"): message(1, 1, t[:60000], "[message truncated: too large]")}
        def hidden_only(t):
            entity = message(1, 2)
            entity["value"]["native"] = {"diagnostic": "assistant output omitted: too large"}
            return {("message", "m"): entity}
        def image_extra_marker(t):  # production content_rows shows only "[image: image/png]" for this block
            entity = message(1, 2, t[:60000])
            entity["value"]["content"].append({"type": "image", "mime": "image/png", "data": "AA==",
                                               "text": "[output truncated: too large]"})
            return {("message", "m"): entity}
        old_marker_rev_bumped = {("message", "old"): message(1, 1, "[earlier output was truncated: too large]")}
        def bump_old_marker_only(t):
            return {("message", "old"): message(2, 1, "[earlier output was truncated: too large]")}
        controls = {
            "whole text in one entity": (lambda t: {("message", "m"): message(1, 2, t)}, None, True),
            "marker only in an image block's extra text (never shown)": (image_extra_marker, None, False),
            "rev-bumped old marker, no O12 output": (bump_old_marker_only, old_marker_rev_bumped, False),
            "rev-bumped old marker beside silent truncation": (lambda t: {**bump_old_marker_only(t),
                ("message", "m"): message(1, 2, t[:60000])}, old_marker_rev_bumped, False),
            "truncated body plus separate marker block": (lambda t: {("message", "m"): message(1, 2, t[:60000], "[message truncated: too large]")}, None, True),
            "truncated body plus omission-only entity": (lambda t: {("message", "m"): message(1, 2, t[:60000]),
                                                                     ("notice", "n"): {"rev": 1, "value": {"content": [{"type": "text", "text": "output omitted: too large"}]}}}, None, True),
            "split across entities whose ids sort against the timeline": (lambda t: {("message", "b"): message(1, 2, t[:40000]),
                                                                                       ("message", "a"): message(1, 3, t[40000:])}, None, True),
            "silent truncation": (lambda t: {("message", "m"): message(1, 2, t[:60000])}, None, False),
            "silent truncation beside an older marker": (lambda t: {("message", "m"): message(1, 2, t[:60000])}, old_marker, False),
            "empty visible message, marker only in native metadata": (hidden_only, None, False),
            "no new output; unchanged old O12 body and marker": (lambda t: {}, "old_output", False),
        }
        for name, (new, old, want) in controls.items():
            if old == "old_output":
                old = old_output("0:" + "".join("O12-" + "x" * 1020 for _ in range(70)))
            with self.subTest(name):
                self.assertEqual(self.accepts(lambda: self.o12(new, old)), want)


if __name__ == "__main__":
    unittest.main()
