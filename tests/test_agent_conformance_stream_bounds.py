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
        self.assertTrue(m.adapter_preserves_or_marks([self.text[:60000]], self.bytes, self.sha,
                                                     other_texts=["[message truncated: too large]"]))
        self.assertFalse(m.adapter_preserves_or_marks([self.text[:60000], "[message truncated: too large]"], self.bytes,
                                                      self.sha), "a marker in the RAW body never counts")

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
        old_seen = {("message", "old"): {"shown": ["too large earlier"], "body": m.body_fingerprint(omitted[("message", "old")]["value"])}}
        texts, others = m.message_texts(reducer(omitted), "O12-", old_seen)
        self.assertTrue(m.adapter_preserves_or_marks(texts, n, sha, other_texts=others), "omission-only entity")
        silent = {("message", "a"): {"rev": 1, "value": {"content": [{"type": "text", "text": text[:60000]}]}},
                  ("message", "old"): {"rev": 1, "value": {"content": [{"type": "text", "text": "too large earlier"}]}}}
        texts, others = m.message_texts(reducer(silent), "O12-", old_seen)
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

    def o12(self, new_entities, old_entities=None, old_notices=(), new_notices=(), *, wait_error=None,
            health_fault=None, evidence_out=None, new_epoch=None, wait_returns_early=False, republish_flag=False,
            fresh_challenge=False, native_mismatch=False, no_frontier=False):
        import asyncio, hashlib
        from types import SimpleNamespace
        from unittest.mock import AsyncMock, patch
        from agent_conformance import evidence
        from agent_conformance.results import CheckFailed
        clock = SimpleNamespace(now=0.0)
        chunks, needle = m.o12_emission_chunks() if fresh_challenge else (["O12-" + "x" * 1020 for _ in range(70)], "O12-")
        text = "0:" + "".join(chunks)
        from collections import deque
        # Faithful to wire.ItemReducer: live notices live in a bounded deque(maxlen=64).
        reducer = SimpleNamespace(snapshots=1, entities=dict(old_entities or {}), notices=deque(old_notices, maxlen=64),
                                  epoch='o12-epoch', sequence=1, cursor={'epoch': 'o12-epoch', 'seq': 1},
                                  failure=None, staged=None, pending=bytearray(), trace=[], trace_dropped=0)
        def accept(row):  # Minimal installed-live-put semantics; real NDJSON is tested separately below.
            if row.get('op') != 'put' or row.get('seq', -1) <= reducer.sequence:
                return
            reducer.entities[(row['kind'], row['id'])] = {'rev': row['rev'], 'value': dict(row['value'])}
            reducer.sequence = row['seq']
            reducer.cursor = {'epoch': reducer.epoch, 'seq': reducer.sequence}
        reducer.accept = accept
        peer = SimpleNamespace(failure=None, closed=False, task=SimpleNamespace(done=lambda: False))
        view = SimpleNamespace(initial='reset', reducer=reducer, peer=peer, stream=1, closed=None, close_notice=None)
        class Native:
            def mark(self): return 0
            async def call(self, *args, **kw):
                rows = new_entities(text)
                for key, row in rows.items():
                    if key[0] == 'message':
                        row['value'].setdefault('final', True)  # This fixture emits a completed native message.
                for key, row in rows.items():
                    if republish_flag and row['value'].get('incomplete') is True:
                        reducer.accept({'op': 'put', 'seq': reducer.sequence + 1, 'kind': key[0], 'id': key[1],
                                        'rev': row['rev'], 'value': dict(row['value'])})
                    else:
                        reducer.entities[key] = row
                reducer.notices.extend(new_notices)
                self.emitted = True
                if health_fault == 'flag_loss':
                    for i in range(m.AppliedFlagPuts.MAX + 1):
                        reducer.accept({'op': 'put', 'seq': reducer.sequence + 1, 'kind': 'message', 'id': 'lost-' + str(i),
                                        'rev': 1, 'value': {'incomplete': True, 'content': []}})
                if new_epoch is not None:
                    reducer.epoch = new_epoch
                    reducer.cursor = {'epoch': new_epoch, 'seq': 1}
                    reducer.snapshots += 1
                if health_fault == 'unknown_view': del view.closed
                if health_fault == 'unknown_peer': del peer.failure
                if health_fault == 'peer_task': peer.task = SimpleNamespace(done=lambda: True)
                if health_fault == 'peer': peer.failure = RuntimeError('connection interrupted')
                if health_fault == 'closed': peer.closed = True
                if health_fault == 'reducer': reducer.failure = 'record reconstruction failed'
                if health_fault == 'staged': reducer.staged = {}
                if health_fault == 'terminal':
                    view.closed = 'retention'
                    view.close_notice = {'method': 'stream.gap', 'reason': {'reason': 'retention'},
                                         'last_good_cursor': dict(reducer.cursor)}
            def records(self, *args, **kw):
                return [{"type": "message_end", "role": "assistant", "text_bytes": len(text.encode()),
                         "text_sha256": hashlib.sha256((text + 'wrong' if native_mismatch else text).encode()).hexdigest()}]
        async def wait_view(view, predicate, **kw):
            observed = predicate(view.reducer)
            if wait_error is not None and getattr(ctx.native, 'emitted', False): raise wait_error
            if not observed and not wait_returns_early:
                clock.now = 15.0
                raise CheckFailed("item stream did not reach expected state") from asyncio.TimeoutError()
        from copy import deepcopy
        async def attach(*, peer=None, after=None, seed=None):
            self.assertIsNone(after); self.assertIsNone(seed)
            if not getattr(ctx.native, 'emitted', False): return view
            if no_frontier: raise RuntimeError('no witnessed public frontier')
            self.assertIs(peer, view.peer)
            fresh = SimpleNamespace(**vars(reducer))
            fresh.entities, fresh.notices = deepcopy(reducer.entities), deque(maxlen=64)
            fresh.accept = lambda row: None
            return SimpleNamespace(initial='reset', reducer=fresh, peer=peer, stream=2, closed=None, close_notice=None)
        ctx = SimpleNamespace(evidence=evidence_out if evidence_out is not None else [], native=Native(), wait_view=wait_view,
                              attach=attach)
        # Inherited producer evidence is stubbed ONLY to isolate the adapter half; it proves nothing here.
        with patch.dict(evidence.REGRESSIONS, {"O12": {}}), patch.object(evidence, "component", lambda *a: ({}, None, None, None)), \
             patch.object(evidence, "regressions", lambda *a: None), patch.object(evidence, "inherited", lambda *a: None), \
             patch.object(m, 'o12_emission_chunks', lambda: (chunks, needle)), \
             patch.object(m, 'time', SimpleNamespace(monotonic=lambda: clock.now)):
            try:
                asyncio.run(m.producer_append_bounds(ctx))
            finally:
                self.assertIs(reducer.accept, accept, 'case observer must restore the exact original on every exit')
        return ctx.evidence

    def test_o12_fresh_flag_and_completed_unmarked_results(self):
        from agent_conformance.results import CheckFailed, HarnessGap
        def row(text, *, flag=None, rev=1, final=True):
            value = {'role': 'assistant', 'position': 1, 'content': [{'type': 'text', 'text': text}], 'final': final}
            if flag is not None: value['incomplete'] = flag
            return {'rev': rev, 'value': value}
        results = self.o12(lambda t: {('message', 'm'): row(t[:6000], flag=True)})
        record = results[-1]
        with self.assertRaisesRegex(HarnessGap, 'flag observation gap'):
            self.o12(lambda t: {('message', 'm'): row(t[:6000], flag=True)}, health_fault='flag_loss')
        self.assertEqual(len(record['incomplete_witnesses']), 1)
        self.assertIs(record['incomplete_witnesses'][0]['value']['incomplete'], True)
        self.assertIsNone(record['observation']['wait_error'])
        self.assertTrue(record['observation']['peer_known'])
        text = '0:' + ''.join('O12-' + 'x' * 1020 for _ in range(70))
        old = {('message', 'm'): row(text[:1000], flag=True)}
        for name, new, before in (
                ('stale flag', lambda t: {('message', 'm'): row(t[:6000], flag=True, rev=2)}, old),
                ('no flag or marker', lambda t: {('message', 'm'): row(t[:6000])}, None),
                ('numeric is not boolean', lambda t: {('message', 'm'): row(t[:6000], flag=1)}, None),
                ('unrelated flag', lambda t: {('message', 'm'): row(t[:6000]),
                                             ('message', 'other'): row('unrelated', flag=True)}, None)):
            with self.subTest(name=name), self.assertRaises(CheckFailed) as raised:
                self.o12(new, before, no_frontier=name == 'stale flag')
            if name == 'stale flag':
                self.assertIsInstance(raised.exception, HarnessGap)  # Held true without a new public cut is never PASS.
            else:
                self.assertNotIsInstance(raised.exception, HarnessGap)
                self.assertEqual(raised.exception.component, 'adapter')
        # The same opaque ID in a genuinely new epoch is a new entity, not a stale flag.
        self.o12(lambda t: {('message', 'm'): row(t[:6000], flag=True)}, old, new_epoch='o12-new-epoch')

    def test_o12_observation_failures_are_gaps_even_with_positive_witness(self):
        from agent_conformance.results import CheckFailed, HarnessGap
        def marked(t):
            return {('message', 'm'): {'rev': 1, 'value': {'role': 'assistant', 'final': True, 'incomplete': True,
                                                         'content': [{'type': 'text', 'text': t[:6000]}]}}}
        for fault in (None, 'peer', 'closed', 'reducer', 'staged', 'terminal', 'unknown_view', 'unknown_peer', 'peer_task'):
            retained = []
            with self.subTest(fault=fault), self.assertRaises(HarnessGap):
                self.o12(marked, health_fault=fault, evidence_out=retained,
                         wait_error=CheckFailed('forced wait failure') if fault is None else None)
            record = retained[-1]
            self.assertEqual(len(record['incomplete_witnesses']), 1)
            if fault is None:
                self.assertEqual(record['observation']['wait_error']['message'], 'forced wait failure')
            if fault == 'terminal':
                self.assertEqual(record['observation']['terminal']['reason'], {'reason': 'retention'})
        retained = []
        def held(t):
            return {('message', 'm'): {'rev': 1, 'value': {'role': 'assistant', 'final': False,
                                                         'content': [{'type': 'text', 'text': t[:6000]}]}}}
        with self.assertRaises(HarnessGap):
            self.o12(held, evidence_out=retained)
        self.assertIsNotNone(retained[-1]['observation']['wait_error'])
        retained = []
        with self.assertRaisesRegex(HarnessGap, 'held prefix'):
            self.o12(held, evidence_out=retained, wait_returns_early=True)
        self.assertIsNone(retained[-1]['observation']['wait_error'])
        # Even independently positive body/marker evidence cannot erase a caught wait failure.
        def whole(t):
            return {('message', 'm'): {'rev': 1, 'value': {'role': 'assistant', 'content': [{'type': 'text', 'text': t}]}}}
        def warning(t):
            return {('notice', 'n'): {'rev': 1, 'value': {'content': [{'type': 'text', 'text': 'output omitted: too large'}]}}}
        for positive in (whole, warning):
            retained = []
            with self.subTest(positive=positive.__name__), self.assertRaises(HarnessGap):
                self.o12(positive, wait_error=CheckFailed('interrupted after observation'), evidence_out=retained)
            self.assertEqual(retained[-1]['observation']['wait_error']['message'], 'interrupted after observation')

    def test_o12_flag_transition_is_public_fresh_and_correlated(self):
        from agent_conformance.results import CheckFailed, HarnessGap
        text = '0:' + ''.join('O12-' + 'x' * 1020 for _ in range(70))
        value = {'role': 'assistant', 'content': [{'type': 'text', 'text': text[:6000]}], 'final': True}
        old_value = dict(value, content=[{'type': 'text', 'text': text[:1000]}])
        old = {('message', 'm'): {'rev': 1, 'value': dict(old_value, incomplete=False)}}
        rows = self.o12(lambda t: {('message', 'm'): {'rev': 2, 'value': dict(value, incomplete=True)}}, old)
        self.assertTrue(rows[-1]['incomplete_witnesses'][0]['fresh_incomplete'])
        old_true = {('message', 'm'): {'rev': 1, 'value': dict(old_value, incomplete=True)}}
        republished = self.o12(lambda t: {('message', 'm'): {'rev': 2, 'value': dict(value, incomplete=True)}},
                               old_true, republish_flag=True)
        self.assertIs(republished[-1]['incomplete_witnesses'][0]['flag_put']['value']['incomplete'], True)
        with self.assertRaisesRegex(HarnessGap, 'flag observation gap'):
            self.o12(lambda t: {('message', 'm'): {'rev': 2, 'value': dict(value, incomplete=True)}},
                     old_true, health_fault='flag_loss', wait_returns_early=True)
        # The same value mutation without a new public revision is not a fresh publication.
        with self.assertRaises(HarnessGap):
            self.o12(lambda t: {('message', 'm'): {'rev': 1, 'value': dict(value, incomplete=True)}}, old)
        def user_flag(t):
            return {('message', 'm'): {'rev': 1, 'value': dict(value)},
                    ('message', 'user'): {'rev': 1, 'value': dict(value, role='user', incomplete=True)}}
        with self.assertRaises(CheckFailed) as raised:
            self.o12(user_flag)
        self.assertNotIsInstance(raised.exception, HarnessGap)

    def test_o12_applied_puts_exclude_ignored_duplicates_and_snapshot_replay(self):
        import json
        from copy import deepcopy
        from agent_conformance.wire import ItemReducer
        reducer = ItemReducer()
        def deliver(row):
            reducer.feed(reducer.offset, (json.dumps(row) + '\n').encode())
            self.assertIsNone(reducer.failure)
        value = {'role': 'assistant', 'position': 1, 'incomplete': True,
                 'content': [{'type': 'text', 'text': '0:O12-' + 'x' * 100}]}
        deliver({'op': 'snapshot', 'phase': 'begin', 'epoch': 'e', 'coverage': 'tail'})
        deliver({'op': 'put', 'kind': 'message', 'id': 'm', 'rev': 1, 'value': value})
        deliver({'op': 'snapshot', 'phase': 'end', 'seq': 1})
        original = reducer.accept
        recorder = m.AppliedFlagPuts(reducer)
        try:
            baseline = m.shown_baseline(reducer, applied_flags=recorder)
            deliver({'op': 'append', 'seq': 2, 'kind': 'message', 'id': 'm', 'base': 1, 'rev': 2,
                     'field': '/content/0/text', 'text': 'xx'})
            saved = deepcopy(reducer.entities)
            for seq, rev in ((1, 1), (2, 2)):
                deliver({'op': 'put', 'seq': seq, 'kind': 'message', 'id': 'm', 'rev': rev, 'value': value})
                self.assertEqual(reducer.entities, saved)
                self.assertIsNone(m.o12_flag_put(reducer, baseline, ('message', 'm')))
            published = dict(value, final=True)
            deliver({'op': 'put', 'seq': 3, 'kind': 'message', 'id': 'm', 'rev': 3, 'value': published})
            self.assertEqual(m.o12_flag_put(reducer, baseline, ('message', 'm'))['seq'], 3)
            deliver({'op': 'snapshot', 'phase': 'begin', 'epoch': 'new', 'coverage': 'tail'})
            deliver({'op': 'put', 'kind': 'message', 'id': 'm', 'rev': 1, 'value': value})
            deliver({'op': 'snapshot', 'phase': 'end', 'seq': 0})
            self.assertIsNone(m.o12_flag_put(reducer, baseline, ('message', 'm')))
            self.assertEqual(recorder.received, 1)
        finally:
            recorder.restore()
        self.assertEqual(reducer.accept, original)

    def test_o12_observer_preserves_original_call_return_exception_and_restore(self):
        from types import SimpleNamespace
        from copy import deepcopy
        sentinel, failure, calls = object(), RuntimeError('original failure'), []
        def original(row):
            calls.append(row)
            if row.get('fail'): raise failure
            return sentinel
        reducer = SimpleNamespace(epoch='e', sequence=1, staged=None, entities={}, accept=original)
        recorder = m.AppliedFlagPuts(reducer)
        row = {'op': 'ignored', 'payload': {'keep': True}}
        saved = deepcopy(row)
        try:
            self.assertIs(reducer.accept(row), sentinel)
            self.assertIs(calls[-1], row)
            self.assertEqual(row, saved)
            with self.assertRaises(RuntimeError) as raised:
                reducer.accept({'fail': True})
            self.assertIs(raised.exception, failure)
            self.assertEqual(len(calls), 2)
        finally:
            recorder.restore()
        self.assertIs(reducer.accept, original)

    def test_o12_current_emission_challenge_and_exact_prefix_binding(self):
        from agent_conformance.results import CheckFailed, HarnessGap
        a, needle_a = m.o12_emission_chunks()
        b, needle_b = m.o12_emission_chunks()
        self.assertNotEqual(needle_a, needle_b)
        self.assertEqual((len(a), len(b)), (70, 70))
        self.assertTrue(all(len(chunk.encode()) == 1024 for chunk in a + b))
        def row(text, flag=None):
            return {'rev': 1, 'value': {'role': 'assistant', 'final': True, 'incomplete': flag,
                                      'content': [{'type': 'text', 'text': text}]}}
        self.o12(lambda t: {('message', 'm'): row(t[:6000], True)}, fresh_challenge=True)
        for wrong in ('O12-not-the-emitted-message', 'unrelated label mentions O12-but is not emitted body'):
            with self.subTest(wrong=wrong), self.assertRaises(CheckFailed) as raised:
                self.o12(lambda t: {('message', 'm'): row(t[:6000]), ('message', 'other'): row(wrong, True)})
            self.assertNotIsInstance(raised.exception, HarnessGap)
        old_text = '0:' + ''.join('O12-' + 'x' * 1020 for _ in range(70))
        old = {('message', 'old'): row(old_text[:1000], True)}
        self.o12(lambda t: {('message', 'old'): row(t[:6000], True)}, old,
                 new_epoch='genuine-new-epoch', fresh_challenge=True)
        with self.assertRaises(CheckFailed) as raised:
            self.o12(lambda t: {('message', 'm'): row(t[:6000], True)}, fresh_challenge=True, native_mismatch=True)
        self.assertEqual(raised.exception.component, 'fixture')
        with self.assertRaises(HarnessGap):
            self.o12(lambda t: {('message', 'old'): row(old_text[:1000], True)}, old,
                     new_epoch='replayed-epoch', fresh_challenge=True)
        with self.assertRaises(HarnessGap):
            self.o12(lambda t: {('message', 'old'): row(old_text[:1000], True)}, old, new_epoch='replayed-epoch')
        split = row(old_text[:1000], True)
        split['value']['content'] = [{'type': 'text', 'text': old_text[:300]}, {'type': 'text', 'text': old_text[300:1000]}]
        with self.assertRaises(HarnessGap):
            self.o12(lambda t: {('message', 'old'): row(old_text[:1000], True)},
                     {('message', 'old'): split}, new_epoch='replayed-blocks')

    def test_o12_item_stream_ruling(self):
        """Shown-marker half of O12's public stream rule (fresh incomplete flags are tested separately).
        Bundled-pane rendering remains separate K3 work."""
        def message(rev, position, *blocks):
            return {"rev": rev, "value": {"role": "assistant", "position": position, "content": list(blocks)}}
        body = lambda t: {"type": "text", "text": t[:60000]}
        notice = {"severity": "warn", "text": "assistant output truncated: too large"}
        cases = {
            "live notice record with the marker (the pane shows none of it)": (lambda t: {("message", "m"): message(1, 1, body(t))}, {}, (), (notice,), True),
            "system message with the marker": (lambda t: {("message", "m"): message(1, 1, body(t)),
                ("message", "s"): {"rev": 1, "value": {"role": "system", "position": 2, "content": [{"type": "text", "text": "[output omitted: too large]"}]}}}, {}, (), (), True),
            "resource text block with the marker": (lambda t: {("message", "m"): message(1, 1, body(t), {"type": "resource", "uri": "x:", "text": "truncated: too large"})}, {}, (), (), True),
            "the same live notice delivered before the emission, nothing new": (lambda t: {("message", "m"): message(1, 1, body(t))}, {}, (notice,), (), False),
            "marker only in a resource_link name": (lambda t: {("message", "m"): message(1, 1, body(t), {"type": "resource_link", "uri": "x:", "name": "truncated"})}, {}, (), (), False),
            "marker only in OSC hyperlink metadata of a separate block": (lambda t: {("message", "m"): message(1, 1, body(t), {"type": "text",
                "text": "\x1b]8;;https://truncated.example/\x07link\x1b]8;;\x07"})}, {}, (), (), False),
            "marker only in an image block's extra text": (lambda t: {("message", "m"): message(1, 1, body(t), {"type": "image", "mime": "image/png",
                "data": "AA==", "text": "[output truncated: too large]"})}, {}, (), (), False),
        }
        for name, (new, old, before, after, want) in cases.items():
            with self.subTest(name):
                self.assertEqual(self.accepts(lambda: self.o12(new, old, before, after)), want)

    def test_o12_live_notice_freshness_is_by_receipt_in_the_bounded_cache(self):
        """prim-agent-ux HOLD on 4424b91b: with 64 identical old warnings cached, one new identical warning evicts one;
        value multisets are unchanged, but the notice was received after the emission and is fresh."""
        def message(rev, position, *blocks):
            return {"rev": rev, "value": {"role": "assistant", "position": position, "content": list(blocks)}}
        new = lambda t: {("message", "m"): message(1, 1, {"type": "text", "text": t[:60000]})}
        warn = {"severity": "warn", "text": "assistant output truncated: too large"}
        for old_count, new_notices, want in ((0, (warn,), True), (1, (warn,), True), (64, (warn,), True), (64, (), False)):
            with self.subTest(old=old_count, new=len(new_notices)):
                self.assertEqual(self.accepts(lambda: self.o12(new, {}, (warn,) * old_count, new_notices)), want)

    def test_o12_current_window_notices_survive_the_cache_evicting_them(self):
        """prim-agent-ux HOLD on 4d35be48 (28 probes): a fresh warning FIRST, then enough benign notices to evict it from
        the 64-entry cache, is still evidence: notices are recorded on receipt since the baseline."""
        def message(rev, position, *blocks):
            return {"rev": rev, "value": {"role": "assistant", "position": position, "content": list(blocks)}}
        new = lambda t: {("message", "m"): message(1, 1, {"type": "text", "text": t[:60000]})}
        warn = {"severity": "warn", "text": "assistant output omitted: too large"}
        info = {"severity": "info", "text": "progress update"}
        cases = [(0, [warn], True), (1, [warn], True), (64, [warn], True), (64, [], False)]
        for old in (0, 64):
            for n in (63, 64, 65, 128):
                cases += [(old, [warn] + [info] * (n - 1), True), (old, [info] * (n - 1) + [warn], True), (old, [info] * n, False)]
        self.assertEqual(len(cases), 28)
        for old, received, want in cases:
            with self.subTest(old=old, received=len(received), warn_first=bool(received) and received[0] is warn):
                self.assertEqual(self.accepts(lambda: self.o12(new, {}, [dict(warn) for _ in range(old)],
                                                              [dict(x) for x in received])), want)

    def test_o12_recorder_overflow_is_a_named_gap_never_a_fail(self):
        from agent_conformance.results import HarnessGap
        def message(rev, position, *blocks):
            return {"rev": rev, "value": {"role": "assistant", "position": position, "content": list(blocks)}}
        new = lambda t: {("message", "m"): message(1, 1, {"type": "text", "text": t[:60000]})}
        info = {"severity": "info", "text": "progress update"}
        with self.assertRaises(HarnessGap):
            self.o12(new, {}, [], [dict(info) for _ in range(m.ReceivedNotices.RECORD + 1)])

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
        full = "0:" + "".join("O12-" + "x" * 1020 for _ in range(70))
        old_seen_marker = "[earlier output was truncated: too large]"
        controls = {
            # prim-agent-ux HOLD on dfa1a85b: raw text never marks; raw freshness is separate from shown freshness.
            "fresh truncation cannot borrow the same entity's old marker": (
                lambda t: {("message", "m"): message(2, 1, t[:60000], old_seen_marker)},
                {("message", "m"): message(1, 1, full[:1000], old_seen_marker)}, False),
            "updated complete raw body whose clipped shown rows are unchanged": (
                lambda t: {("message", "m"): message(2, 1, t)}, {("message", "m"): message(1, 1, full[:50000])}, True),
            "unchanged complete raw history with a bumped revision": (
                lambda t: {("message", "m"): message(2, 1, t)}, {("message", "m"): message(1, 1, full)}, False),
            "newly shown current refusal marker": (
                lambda t: {("message", "m"): message(2, 1, t[:60000], "[current output omitted: too large]")},
                {("message", "m"): message(1, 1, full[:1000], old_seen_marker)}, True),
            # vqro-lead ruling: graded on the item stream (not the pane's clipping), so an inline warning after the body
            # in the same block is published display content (prim-agent-ux d39d3421 HOLD, class 1).
            "inline warning after the body in the same block": (
                lambda t: {("message", "m"): message(1, 1, t[:60000] + " [output truncated: too large]")}, None, True),
            "warning on its own line in the same block as the body": (
                lambda t: {("message", "m"): message(1, 1, t[:60000] + "\n[output truncated: too large]")}, None, True),
            "same-block old warning retained while the body freshly truncates": (
                lambda t: {("message", "m"): message(2, 1, t[:60000] + "\n" + old_seen_marker)},
                {("message", "m"): message(1, 1, full[:1000] + "\n" + old_seen_marker)}, False),
            "NUL-joined old body against the new two-block complete body": (  # class 2: unambiguous fingerprint
                lambda t: {("message", "m"): message(2, 1, t[:40000], t[40000:])},
                {("message", "m"): message(1, 1, full[:40000] + "\x00" + full[40000:])}, True),
            "OSC hyperlink URI word inside the emitted body's own line is not a refusal marker": (
                lambda t: {("message", "m"): message(1, 1, t[:1000] + "\x1b]8;;https://omitted.example/\x07link\x1b]8;;\x07")},
                None, False),
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


class O12SnapshotFrontier(unittest.TestCase):
    """Real NDJSON reducers, unpatched random challenge, mocked native/public transport only."""
    def run_case(self, mode, *, retained=None, notices=(), early_notices=False):
        import asyncio, hashlib, json
        from types import SimpleNamespace
        from agent_conformance.wire import ItemReducer
        from agent_conformance.results import CheckFailed
        def feed(reducer, *rows):
            reducer.feed(reducer.offset, ''.join(json.dumps(row) + '\n' for row in rows).encode())
            self.assertIsNone(reducer.failure)
        def value(text, *, flag=None, final=True, position=1):
            data = {'role': 'assistant', 'position': position, 'final': final,
                    'content': [{'type': 'text', 'text': text}]}
            if flag is not None: data['incomplete'] = flag
            return data
        def cut(reducer, rows=(), *, epoch='e', seq=0):
            feed(reducer, {'op': 'snapshot', 'phase': 'begin', 'epoch': epoch, 'coverage': 'tail'},
                 *rows, {'op': 'snapshot', 'phase': 'end', 'seq': seq})
        def put(rev, data, *, id='m', seq=None):
            row = {'op': 'put', 'kind': 'message', 'id': id, 'rev': rev, 'value': data}
            if seq is not None: row['seq'] = seq
            return row
        peer = SimpleNamespace(failure=None, closed=False, task=SimpleNamespace(done=lambda: False))
        old = ItemReducer()
        historical = mode in ('same_rev', 'oldbody', 'newer_true')
        cut(old, [put(5, value('historic body', flag=True))] if historical else [])
        views = [SimpleNamespace(initial='reset', reducer=old, peer=peer, stream=1, closed=None, close_notice=None)]
        originals = [old.accept]
        calls, attaches, waits = [], [], []
        clock, late = SimpleNamespace(now=0.0), SimpleNamespace(event=None)
        revision = 6 if historical else 1
        def gap(view, reason='retention'):
            view.closed = reason; view.reducer.failure = reason
            view.close_notice = {'method': 'stream.gap', 'reason': {'reason': 'gap', 'gap': reason},
                                 'last_good_cursor': dict(view.reducer.cursor)}
        class Native:
            def mark(self): return 0
            async def call(self, method, **params):
                calls.append((method, params))
                self.text = '0:' + ''.join(params['chunks'])
                if mode == 'uncertain_native': raise RuntimeError('native outcome unknown')
                feed(old, put(revision, value(self.text[:6000], flag=True if mode == 'newer_true' else None,
                                              final=mode.startswith('late') or mode in ('negative', 'frontier_after_cut', 'spent_budget')), seq=1))
                if mode == 'late_batched': feed(old, put(2, value(self.text[:6000], flag=True), seq=2))
                if mode not in ('late_batched', 'late_split', 'negative', 'frontier_after_cut', 'spent_budget'):
                    gap(views[0], 'slow_consumer' if mode == 'nonretention' else 'retention')
            def records(self, *args, **params):
                return [{'type': 'message_end', 'role': 'assistant', 'text_bytes': len(self.text.encode()),
                         'text_sha256': hashlib.sha256(self.text.encode()).hexdigest()}]
        async def attach(**params):
            attaches.append(params)
            self.assertEqual(params, {'peer': peer})  # No after/seed or native control.
            if mode == 'attach_error': raise RuntimeError('attach rejected')
            new = ItemReducer(); originals.append(new.accept)
            view = SimpleNamespace(initial='continue' if mode == 'continue' else 'reset', reducer=new,
                                   peer=peer, stream=2, closed=None, close_notice=None)
            views.append(view)
            if mode == 'spent_budget': clock.now += .75
            if early_notices: feed(new, *({'op': 'notice', 'value': value} for value in notices))
            return view
        async def wait(view, predicate, **params):
            waits.append(params['timeout'])
            if view is views[0]:
                if view.reducer.failure: raise CheckFailed('item reconstruction gap/error:' + view.reducer.failure)
                self.assertTrue(predicate(view.reducer)); return
            text = ctx.native.text
            if view.reducer.snapshots:
                if late.event is not None: await late.event
                if mode == 'second_after_cut': gap(view); raise CheckFailed('second retention after installed snapshot')
                if mode == 'early_timeout': raise CheckFailed('early observer timeout') from asyncio.TimeoutError()
                if mode == 'wait_failure': raise CheckFailed('observer interrupted')
                if predicate(view.reducer): return
                clock.now = 15.0
                if mode == 'unhealthy_expiry': peer.failure = RuntimeError('peer failed at expiry')
                raise CheckFailed('item stream did not reach expected state') from asyncio.TimeoutError()
            if mode == 'late_split': feed(old, put(2, value(text[:6000], flag=True), seq=2))
            data = value(text[:6000], flag=True)
            rows = [put(6 if historical else 2, data)]
            if mode in ('negative', 'partial', 'notice_retention', 'frontier_after_cut', 'retention_after_cut',
                        'early_timeout', 'wait_failure', 'unhealthy_expiry', 'second_after_cut', 'spent_budget'):
                rows = [put(2, value(text[:6000], final=mode != 'partial'))]
            if mode in ('frontier_after_cut', 'retention_after_cut'): rows = [put(1, value(text[:6000]))]
            if mode == 'same_rev': rows = [put(5, data)]
            if mode == 'oldbody': rows = [put(6, value('historic body', flag=True))]
            if mode == 'full': rows = [put(2, value(text[:35000])), put(1, value(text[35000:], position=2), id='m2')]
            if mode == 'marker':
                rows = [put(2, value(text[:6000])), {'op': 'put', 'kind': 'notice', 'id': 'n', 'rev': 1,
                        'value': {'content': [{'type': 'text', 'text': 'assistant output omitted: too large'}]}}]
            if mode == 'staged':
                feed(view.reducer, {'op': 'snapshot', 'phase': 'begin', 'epoch': 'e', 'coverage': 'tail'}, *rows)
            else: cut(view.reducer, rows, epoch='other' if mode == 'epoch' else 'e',
                      seq=1 if mode in ('frontier_after_cut', 'retention_after_cut') else 2)
            if mode in ('frontier_after_cut', 'retention_after_cut'):
                late.event = asyncio.get_running_loop().create_future()
                def publish_after_cut():
                    feed(view.reducer, put(2, value(text[:6000], flag=True), seq=2))
                    late.event.set_result(True)
                asyncio.get_running_loop().call_soon(publish_after_cut)
            if not early_notices: feed(view.reducer, *({'op': 'notice', 'value': value} for value in notices))
            if mode == 'second_gap': gap(view); raise CheckFailed('second retention')
            if mode == 'spent_budget': clock.now += .5
            if not predicate(view.reducer): raise CheckFailed('snapshot not installed')
        ctx = SimpleNamespace(evidence=retained if retained is not None else [], native=Native(), attach=attach, wait_view=wait)
        recorder = m.AppliedFlagPuts(old)
        from unittest.mock import patch
        try:
            with patch.object(m, 'time', SimpleNamespace(monotonic=lambda: clock.now)):
                asyncio.run(m.o12_adapter_result(ctx, views[0], recorder))
        finally:
            recorder.restore()
            self.assertEqual(len(calls), 1)
            self.assertEqual(calls[0][0], 'retain')
            self.assertEqual(calls[0][1]['count'], 1)
            self.assertEqual(len(calls[0][1]['chunks']), 70)
            self.assertTrue(all(len(chunk.encode()) == 1024 for chunk in calls[0][1]['chunks']))
            self.assertLessEqual(len(attaches), 1)
            self.assertTrue(all(0 <= timeout <= 15 for timeout in waits))
            if len(waits) > 1: self.assertLessEqual(waits[1], 2)  # Snapshot read, not the remaining output budget.
            for view, original in zip(views, originals): self.assertEqual(view.reducer.accept, original)
        return ctx.evidence, attaches

    def test_one_retention_snapshot_disclosure_paths(self):
        for mode in ('flag', 'full', 'marker', 'newer_true'):
            with self.subTest(mode=mode):
                evidence, attaches = self.run_case(mode)
                self.assertEqual(len(attaches), 1)
                self.assertEqual(evidence[0]['observation']['view_closed'], 'retention')
                self.assertTrue(evidence[-1]['snapshot_cut_installed'])
                self.assertFalse(evidence[-1]['observation']['budget_exhausted'])
                self.assertTrue(evidence[-1]['disclosure_observed'])
                if mode in ('flag', 'newer_true'):
                    self.assertTrue(evidence[-1]['incomplete_witnesses'][0]['snapshot_publication'])

    def test_failed_second_gap_epoch_stale_or_unfinished_snapshot_are_named_gaps(self):
        from agent_conformance.results import HarnessGap
        for mode in ('attach_error', 'continue', 'second_gap', 'epoch', 'staged', 'partial', 'oldbody', 'same_rev', 'nonretention'):
            retained = []
            with self.subTest(mode=mode), self.assertRaises(HarnessGap):
                self.run_case(mode, retained=retained)
            self.assertTrue(retained)
            self.assertEqual(retained[0]['phase'], 'initial')
            if mode == 'nonretention': self.assertEqual(len(retained), 1)

    def test_late_separate_flag_is_batch_invariant_and_healthy_budget_expiry_still_fails(self):
        from agent_conformance.results import CheckFailed, HarnessGap
        for mode, attach_count in (('late_batched', 0), ('late_split', 1)):
            evidence, attaches = self.run_case(mode)
            self.assertEqual(len(attaches), attach_count)
            self.assertTrue(evidence[-1]['disclosure_observed'])
        retained = []
        with self.assertRaises(CheckFailed) as raised: self.run_case('negative', retained=retained)
        self.assertNotIsInstance(raised.exception, HarnessGap)
        self.assertEqual(raised.exception.component, 'adapter')
        self.assertTrue(retained[-1]['public_frontier'])
        self.assertTrue(retained[-1]['observation']['budget_exhausted'])
        self.assertEqual(retained[-1]['observation']['budget_deadline'], 15.0)
        self.assertEqual(retained[-1]['observation']['wait_error']['cause']['type'], 'TimeoutError')

    def test_snapshot_cut_is_not_completion_and_next_event_flag_counts(self):
        for mode in ('frontier_after_cut', 'retention_after_cut'):
            with self.subTest(mode=mode):
                evidence, attaches = self.run_case(mode)
                self.assertEqual(len(attaches), 1)
                self.assertTrue(evidence[-1]['disclosure_observed'])
                self.assertFalse(evidence[-1]['observation']['budget_exhausted'])
                self.assertFalse(evidence[-1]['public_frontier'])
                self.assertTrue(evidence[-1]['snapshot_cut_installed'])
                self.assertEqual(evidence[-1]['observation']['sequence'], 2)
                self.assertEqual(evidence[-1]['incomplete_witnesses'][0]['flag_put']['seq'], 2)

    def test_early_timeout_other_failure_and_unhealthy_expiry_are_gaps(self):
        from agent_conformance.results import HarnessGap
        for mode in ('early_timeout', 'wait_failure', 'unhealthy_expiry', 'second_after_cut'):
            with self.subTest(mode=mode), self.assertRaises(HarnessGap): self.run_case(mode)

    def test_snapshot_read_uses_remaining_original_budget_not_a_reset(self):
        from agent_conformance.results import CheckFailed, HarnessGap
        retained = []
        with self.assertRaises(CheckFailed) as raised: self.run_case('spent_budget', retained=retained)
        self.assertNotIsInstance(raised.exception, HarnessGap)
        self.assertTrue(retained[-1]['observation']['budget_exhausted'])
        self.assertEqual(retained[-1]['observation']['budget_deadline'], 15.0)
        self.assertEqual(retained[-1]['observation']['budget_checked_at'], 15.0)
        self.assertEqual(retained[-1]['observation']['wait_timeout_seconds'], 13.75)

    def test_real_context_wait_timeout_proves_expiry_without_an_arbitrary_error_upgrade(self):
        import asyncio, hashlib, json, time
        from types import MethodType, SimpleNamespace
        from agent_conformance.core import PublicImplementation
        from agent_conformance.wire import ItemReducer, Peer
        from agent_conformance.results import CheckFailed, HarnessGap
        async def run():
            reducer = ItemReducer()
            def feed(*rows): reducer.feed(reducer.offset, ''.join(json.dumps(row) + '\n' for row in rows).encode())
            feed({'op': 'snapshot', 'phase': 'begin', 'epoch': 'e', 'coverage': 'tail'},
                 {'op': 'snapshot', 'phase': 'end', 'seq': 0})
            chunks, needle = m.o12_emission_chunks(); text = '0:' + ''.join(chunks)
            recorder = m.AppliedFlagPuts(reducer)
            baseline = m.shown_baseline(reducer, text, needle, applied_flags=recorder)
            feed({'op': 'put', 'seq': 1, 'kind': 'message', 'id': 'm', 'rev': 1,
                  'value': {'role': 'assistant', 'final': True, 'content': [{'type': 'text', 'text': text[:6000]}]}})
            peer = SimpleNamespace(failure=None, closed=False, changed=asyncio.Event(), task=SimpleNamespace(done=lambda: False))
            peer.wait = MethodType(Peer.wait, peer)  # ACTUAL waiter and timeout wrapper, no socket/runtime.
            view = SimpleNamespace(reducer=reducer, peer=peer, closed=None, close_notice=None)
            ctx = SimpleNamespace(evidence=[])
            ctx.wait_view = MethodType(PublicImplementation.wait_view, ctx)
            witness = {'text_bytes': len(text.encode()), 'text_sha256': hashlib.sha256(text.encode()).hexdigest()}
            try:
                record = await m.o12_capture(ctx, view, recorder, baseline, text, needle, witness,
                                             recovered=True, deadline=time.monotonic() + .005)
                self.assertTrue(record['observation']['budget_exhausted'])
                self.assertEqual(record['observation']['wait_error']['cause']['type'], 'TimeoutError')
                with self.assertRaises(CheckFailed) as raised: m.o12_grade(record)
                self.assertNotIsInstance(raised.exception, HarnessGap)
            finally: recorder.restore()
        asyncio.run(run())

    def test_snapshot_notice_window_preserves_warning_before_cache_eviction(self):
        warning = {'severity': 'warn', 'text': 'assistant output omitted: too large'}
        info = {'severity': 'info', 'text': 'progress update'}
        for mode in ('notice_retention', 'negative'):
            for early in (False, True):
                for n, last in ((0, False), (63, False), (64, False), (64, True)):
                    notices = [info] * n + [warning] if last else [warning] + [info] * n
                    with self.subTest(mode=mode, early=early, n=n, last=last):
                        evidence, _ = self.run_case(mode, notices=notices, early_notices=early)
                        self.assertTrue(evidence[-1]['disclosure_observed'])
                        self.assertEqual(evidence[-1]['notice_witnesses'][0]['value'], warning)
                        self.assertFalse(evidence[-1]['notice_evidence_lost'])

    def test_snapshot_notice_unknown_early_prefix_is_gap_but_recorded_window_survives_trace_loss(self):
        from agent_conformance.results import HarnessGap
        warning = {'severity': 'warn', 'text': 'assistant output omitted: too large'}
        info = {'severity': 'info', 'text': 'progress update'}
        for mode in ('notice_retention', 'negative'):
            retained = []
            with self.subTest(mode=mode), self.assertRaisesRegex(HarnessGap, 'notice observation gap'):
                self.run_case(mode, notices=[warning] + [info] * 200, early_notices=True, retained=retained)
            self.assertTrue(retained[-1]['notice_evidence_lost'])
            self.assertEqual(retained[-1]['notice_witnesses'], [])
            evidence, _ = self.run_case(mode, notices=[warning] + [info] * 200)
            self.assertTrue(evidence[-1]['disclosure_observed'])
            self.assertEqual(evidence[-1]['notice_witnesses'][0]['value'], warning)
            self.assertGreater(evidence[-1]['observation']['trace_dropped'], 0)

    def test_uncertain_native_call_is_never_replayed_or_recovered(self):
        with self.assertRaisesRegex(RuntimeError, 'native outcome unknown'):
            self.run_case('uncertain_native')


if __name__ == "__main__":
    unittest.main()
