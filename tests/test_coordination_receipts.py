"""Canonical receipt stream reducer; in-memory only, no core/runtime/network."""
import json
import random
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
from coordination_receipts import ReceiptCollector, ReceiptError


def wire(*records):
    return b"".join(json.dumps(r, ensure_ascii=False, separators=(",", ":")).encode() + b"\n" for r in records)


def put(seq=1, identifier="q1", revision=1, delivery="d:1", stage="admitted", **value):
    r = {"op": "put", "kind": "receipt", "id": identifier, "rev": revision,
         "value": {"delivery": delivery, "stage": stage, **value}}
    if seq is not None:
        r["seq"] = seq
    return r


def begin(epoch="e1", coverage="all_retained", **extra):
    return {"op": "snapshot", "phase": "begin", "epoch": epoch, "coverage": coverage, **extra}


def end(seq=10, **extra):
    return {"op": "snapshot", "phase": "end", "seq": seq, **extra}


def live(**kwargs):
    return ReceiptCollector("e1", after={"epoch": "e1", "seq": 0}, **kwargs)


class Receipts(unittest.TestCase):
    def rejected(self, kind, collector, data):
        with self.assertRaises(ReceiptError) as caught:
            collector.feed(data)
        self.assertEqual(caught.exception.kind, kind)

    def test_split_every_utf8_byte_and_jsonl_record(self):
        c = live()
        data = wire(put(cause={"kind": "delivery", "id": "d:🐈", "provenance": "native"}))
        output = []
        for byte in data:
            output.extend(c.feed(bytes([byte])))
        self.assertEqual(len(output), 1)
        self.assertEqual(output[0]["receipt"]["cause"]["id"], "d:🐈")
        self.assertEqual(c.cursor, {"epoch": "e1", "seq": 1})

    def test_projection_does_not_leak_native_or_infer_from_messages(self):
        c = live()
        out = c.feed(wire({"op": "put", "seq": 1, "kind": "message", "id": "m", "rev": 1,
                           "value": {"delivery": "fake", "stage": "inserted"}},
                          put(2, native={"huge": "not projected"}, run="r", cause={"provenance": "asserted"})))
        self.assertEqual(out, [{"epoch": "e1", "seq": 2, "receipt": {
            "delivery": "d:1", "stage": "admitted", "run": "r", "cause": {"provenance": "asserted"}}}])
        out[0]["receipt"]["stage"] = "corrupted"
        self.assertEqual(c.feed(wire({"op": "append", "seq": 3, "kind": "receipt", "id": "q1", "base": 1,
                                     "rev": 2, "field": "/stage", "text": "_later"}))[0]["receipt"]["stage"], "admitted_later")

    def test_every_other_kind_counts_seq_but_retains_no_body(self):
        c = live(entity_limit=1, byte_limit=1000)
        for i in range(1, 101):
            self.assertEqual(c.feed(wire({"op": "put", "seq": i, "kind": "message", "id": str(i), "rev": 1,
                                         "value": {"text": "x" * 1000}})), [])
        self.assertEqual(c.cursor["seq"], 100)
        self.assertEqual(len(c._state.entities), 0)
        self.assertEqual(len(c.feed(wire(put(101)))), 1)

    def test_duplicate_records_and_notice_are_not_receipts(self):
        c = live()
        self.assertEqual(len(c.feed(wire(put()))), 1)
        self.assertEqual(c.feed(wire(put(), {"op": "notice", "value": {"text": "hi"}})), [])
        self.assertEqual(c.cursor["seq"], 1)

    def test_sequence_gap_and_batch_failure_do_not_skip_unreturned_receipt(self):
        c = live()
        self.rejected("gap", c, wire(put(), put(3, identifier="q2")))
        self.assertEqual(c.cursor["seq"], 0, "first receipt from failed feed was never returned")
        self.assertEqual(len(c._state.entities), 0)
        self.rejected("gap", c, wire(put()))
        self.assertEqual(len(c.feed(wire(begin(), put(None), end(1)))), 1)

    def test_checked_append_and_json_pointer_escaping(self):
        c = live()
        c.feed(wire(put(stage="insert", native={"a/b": {"~key": ["log"]}})))
        r = {"op": "append", "seq": 2, "kind": "receipt", "id": "q1", "base": 1,
             "rev": 2, "field": "/native/a~1b/~0key/0", "text": "!"}
        self.assertEqual(c.feed(wire(r))[0]["receipt"]["stage"], "insert")
        r.update(seq=3, base=2, rev=3, field="/stage", text="ed")
        self.assertEqual(c.feed(wire(r))[0]["receipt"]["stage"], "inserted")

    def test_append_missing_wrong_or_nonincreasing_base_is_gap(self):
        for base, rev in ((2, 3), (1, 1)):
            c = live()
            c.feed(wire(put()))
            self.rejected("gap", c, wire({"op": "append", "seq": 2, "kind": "receipt", "id": "q1",
                                         "base": base, "rev": rev, "field": "/stage", "text": "x"}))
            self.assertEqual(c.cursor["seq"], 1)
        c = ReceiptCollector("e1", after={"epoch": "e1", "seq": 10})
        self.rejected("gap", c, wire({"op": "append", "seq": 11, "kind": "receipt", "id": "unknown",
                                     "base": 1, "rev": 2, "field": "/stage", "text": "x"}))

    def test_append_invalid_target_and_bytes_are_never_guessed(self):
        for field, expected in (("/missing", "gap"), ("/native/x", "gap"), ("/native/~2", "invalid")):
            c = live()
            c.feed(wire(put(native={"x": 1})))
            self.rejected(expected, c, wire({"op": "append", "seq": 2, "kind": "receipt", "id": "q1",
                                            "base": 1, "rev": 2, "field": field, "text": "x"}))
        c = live()
        c.feed(wire(put()))
        self.rejected("invalid", c, wire({"op": "append", "seq": 2, "kind": "receipt", "id": "q1",
                                         "base": 1, "rev": 2, "field": "/stage", "bytes": "AA=="}))

    def test_put_revisions_and_remove_are_checked(self):
        c = live()
        c.feed(wire(put()))
        self.assertEqual(c.feed(wire({"op": "remove", "seq": 2, "kind": "receipt", "id": "q1", "rev": 2})), [])
        self.assertEqual(len(c._state.entities), 0)
        c = live()
        c.feed(wire(put()))
        self.rejected("gap", c, wire(put(2)))

    def test_snapshot_is_buffered_until_end_and_emits_cut_cursor(self):
        c = ReceiptCollector("e1")
        self.assertIsNone(c.cursor)
        self.assertEqual(c.feed(wire(begin(), put(None))), [])
        self.assertIsNone(c.cursor)
        out = c.feed(wire(end(25)))
        self.assertEqual(out[0]["seq"], 25)
        self.assertEqual(c.cursor, {"epoch": "e1", "seq": 25})
        self.assertEqual(c.feed(wire(put(26, revision=2, stage="inserted")))[0]["receipt"]["stage"], "inserted")

    def test_reset_session_epoch_discards_old_entity_bases(self):
        c = live()
        c.feed(wire(put()))
        self.assertEqual(c.feed(wire(begin("e2", session="s2"), put(None, stage="queued"))), [])
        self.assertEqual(c.cursor, {"epoch": "e1", "seq": 1})
        out = c.feed(wire(end(0, session="s2")))
        self.assertEqual(out[0]["epoch"], "e2")
        self.assertEqual(c.cursor, {"epoch": "e2", "seq": 0})
        self.assertEqual(c.feed(wire(put(1, revision=2, stage="inserted")))[0]["receipt"]["stage"], "inserted")

    def test_partial_cut_discard_never_installs_or_emits_it(self):
        c = live()
        c.feed(wire(put()))
        c.feed(wire(begin("e2"), put(None, delivery="d:lost")))
        c.discard_partial()
        self.assertEqual(c.cursor, {"epoch": "e1", "seq": 1})
        self.rejected("gap", c, wire(end(10)))
        out = c.feed(wire(begin("e3"), put(None, delivery="d:new"), end(0)))
        self.assertEqual([r["receipt"]["delivery"] for r in out], ["d:new"])

    def test_partial_line_discard_resumes_after_last_applied_record(self):
        c = live()
        data = wire(put())
        c.feed(data[:-2])
        c.discard_partial()
        self.assertEqual(c.cursor["seq"], 0)
        self.assertEqual(len(c.feed(data)), 1)

    def test_snapshot_count_and_byte_overflow_drop_whole_cut(self):
        c = live(entity_limit=1)
        c.feed(wire(put()))
        self.rejected("overflow", c, wire(begin("e2"), put(None), put(None, identifier="q2"), end(2)))
        self.assertEqual(c.cursor, {"epoch": "e1", "seq": 1})
        self.assertEqual(len(c._state.entities), 1)
        c = ReceiptCollector("e1", byte_limit=150)
        self.rejected("overflow", c, wire(begin(), put(None, native={"text": "x" * 200}), end()))
        self.assertIsNone(c.cursor)
        self.assertIsNone(c._cut)

    def test_partial_cut_accumulation_is_bounded_across_feeds(self):
        c = ReceiptCollector("e1", entity_limit=2)
        c.feed(wire(begin(), put(None), put(None, identifier="q2")))
        self.rejected("overflow", c, wire(put(None, identifier="q3")))
        self.assertIsNone(c._cut)
        self.assertIsNone(c.cursor)

    def test_live_memory_is_bounded_and_oversize_feed_is_atomic(self):
        c = live(entity_limit=1)
        c.feed(wire(put()))
        self.rejected("overflow", c, wire(put(2, identifier="q2")))
        self.assertEqual(c.cursor["seq"], 1)
        self.assertEqual(len(c._state.entities), 1)
        c = live(byte_limit=250)
        self.rejected("overflow", c, wire(*(put(i, revision=i) for i in range(1, 5))))
        self.assertEqual(c.cursor["seq"], 0)

    def test_tail_and_state_only_never_synthesize_missing_receipts(self):
        for coverage in ("tail", "state_only", "_future"):
            c = ReceiptCollector("e1")
            self.assertEqual(c.feed(wire(begin(coverage=coverage), end(10))), [])
            self.assertEqual(c.cursor["seq"], 10)

    def test_malformed_json_utf8_duplicate_keys_and_nonfinite_fail_closed(self):
        for data in (b"{no}\n", b"\xff\n", b'[]\n', b'{"op":"notice","op":"put","value":{}}\n',
                     b'{"op":"notice","value":NaN}\n'):
            c = live()
            self.rejected("invalid", c, data)
            self.assertEqual(c.cursor["seq"], 0)

    def test_line_limit_and_truncated_json_are_bounded(self):
        c = live(line_limit=32)
        self.rejected("overflow", c, b"x" * 33)
        self.assertEqual(c._line, b"")
        c = live()
        self.assertEqual(c.feed(b'{"op":'), [])
        self.assertEqual(c.cursor["seq"], 0)
        self.assertLessEqual(len(c._line), c.line_limit)

    def test_only_canonical_shapes_and_snapshot_fences_are_accepted(self):
        for record in ({"op": "reset", "epoch": "e1"}, {"op": "snapshot_end", "seq": 1},
                       {"op": "remove", "seq": 1, "kind": "receipt", "id": "q1"},
                       dict(put(), extra="not canonical")):
            self.rejected("invalid", live(), wire(record))
        self.rejected("gap", live(), wire(end()))
        self.rejected("gap", ReceiptCollector("e1"), wire(put()))
        c = ReceiptCollector("e1")
        self.rejected("invalid", c, wire(begin(), put(1)))
        c = live()
        self.rejected("gap", c, wire(begin(session="s1"), end(session="s2")))

    def test_failed_feed_rolls_back_a_completed_cut_in_that_same_feed(self):
        c = live()
        c.feed(wire(put()))
        self.rejected("gap", c, wire(begin("e2"), put(None, delivery="d:new"), end(3), put(5)))
        self.assertEqual(c.cursor, {"epoch": "e1", "seq": 1})
        self.assertEqual(c._state.entities["q1"]["value"]["delivery"], "d:1")

    def test_unicode_line_separators_are_content_not_record_boundaries(self):
        c = live()
        receipt = put(cause={"id": "a\u2028b\u2029c"})
        self.assertEqual(c.feed(wire(receipt))[0]["receipt"]["cause"]["id"], "a\u2028b\u2029c")

    def test_deep_receipt_json_fails_with_receipt_error_not_python_recursion(self):
        c = live()
        raw = b'{"op":"put","seq":1,"kind":"receipt","id":"q","rev":1,"value":{"delivery":"d","stage":"inserted","cause":' + b'[' * 1100 + b'0' + b']' * 1100 + b'}}\n'
        self.rejected("invalid", c, raw)
        self.assertEqual(c.cursor["seq"], 0)

    def test_random_transport_chunking_matches_one_frame(self):
        records = [begin("e2"), put(None, stage="insert"), end(500),
                   {"op": "put", "seq": 501, "kind": "message", "id": "q1", "rev": 1,
                    "value": {"delivery": "wrong", "stage": "read", "text": "猫\u2028"}},
                   {"op": "append", "seq": 502, "kind": "receipt", "id": "q1", "base": 1,
                    "rev": 2, "field": "/stage", "text": "ed"},
                   {"op": "remove", "seq": 503, "kind": "receipt", "id": "q1", "rev": 3}]
        data = wire(*records)
        expected = ReceiptCollector("e1").feed(data)
        for seed in range(32):
            rng, c, output, offset = random.Random(seed), ReceiptCollector("e1"), [], 0
            while offset < len(data):
                length = rng.randrange(1, 65)
                output.extend(c.feed(data[offset:offset + length]))
                offset += length
            self.assertEqual(output, expected)
            self.assertEqual(c.cursor, {"epoch": "e2", "seq": 503})
            self.assertEqual(c._state.entities, {})

    def test_snapshot_cut_cannot_rollback_same_epoch_cursor(self):
        c = ReceiptCollector("e1", after={"epoch": "e1", "seq": 50})
        self.rejected("gap", c, wire(begin(), put(None), end(49)))
        self.assertEqual(c.cursor, {"epoch": "e1", "seq": 50})

    def test_repeated_remove_put_keeps_receipt_storage_bounded(self):
        c = live(entity_limit=1, byte_limit=1000)
        for i in range(100):
            c.feed(wire(put(2 * i + 1, identifier=str(i)),
                        {"op": "remove", "seq": 2 * i + 2, "kind": "receipt", "id": str(i), "rev": 2}))
            self.assertEqual(c._state.entities, {})
            self.assertEqual(c._state.size, 0)

    def test_constructor_and_cursor_are_defensive(self):
        for kwargs in ({"after": {"epoch": "other", "seq": 1}}, {"entity_limit": True},
                       {"byte_limit": 0}, {"line_limit": 65537}, {"after": {"epoch": "e1", "seq": -1}}):
            with self.assertRaises(ReceiptError):
                ReceiptCollector("e1", **kwargs)
        c = live()
        cursor = c.cursor
        cursor["seq"] = 999
        self.assertEqual(c.cursor["seq"], 0)
        self.assertEqual(c.feed(memoryview(wire(put())))[0]["receipt"]["delivery"], "d:1")


if __name__ == "__main__":
    unittest.main()
