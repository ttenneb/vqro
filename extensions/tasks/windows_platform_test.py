import ctypes
import io
import json
import sqlite3
import tempfile
import time
import unittest
from unittest.mock import patch

import local_platform as platform
import provider
import view
import windows_api as win

SID = "S-1-5-21-123-456-789-1001"
H = {"class": "human_unverified", "terminal_id": "human", "sid": SID}


class AccountTests(unittest.TestCase):
    def test_sid_binding_is_exact_and_distinct_from_uid(self):
        self.assertEqual(H, provider.checked_binding(H))
        self.assertEqual(H, provider.binding(H))
        for bad in ({**H, "uid": 1}, {**H, "sid": "S-1-5"}, {**H, "sid": "S-1-5-4294967296"},
                    {**H, "sid": "S-1-5-01"}, {**H, "sid": "S-1-5-1\n"}, {**H, "extra": True}):
            with self.assertRaises(ValueError):
                provider.checked_binding(bad)
        with patch.object(view, "local_account", return_value=("sid", SID)):
            self.assertTrue(view.human_debtor(H))
            self.assertFalse(view.human_debtor({**H, "sid": "S-1-5-18"}))
            self.assertFalse(view.human_debtor({"class": "human_unverified", "uid": 1001}))
            self.assertFalse(view.human_debtor({**H, "uid": 1001}))
            self.assertTrue(view.human_debtor({"class": "human_unverified"}))

    def test_sid_store_receipts_acceptance_and_message_binding_round_trip(self):
        with tempfile.TemporaryDirectory() as root:
            store = provider.Store(root)
            try:
                payload = {"action": "create", "operation_id": "sid-create", "title": "Windows", "done_when": "Recorded"}
                result = store.invoke({"caller": H, "payload": payload})
                self.assertEqual(H, result["caller"])
                self.assertEqual(result, store.invoke({"caller": H, "payload": payload}))
                self.assertTrue(store.accept_match(H, {**H, "terminal_id": "other"}))
                self.assertFalse(store.accept_match(H, {"class": "human_unverified", "terminal_id": "other", "uid": 1001}))
                self.assertFalse(store.accept_match(H, {**H, "sid": "S-1-5-18"}))
                agent = {"class":"agent", "terminal_id":"agent", "agent":"pi", "execution":"boot:10:20"}
                sent = store.invoke({"caller": H, "payload": {"action":"message_send", "operation_id":"sid-send", "recipient":agent, "body":"hello"}})
                self.assertEqual(H, sent["caller"])
            finally:
                store.db.close()

    def test_sid_decision_ack_round_trip_leaves_unix_keyspace_separate(self):
        with tempfile.TemporaryDirectory() as root:
            store = provider.Store(root)
            try:
                self.assertEqual({"sid": SID, "seen_seq": 0}, store.decisions.acknowledge(H, 0))
                self.assertTrue(store.decisions.digest(H)["last_look_known"])
                self.assertEqual(0, store.db.execute("SELECT COUNT(*) FROM decision_seen").fetchone()[0])
                self.assertEqual(SID, store.db.execute("SELECT sid FROM decision_seen_sid").fetchone()[0])
            finally:
                store.db.close()

    def test_windows_state_path_requires_absolute_local_appdata(self):
        self.assertEqual(r"C:\Users\Me\AppData\Local\vqro\extensions\vqro.tasks",
                         platform.state_directory({"LOCALAPPDATA": r"C:\Users\Me\AppData\Local"}, True))
        for env in ({}, {"LOCALAPPDATA": "relative"}, {"LOCALAPPDATA": r"C:relative"}, {"LOCALAPPDATA": r"\\server\share\data"}):
            with self.assertRaises(ValueError):
                platform.state_directory(env, True)
        self.assertEqual("/state/vqro/extensions/vqro.tasks", platform.state_directory({"XDG_STATE_HOME": "/state"}, False))


class InputTests(unittest.TestCase):
    def lines(self, data):
        lines = platform.InputLines(io.BytesIO(data).read)
        self.addCleanup(lines.close)
        return lines

    def test_frames_eof_and_truncated_frame(self):
        lines = self.lines(b"one\ntwo\n")
        self.assertEqual(b"one", lines.get(1))
        self.assertEqual(b"two", lines.get(1))
        with self.assertRaises(EOFError):
            lines.get(1)
        with self.assertRaises(ValueError):
            self.lines(b"partial").get(1)

    def test_frame_limit_and_single_bounded_queue(self):
        lines = self.lines(b"x" * (platform.MAX_FRAME + 1))
        self.assertEqual(1, lines.frames.maxsize)
        with self.assertRaises(ValueError):
            lines.get(1)
        self.assertEqual(b"x" * platform.MAX_FRAME, self.lines(b"x" * platform.MAX_FRAME + b"\n").get(1))

    def test_timeout_is_not_eof(self):
        from threading import Event
        ready = Event()
        def read(_):
            ready.wait(1)
            return b""
        lines = platform.InputLines(read)
        self.addCleanup(lines.close)
        self.assertIsNone(lines.get(0.001))
        ready.set()
        with self.assertRaises(EOFError):
            lines.get(1)


class FakeKernel:
    def __init__(self):
        self.closed = []
        self.canceled = []
        self.drained = []
        self.created = 0
        self.wait_code = win.WAIT_TIMEOUT

    def create(self, *_):
        self.created += 1
        return 10

    def event(self, *_):
        return 20

    def close(self, handle):
        self.closed.append(handle)

    def read(self, *_):
        return False

    write = read

    def wait(self, *_):
        return self.wait_code

    def cancel(self, handle, operation):
        self.canceled.append((handle, operation._obj.hEvent))
        return False  # Also drain the ERROR_NOT_FOUND completion race.

    def result(self, handle, operation, _, wait):
        self.drained.append((handle, operation._obj.hEvent, wait))
        return False

    def error(self):
        return OSError("fake failure")


class PipeTests(unittest.TestCase):
    def test_pending_timeout_cancels_and_drains_before_event_close(self):
        kernel = FakeKernel()
        with patch.object(ctypes, "get_last_error", return_value=win.ERROR_IO_PENDING, create=True):
            with self.assertRaises(TimeoutError):
                win._transfer(kernel, 10, ctypes.create_string_buffer(10), 10, False, time.monotonic() + 2)
        self.assertEqual([(10, 20)], kernel.canceled)
        self.assertEqual([(10, 20, True)], kernel.drained)
        self.assertEqual([20], kernel.closed)

    def test_deadline_expiration_after_submission_still_drains(self):
        kernel = FakeKernel()
        with patch.object(ctypes, "get_last_error", return_value=win.ERROR_IO_PENDING, create=True):
            with self.assertRaises(TimeoutError):
                win._transfer(kernel, 10, ctypes.create_string_buffer(10), 10, True, 0)
        self.assertTrue(kernel.canceled)
        self.assertEqual([(10, 20, True)], kernel.drained)

    def test_partial_io_and_no_reconnect_after_submission(self):
        kernel = FakeKernel()
        sent = bytearray()
        reply = iter([b'{"ok":', b'true}\n'])
        def transfer(_, handle, buffer, size, writing, deadline):
            if writing:
                count = min(size, 2)
                sent.extend(buffer.raw[:count])
                return count
            block = next(reply)
            ctypes.memmove(buffer, block, len(block))
            return len(block)
        with patch.object(win, "_transfer", side_effect=transfer):
            result = win.request(r"\\.\pipe\vqro-test", b"test\n", kernel=kernel)
        self.assertEqual({"ok": True}, json.loads(result))
        self.assertEqual(b"test\n", sent)
        self.assertEqual([10], kernel.closed)
        kernel = FakeKernel()
        with patch.object(win, "_transfer", side_effect=TimeoutError("unknown")):
            with self.assertRaises(TimeoutError):
                win.request(r"\\.\pipe\vqro-test", b"test\n", kernel=kernel)
        self.assertEqual(1, kernel.created)
        self.assertEqual([10], kernel.closed)

    def test_reply_bound_and_local_endpoint_validation(self):
        kernel = FakeKernel()
        def oversized(_, handle, buffer, size, writing, deadline):
            if writing:
                return size
            ctypes.memset(buffer, 120, size)
            return size
        with patch.object(win, "_transfer", side_effect=oversized):
            with self.assertRaises(OSError):
                win.request(r"\\.\pipe\vqro-test", b"test\n", kernel=kernel)
        self.assertEqual(r"\\.\pipe\C:\Vqro\api.sock", win.pipe_path(r"C:\Vqro\api.sock"))
        self.assertTrue(win.pipe_path(r"\\remote\pipe\x").startswith("\\\\.\\pipe\\"))
        for path in ("", "bad\x00", "x" * 32001):
            with self.assertRaises(ValueError):
                win.request(path, b"test\n", kernel=kernel)


class LockTests(unittest.TestCase):
    def test_competing_writer_has_bounded_precommit_rejection_without_receipt(self):
        with tempfile.TemporaryDirectory() as root:
            store = provider.Store(root)
            lock = sqlite3.connect(root + "/tasks.sqlite", isolation_level=None)
            try:
                self.assertEqual(1000, store.db.execute("PRAGMA busy_timeout").fetchone()[0])
                lock.execute("BEGIN IMMEDIATE")
                start = time.monotonic()
                payload = {"action":"create", "operation_id":"busy-create", "title":"busy", "done_when":"Recorded"}
                with self.assertRaises(sqlite3.OperationalError):
                    store.invoke({"caller":H, "payload":payload})
                self.assertLess(time.monotonic() - start, 3)
                self.assertFalse(store.db.in_transaction)
                self.assertEqual(0, store.db.execute("SELECT COUNT(*) FROM operations").fetchone()[0])
                lock.rollback()
                missing = store.invoke({"caller":H, "payload":{"action":"op", "operation_id":"busy-create"}})
                self.assertFalse(missing["found"])
                result = store.invoke({"caller":H, "payload":payload})
                self.assertEqual(result, store.invoke({"caller":H, "payload":payload}))
            finally:
                lock.close()
                store.db.close()


if __name__ == "__main__":
    unittest.main()
