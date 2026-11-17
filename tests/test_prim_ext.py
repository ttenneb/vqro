"""Snapshot fencing regressions for the stdlib extension helper (no running core)."""
import pathlib
import sys
import threading
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "extensions"))
from prim_ext import Extension


def snapshot(seq, value):
    return {"epoch": "boot", "seq": seq, "entries": {"doc/test/value": {"version": seq, "value": value}}, "resources": []}


def event(seq, value):
    return {"epoch": "boot", "seq": seq, "type": "entries", "data": {"changes": [{"key": "doc/test/value", "version": seq, "value": value}]}}


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


if __name__ == "__main__":
    unittest.main()
