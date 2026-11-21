"""Private fake item viewer: bounded catch-up never writes native input."""
import base64
import copy
import json
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
from coordination_streams import ItemReceiptStreams
from coordination_store import StoreError


def frame(*records):
    return b"".join((json.dumps(record, ensure_ascii=False) + "\n").encode() for record in records)


def snapshot(delivery="d:one", epoch="epoch1", seq=0):
    return frame({"op": "snapshot", "phase": "begin", "epoch": epoch, "coverage": "tail"},
                 {"op": "put", "kind": "receipt", "id": "arbitrary-native-id", "rev": 1,
                  "value": {"delivery": delivery, "stage": "admitted"}},
                 {"op": "snapshot", "phase": "end", "seq": seq})


class StreamGlueTests(unittest.TestCase):
    def setUp(self):
        self.now = 0.0
        self.calls, self.applied = [], []
        self.next_stream = 0
        self.data = snapshot()
        self.fail_apply = False
        self.unsupported = False
        self.resources = {"r1": {"id": "r1", "kind": "agent", "provider": "fake", "instance": "host1", "incarnation": "i1", "available": True,
                                 "data": {"agent": {"v": 0, "caps": {"rev": 1}}}}}
        self.providers = {"fake": {"generation": 1, "capabilities": {"items.stream": {}}}}
        def call(method, params):
            self.calls.append((method, copy.deepcopy(params)))
            if method == "resource.attach":
                if self.unsupported:
                    raise StoreError("unsupported", "not supported")
                self.next_stream += 1
                # Frames arrive before the attach reply. The glue must retain them.
                for data in (self.data if isinstance(self.data, list) else [self.data]):
                    self.viewer.handle("stream.data", {"stream": self.next_stream, "data": base64.b64encode(data).decode()})
                return {"outcome": "accepted", "stream": self.next_stream, "initial": "reset", "epoch": "epoch1"}
            return {}
        def apply(agent, incarnation, events):
            if self.fail_apply:
                raise StoreError("io", "evidence not persisted")
            self.applied.append((agent, incarnation, copy.deepcopy(events)))
        self.viewer = ItemReceiptStreams(call, apply, clock=lambda: self.now)
        self.viewer.sync(self.resources, self.providers)

    def test_early_frames_use_canonical_fence_and_no_client_credit_or_input(self):
        self.viewer.pump()
        self.assertEqual(self.applied, [])
        self.viewer.pump()
        event = self.applied[0][2][0]
        self.assertEqual(event["receipt"]["delivery"], "d:one")
        self.assertEqual(event["receipt"]["stage"], "admitted")
        self.assertEqual(self.viewer.slots["r1"]["cursor"], {"epoch": "epoch1", "seq": 0})
        self.assertTrue(all(method in ("resource.attach", "stream.close") for method, _ in self.calls))
        self.assertNotIn("cols", self.calls[0][1])

    def test_persistence_failure_resets_cursor_and_reads_again_without_input_replay(self):
        self.fail_apply = True
        self.viewer.pump()
        self.viewer.pump()
        slot = self.viewer.slots["r1"]
        self.assertIsNone(slot["cursor"])
        self.assertIsNone(slot["stream"])
        self.assertEqual(self.applied, [])
        self.fail_apply = False
        self.now = 2.0
        self.viewer.pump()
        self.viewer.pump()
        attaches = [params for method, params in self.calls if method == "resource.attach"]
        self.assertEqual(attaches[-1]["items"], {})
        self.assertEqual(len(self.applied), 1)

    def test_a_repeated_rejection_backs_off_instead_of_retrying_every_second(self):
        """A record the collector rejects repeats after every fresh snapshot. Re-installing the same cut is not
        progress, so the backoff grows (1, 2, 4, 8, 16 s ...) instead of restarting at 1 s forever (the O14 storm seen
        beside the agents viewer: one reattach per second)."""
        self.data = [snapshot(seq=3), b"[1]\n"]  # a good cut, then (next frame) a record rejected every time
        attach_times = []
        for step in range(60):  # one minute
            self.now = float(step)
            before = len([c for c in self.calls if c[0] == "resource.attach"])
            self.viewer.pump()
            self.viewer.pump()
            if len([c for c in self.calls if c[0] == "resource.attach"]) > before:
                attach_times.append(self.now)
        self.assertLessEqual(len(attach_times), 8, f"bounded, not once a second: {attach_times}")
        gaps = [b - a for a, b in zip(attach_times, attach_times[1:])]
        self.assertEqual(gaps, sorted(gaps), "the waits only grow while nothing progresses")
        self.assertTrue(self.applied, "the good cut before the rejection still applies")

    def test_progress_past_the_failed_position_resets_the_backoff(self):
        self.data = [snapshot(seq=3), b"[1]\n"]
        for step in range(8):
            self.now = float(step)
            self.viewer.pump()
            self.viewer.pump()
        self.assertGreater(self.viewer.slots["r1"]["failures"], 1)
        self.data = snapshot(seq=9)  # the producer moved past the rejected record
        self.now = 1000.0
        self.viewer.pump()
        self.viewer.pump()
        self.assertEqual(self.viewer.slots["r1"]["failures"], 0, "a cursor past the failed position is progress")

    def test_gap_never_applies_missing_history_or_relabels_known_receipts(self):
        self.viewer.pump()
        self.viewer.pump()
        stream = self.viewer.slots["r1"]["stream"]
        bad = frame({"op": "put", "seq": 2, "kind": "receipt", "id": "r", "rev": 2,
                     "value": {"delivery": "d:one", "stage": "inserted"}})
        self.viewer.handle("stream.data", {"stream": stream, "data": base64.b64encode(bad).decode()})
        self.viewer.pump()
        self.assertEqual(len(self.applied), 1)
        self.assertIsNone(self.viewer.slots["r1"]["cursor"])
        self.assertEqual(self.viewer.status()["issues"]["r1"], "gap")

    def test_sdk_notification_gap_closes_this_viewers_streams_only(self):
        self.viewer.pump()
        self.viewer.pump()
        self.viewer.gap()
        self.viewer.pump()
        self.assertEqual(self.viewer.status()["attached"], 0)
        self.assertTrue(any(method == "stream.close" for method, _ in self.calls))
        self.assertEqual(len(self.applied), 1)

    def test_snapshot_opens_are_windowed_four_at_a_time(self):
        self.resources = {"r" + str(n): dict(self.resources["r1"], id="r" + str(n)) for n in range(1, 21)}
        self.viewer.sync(self.resources, self.providers)
        self.viewer.pump()
        self.assertEqual(sum(method == "resource.attach" for method, _ in self.calls), 4)
        self.viewer.pump()
        self.assertLessEqual(sum(method == "resource.attach" for method, _ in self.calls), 8)

    def test_missing_capability_never_falls_back_to_terminal_attach(self):
        self.providers["fake"]["capabilities"] = {}
        self.viewer.sync(self.resources, self.providers)
        self.viewer.pump()
        self.assertEqual(self.calls, [])

    def test_unsupported_suspend_resets_after_provider_generation_change(self):
        self.unsupported = True
        self.viewer.pump()
        count = len(self.calls)
        self.now = 100
        self.viewer.pump()
        self.assertEqual(len(self.calls), count)
        self.providers["fake"]["generation"] = 2
        self.viewer.sync(self.resources, self.providers)
        self.unsupported = False
        self.viewer.pump()
        self.viewer.pump()
        self.assertEqual(len(self.applied), 1)

    def test_notification_queue_and_endpoint_count_are_bounded(self):
        self.viewer.pump()
        for _ in range(200):
            self.viewer.handle("stream.data", {"stream": 1, "data": base64.b64encode(b"x" * 60000).decode()})
        self.assertLessEqual(self.viewer.status()["queued_bytes"], 1024 * 1024)
        self.assertLessEqual(len(self.viewer.queue), 128)
        self.resources = {"r" + str(n): dict(self.resources["r1"], id="r" + str(n)) for n in range(1, 300)}
        self.viewer.sync(self.resources, self.providers)
        self.assertEqual(len(self.viewer.slots), 256)
        self.assertEqual(self.viewer.excess, 43)


if __name__ == "__main__":
    unittest.main()
