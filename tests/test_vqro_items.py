"""The Python item-stream viewer (extensions/vqro_items.py) and the SDK's stream routing (no running core)."""
import base64
import json
import pathlib
import random
import sys
import threading
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "extensions"))
from vqro_items import ItemView
from vqro_ext import Vqro

TEXT = ["héllo ", "日本語 ", "🙂🚀 ", "naïve ", "ok "]


def line(record):
    return (json.dumps(record, ensure_ascii=False) + "\n").encode()


def snapshot(epoch="e1", seq=0, entities=(), coverage="all_retained"):
    out = line({"op": "snapshot", "phase": "begin", "epoch": epoch, "coverage": coverage})
    for kind, eid, rev, value in entities:
        out += line({"op": "put", "kind": kind, "id": eid, "rev": rev, "value": value})
    return out + line({"op": "snapshot", "phase": "end", "seq": seq})


def message(text, final=False):
    return {"position": 1, "role": "assistant", "content": [{"type": "text", "text": text}], "final": final,
            "native": {"raw": ""}}


class SnapshotPendingParity(unittest.TestCase):
    def test_pending_covers_split_snapshot_and_clears_on_install_or_end(self):
        view = ItemView()
        self.assertFalse(view.snapshot_pending)
        begin = line({'op': 'snapshot', 'phase': 'begin', 'epoch': 'e1', 'coverage': 'all_retained'})
        view.feed(begin[:-1])
        self.assertFalse(view.snapshot_pending)
        view.feed(begin[-1:])
        self.assertTrue(view.snapshot_pending)
        self.assertIsNone(view.epoch)
        view.feed(line({'op': 'put', 'kind': 'message', 'id': 'm', 'rev': 1, 'value': message('x')}))
        self.assertTrue(view.snapshot_pending)
        self.assertFalse(view.entities)
        view.feed(line({'op': 'snapshot', 'phase': 'end', 'seq': 0}))
        self.assertFalse(view.snapshot_pending)
        self.assertEqual(view.epoch, 'e1')
        view.feed(begin.replace(b'e1', b'e2'))
        self.assertTrue(view.snapshot_pending)
        self.assertEqual(view.epoch, 'e1')
        view.stream_ended()
        self.assertFalse(view.snapshot_pending)
        self.assertEqual(view.epoch, 'e1')


class Producer:
    """A tiny reference producer: the expected entities, and the records that build them."""

    def __init__(self):
        self.seq, self.entities = 0, {}

    def put(self, eid, value):
        rev = self.entities.get(eid, (0, None))[0] + 1
        self.seq += 1
        self.entities[eid] = (rev, json.loads(json.dumps(value)))
        return line({"op": "put", "seq": self.seq, "kind": "message", "id": eid, "rev": rev, "value": value})

    def append(self, eid, text):
        rev, value = self.entities[eid]
        value["content"][0]["text"] += text
        self.seq += 1
        self.entities[eid] = (rev + 1, value)
        return line({"op": "append", "seq": self.seq, "kind": "message", "id": eid, "base": rev, "rev": rev + 1,
                     "field": "/content/0/text", "text": text})

    def append_bytes(self, eid, chunk):
        rev, value = self.entities[eid]
        value["native"]["raw"] = base64.b64encode(base64.b64decode(value["native"]["raw"]) + chunk).decode()
        self.seq += 1
        self.entities[eid] = (rev + 1, value)
        return line({"op": "append", "seq": self.seq, "kind": "message", "id": eid, "base": rev, "rev": rev + 1,
                     "field": "/native/raw", "bytes": base64.b64encode(chunk).decode()})

    def remove(self, eid):
        rev, _ = self.entities.pop(eid)
        self.seq += 1
        return line({"op": "remove", "seq": self.seq, "kind": "message", "id": eid, "rev": rev + 1})


class ItemViewTests(unittest.TestCase):
    def test_any_byte_split_reconstructs_the_producer_exactly(self):
        rng = random.Random(7)
        producer, stream = Producer(), snapshot()
        for _ in range(400):
            eid = f"m{rng.randrange(6)}"
            n = rng.randrange(10)
            if eid in producer.entities and n == 0:
                stream += producer.remove(eid)
            elif eid in producer.entities and n < 6:
                stream += producer.append(eid, TEXT[n % len(TEXT)])
            elif eid in producer.entities and n == 6:
                stream += producer.append_bytes(eid, bytes([n, 0, 255]))
            else:
                stream += producer.put(eid, message(TEXT[n % len(TEXT)]))
        for trial in range(20):
            view, at = ItemView(), 0
            while at < len(stream):
                step = rng.randrange(1, 300)
                view.feed(stream[at:at + step])
                at += step
            self.assertEqual(sum(view.errors.values()), 0, view.errors)
            self.assertEqual({k[1]: v for k, v in view.entities.items()}, producer.entities)
            self.assertEqual(view.after(), {"epoch": "e1", "seq": producer.seq})

    def test_a_snapshot_installs_only_at_its_end(self):
        view = ItemView()
        whole = snapshot("e2", 9, [("message", "m1", 3, message("hi"))])
        view.feed(whole[:-5])
        self.assertIsNone(view.after())
        self.assertEqual(view.entities, {})
        view.feed(whole[-5:])
        self.assertEqual((view.after(), view.coverage, view.snapshots), ({"epoch": "e2", "seq": 9}, "all_retained", 1))

    def test_lost_records_break_the_view_until_the_stream_ends(self):
        view, producer = ItemView(), Producer()
        view.feed(snapshot() + producer.put("m1", message("a")))
        producer.seq += 1  # a record the viewer never sees
        view.feed(producer.append("m1", "b"))
        self.assertTrue(view.broken)
        self.assertEqual(view.errors["missing"], 1)
        self.assertEqual(view.after(), {"epoch": "e1", "seq": 1}, "the cursor stays at the last record applied")
        self.assertEqual(view.entities[("message", "m1")][1]["content"][0]["text"], "a", "the record past the loss is not applied")
        view.feed(producer.append("m1", "c"))
        self.assertEqual(view.entities[("message", "m1")][1]["content"][0]["text"], "a", "nothing applied after loss")
        view.stream_ended()
        self.assertFalse(view.broken)

    def test_stale_base_duplicates_and_unknown_entities(self):
        view, producer = ItemView(), Producer()
        first = producer.put("m1", message("a"))
        view.feed(snapshot() + first + first)
        self.assertEqual((view.errors["duplicate"], view.broken), (1, False), "a duplicate is ignored, not a loss")
        view.feed(line({"op": "append", "seq": 2, "kind": "message", "id": "m1", "base": 9, "rev": 10,
                        "field": "/content/0/text", "text": "x"}))
        self.assertEqual((view.errors["stale_base"], view.broken, view.seq), (1, True, 1), "a stale append doesn't advance")
        other = ItemView()
        other.feed(snapshot() + line({"op": "remove", "seq": 1, "kind": "tool", "id": "t9", "rev": 2}))
        self.assertEqual((other.errors["unknown_entity"], other.broken), (1, True))
        appended = ItemView()
        appended.feed(snapshot() + line({"op": "append", "seq": 1, "kind": "tool", "id": "t9", "base": 1, "rev": 2,
                                         "field": "/x", "text": "y"}))
        self.assertEqual((appended.errors["unknown_entity"], appended.seq), (1, 0))

    def test_a_record_that_cannot_be_applied_whole_is_a_loss_and_never_moves_the_cursor(self):
        """Parity with spike-rev-a's Rust fix: validate before advancing; anything unappliable breaks the view."""
        def fresh():
            view, producer = ItemView(max_entity_bytes=600), Producer()
            view.feed(snapshot() + producer.put("m1", message("a")))
            return view
        cases = {
            "bad base64": {"field": "/native/raw", "bytes": "!!not base64"},
            "no such field": {"field": "/content/9/text", "text": "x"},
            "both payloads": {"field": "/content/0/text", "text": "x", "bytes": "AA=="},
            "text with a null bytes key": {"field": "/content/0/text", "text": "x", "bytes": None},
            "neither payload": {"field": "/content/0/text"},
            "escapes over the bound": {"field": "/content/0/text", "text": "\n" * 300},  # 300 raw bytes, 600 encoded
            "over the bound": {"field": "/content/0/text", "text": "x" * 1000},
        }
        for name, extra in cases.items():
            view = fresh()
            view.feed(line({"op": "append", "seq": 2, "kind": "message", "id": "m1", "base": 1, "rev": 2, **extra}))
            self.assertEqual((view.broken, view.after()["seq"]), (True, 1), name)
            self.assertEqual(view.entities[("message", "m1")][1]["content"][0]["text"], "a", name)
        view = fresh()
        view.feed(line({"op": "put", "seq": 2, "kind": "message", "id": "m2", "rev": 1, "value": message("y" * 1000)}))
        self.assertEqual((view.broken, view.seq, ("message", "m2") in view.entities), (True, 1, False), "an oversized put")
        view = fresh()
        view.feed(line({"op": "frobnicate", "seq": 2, "kind": "message", "id": "m1", "rev": 2}))
        self.assertEqual((view.broken, view.seq), (True, 1), "an unknown op")

    def test_bounds_and_bad_lines(self):
        view = ItemView(max_record_bytes=64)
        view.feed(b"x" * 70)
        self.assertEqual((view.errors["oversize"], view.broken), (1, True))
        small = ItemView(max_entities=1)
        small.feed(snapshot(entities=[("message", "a", 1, message("1")), ("message", "b", 1, message("2"))]))
        self.assertIsNone(small.after(), "an overflowing snapshot is never installed")
        self.assertEqual(small.errors["over_bound"], 1)
        bad = ItemView()
        bad.feed(b"\xff\n{nope\n[1]\n" + line({"op": "put", "kind": "message", "id": "m"}))
        self.assertEqual((bad.errors["bad_utf8"], bad.errors["bad_json"], bad.errors["bad_record"]), (1, 1, 1))
        self.assertTrue(bad.broken, "a malformed record is a loss: nothing after it applies until the stream ends")
        envelope = ItemView()
        envelope.feed(snapshot() + line({"op": "put", "seq": 1, "kind": "message", "id": 7, "rev": 1, "value": {}}))
        self.assertEqual((envelope.broken, envelope.seq, envelope.errors["bad_record"]), (True, 0, 1), "a malformed live envelope")

    def test_notices_are_returned_and_never_stored(self):
        view = ItemView()
        notices = view.feed(snapshot() + line({"op": "notice", "value": {"text": "compacting"}}))
        self.assertEqual((notices, view.entities), ([{"text": "compacting"}], {}))


class SharedVectors(unittest.TestCase):
    """tests/fixtures/items-vectors.jsonl: the same cases the Rust ItemView runs (P3-5, prim-opus-review)."""

    def test_every_vector(self):
        path = pathlib.Path(__file__).resolve().parent / "fixtures" / "items-vectors.jsonl"
        cases = [json.loads(l) for l in path.read_text().splitlines() if l.strip()]
        cases = [c for c in cases if "name" in c]
        self.assertGreaterEqual(len(cases), 20)
        for case in cases:
            with self.subTest(case["name"]):
                view = ItemView()
                view.feed(b"".join(line(r) for r in case["snapshot"]))
                view.feed(b"".join(line(r) for r in case["records"]))
                got = {"seq": view.seq, "records": view.records, "broken": view.broken,
                       "errors": {k: v for k, v in view.errors.items() if v}}
                self.assertEqual(got, case["expect"])


class Sink:
    def __init__(self):
        self.got = []

    def data(self, data):
        self.got.append(("data", data))

    def error(self, error):
        self.got.append(("error", error))

    def ended(self, kind, reason):
        self.got.append((kind, reason))


class StreamRouting(unittest.TestCase):
    def vqro(self):
        v = Vqro.__new__(Vqro)
        v.lock, v.waiting, v.streams = threading.Lock(), {}, {}
        v.early, v.early_bytes, v.evicted, v.next, v.sent = {}, 0, set(), 100, []
        v._send = v.sent.append
        v.closed, v.closed_lock = __import__("collections").deque(maxlen=256), threading.Lock()
        return v

    def note(self, method, **params):
        return {"jsonrpc": "2.0", "method": method, "params": params}

    def test_the_attach_hook_registers_the_sink_before_later_lines_and_replays_early_data(self):
        v, sink = self.vqro(), Sink()
        ev = threading.Event()
        v.waiting[5] = [ev, None, lambda r: v.register_stream(r["stream"], sink)]
        v._line(self.note("stream.data", stream=9, data=base64.b64encode(b"early").decode()), 50)
        v._line({"jsonrpc": "2.0", "id": 5, "result": {"stream": 9, "initial": "reset"}}, 50)
        v._line(self.note("stream.data", stream=9, data=base64.b64encode(b"late").decode()), 50)
        v._line(self.note("stream.gap", stream=9, reason={"reason": "retention"}), 50)
        v._line(self.note("stream.data", stream=9, data="eA=="), 50)
        self.assertTrue(ev.is_set())
        self.assertEqual(sink.got, [("data", b"early"), ("data", b"late"), ("gap", {"reason": "retention"})])
        self.assertNotIn(9, v.streams, "ended streams stop routing")

    def test_early_buffer_is_bounded_and_closed_streams_are_dropped(self):
        v = self.vqro()
        for n in range(100):
            v._line(self.note("stream.data", stream=n, data="eA=="), 50)
        self.assertLessEqual(len(v.early), 64)
        v.closed.append(500)
        v._line(self.note("stream.data", stream=500, data="eA=="), 50)
        self.assertNotIn(500, v.early)
        v._line(self.note("stream.data", stream=7, data="A" * (300 * 1024)), 50)
        self.assertNotIn(7, v.early, "one oversized note is never buffered")
        self.assertLessEqual(v.early_bytes, 256 * 1024)

    def test_a_stream_that_lost_early_notes_registers_with_a_gap(self):
        """Review P3-2: a dropped stream.closed or snapshot begin must not leave a stuck view."""
        v, sink = self.vqro(), Sink()
        v._line(self.note("stream.data", stream=1, data="eA=="), 50)
        for n in range(2, 70):  # push stream 1 out of the early buffer
            v._line(self.note("stream.data", stream=n, data="eA=="), 50)
        self.assertNotIn(1, v.early)
        v._line(self.note("stream.closed", stream=1, reason={"reason": "x"}), 50)  # not kept: already lost notes
        v.register_stream(1, sink)
        self.assertEqual(sink.got, [("gap", {"reason": "early_overflow"})])
        self.assertNotIn(1, v.streams)

    def test_a_late_attach_reply_is_closed_not_leaked(self):
        """Review P3-3: the caller timed out, the core attached anyway: close the stream."""
        v = self.vqro()
        v._line({"jsonrpc": "2.0", "id": 77, "result": {"stream": 9, "initial": "reset", "epoch": "e"}}, 50)
        self.assertEqual(v.sent, [{"jsonrpc": "2.0", "id": 100, "method": "stream.close", "params": {"stream": 9}}])
        self.assertIn(9, v.closed)
        v._line({"jsonrpc": "2.0", "id": 78, "result": {"ok": True}}, 50)
        self.assertEqual(len(v.sent), 1, "other orphan replies are ignored")


if __name__ == "__main__":
    unittest.main()
