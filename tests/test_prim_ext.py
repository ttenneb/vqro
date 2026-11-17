"""Snapshot fencing regressions for the stdlib extension helper (no running core)."""
import pathlib
import queue
import sys
import threading
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "extensions"))
from prim_ext import Extension, Prim, NOTIFICATION_BYTES


def snapshot(seq, value):
    return {"epoch": "boot", "seq": seq, "entries": {"doc/test/value": {"version": seq, "value": value}}, "resources": []}


def event(seq, value):
    return {"epoch": "boot", "seq": seq, "type": "entries", "data": {"changes": [{"key": "doc/test/value", "version": seq, "value": value}]}}


class InboundBounds(unittest.TestCase):
    def test_notification_bytes_stay_charged_until_callback_finishes(self):
        prim = Prim.__new__(Prim)
        prim.notes = queue.Queue(maxsize=128)
        prim.note_lock = threading.Lock()
        prim.note_bytes = 0
        prim.handler = lambda method, params: None
        msg = {"method":"event", "params":{}}
        accepted = 0
        while prim._queue_note(msg, 65536):
            accepted += 1
        self.assertEqual(accepted, NOTIFICATION_BYTES // 65536)
        self.assertLessEqual(prim.note_bytes, NOTIFICATION_BYTES)
        prim._note(*prim.notes.get_nowait())
        self.assertTrue(prim._queue_note(msg, 65536))
        while not prim.notes.empty():
            prim._note(*prim.notes.get_nowait())
        self.assertEqual(prim.note_bytes, 0)

    def test_tiny_notifications_have_a_metadata_and_count_bound(self):
        prim = Prim.__new__(Prim)
        prim.notes = queue.Queue(maxsize=128)
        prim.note_lock = threading.Lock()
        prim.note_bytes = 0
        count = 0
        while prim._queue_note({}, 1):
            count += 1
        self.assertEqual(count, 128)
        self.assertEqual(prim.note_bytes, 128*64)


class SnapshotFence(unittest.TestCase):
    def extension(self):
        ext = Extension.__new__(Extension)
        ext.cache_lock = threading.RLock()
        ext.listeners = []
        ext._load(snapshot(1, "old"))
        return ext

    def test_queued_pre_snapshot_event_cannot_regress_cache(self):
        ext = self.extension()

        class Prim:
            def call(self, method):
                return snapshot(3, "current")

        ext.prim = Prim()
        ext._handle("missed", {})
        ext._handle("event", event(2, "stale"))
        self.assertEqual(ext.value("doc/test/value"), "current")
        ext._handle("event", event(4, "new"))
        self.assertEqual(ext.value("doc/test/value"), "new")
        self.assertEqual(ext.seq, 4)

    def test_listeners_do_not_hold_cache_lock_across_extension_code(self):
        ext = self.extension()

        class Prim:
            def call(self, method):
                return snapshot(3, "fresh")

        ext.prim = Prim()

        def listener(kind, data):
            thread = threading.Thread(target=ext.fresh)
            thread.start()
            thread.join(1)
            self.assertFalse(thread.is_alive(), "a listener must not block command-thread refreshes")

        ext.listeners.append(listener)
        ext._handle("event", event(2, "event"))
        self.assertEqual(ext.seq, 3)

    def test_command_thread_snapshot_is_serialized_with_notifications(self):
        ext = self.extension()
        requested = threading.Event()
        release = threading.Event()
        attempted = threading.Event()

        class Prim:
            def call(self, method):
                requested.set()
                if not release.wait(2):
                    raise AssertionError("snapshot was not released")
                return snapshot(2, "snapshot")

        ext.prim = Prim()
        fresh = threading.Thread(target=ext.fresh)
        fresh.start()
        self.assertTrue(requested.wait(2))

        def notify():
            attempted.set()
            ext._handle("event", event(3, "new"))

        notification = threading.Thread(target=notify)
        notification.start()
        self.assertTrue(attempted.wait(2))
        release.set()
        fresh.join(2)
        notification.join(2)
        self.assertFalse(fresh.is_alive())
        self.assertFalse(notification.is_alive())
        self.assertEqual(ext.value("doc/test/value"), "new")
        self.assertEqual(ext.seq, 3)


class PartialResources(unittest.TestCase):
    def test_partial_resource_events_merge_onto_the_cached_record(self):
        ext = SnapshotFence.extension(self)
        ext._handle("event", {"epoch": "boot", "seq": 2, "type": "resource", "data": {"id": "r1", "argv": ["pi"], "status": {"state": "idle"}}})
        ext._handle("event", {"epoch": "boot", "seq": 3, "type": "resource", "partial": True, "data": {"id": "r1", "status": {"state": "working"}}})
        self.assertEqual(ext.resources["r1"], {"id": "r1", "argv": ["pi"], "status": {"state": "working"}})

    def test_partial_event_without_a_base_reloads_the_snapshot(self):
        ext = SnapshotFence.extension(self)

        class Prim:
            calls = 0

            def call(self, method):
                Prim.calls += 1
                return dict(snapshot(5, "x"), resources=[{"id": "r9", "argv": ["sh"], "status": None}])

        ext.prim = Prim()
        ext._handle("event", {"epoch": "boot", "seq": 2, "type": "resource", "partial": True, "data": {"id": "r9", "status": "busy"}})
        self.assertEqual(Prim.calls, 1)
        self.assertEqual(ext.resources["r9"]["argv"], ["sh"])


if __name__ == "__main__":
    unittest.main()
