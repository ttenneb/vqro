"""Extension-only characterization; the real host/Pi demo is separate acceptance."""
import json
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest import mock

import provider

A = {"class": "agent", "terminal_id": "term_a", "agent": "pi", "execution": "boot:10:100"}
B = {"class": "agent", "terminal_id": "term_b", "agent": "pi", "execution": "boot:20:200"}
UNKNOWN = {"class": "unknown"}


class FaultCommit:
    def __init__(self, db, after):
        self.db, self.after = db, after

    def __getattr__(self, name):
        return getattr(self.db, name)

    def execute(self, sql, *args):
        if sql == "COMMIT":
            if self.after:
                self.db.execute(sql)
            raise sqlite3.OperationalError("injected uncertain commit")
        return self.db.execute(sql, *args)


class StoreTests(unittest.TestCase):
    def setUp(self):
        self.root = tempfile.TemporaryDirectory()
        self.store = provider.Store(self.root.name)

    def tearDown(self):
        try:
            self.store.db.close()
        except sqlite3.Error:
            pass
        self.root.cleanup()

    def invoke(self, payload, caller=A):
        return self.store.invoke({"caller": caller, "payload": payload})

    def create(self, op="create-1", caller=A, **extra):
        return self.invoke({"action": "create", "operation_id": op, "title": "work", **extra}, caller)

    def lookup(self, op="create-1", caller=A, **extra):
        return self.invoke({"action": "op", "operation_id": op, **extra}, caller)

    def test_replay_is_once_and_changed_payload_conflicts(self):
        first = self.create()
        self.assertEqual(first, self.create())
        with self.assertRaisesRegex(ValueError, "payload conflict"):
            self.create(title="changed")
        self.assertEqual(1, self.store.db.execute("SELECT COUNT(*) FROM tasks").fetchone()[0])
        self.assertEqual(1, self.store.db.execute("SELECT COUNT(*) FROM transitions").fetchone()[0])
        self.assertEqual(1, self.store.db.execute("SELECT COUNT(*) FROM operations").fetchone()[0])

    def test_same_operation_id_other_caller_is_not_my_receipt(self):
        first = self.create()
        lookup = self.lookup(caller=B)
        self.assertFalse(lookup["found"])
        self.assertFalse(lookup["authoritative"])
        self.assertTrue(lookup["other_caller_exists"])
        second = self.create(caller=B)
        self.assertNotEqual(first["task"]["id"], second["task"]["id"])
        self.assertEqual(first, self.lookup()["outcome"])
        self.assertEqual(second, self.lookup(caller=B)["outcome"])

    def test_unknown_read_requires_explicit_original_operation_scope(self):
        first = self.create()
        with self.assertRaisesRegex(ValueError, "unknown caller"):
            self.lookup(caller=UNKNOWN)
        result = self.lookup(caller=UNKNOWN, lookup_caller=A)
        self.assertTrue(result["authoritative"])
        self.assertEqual(A, result["caller"])
        self.assertEqual(first, result["outcome"])
        absent = self.lookup("absent", caller=UNKNOWN, lookup_caller=A)
        self.assertTrue(absent["authoritative"])
        self.assertFalse(absent["found"])

    def test_expected_caller_is_a_precondition_not_authority(self):
        with self.assertRaisesRegex(ValueError, "caller changed"):
            self.create(expected_caller=B)
        with self.assertRaisesRegex(ValueError, "unknown caller"):
            self.create(caller=UNKNOWN, expected_caller=A)
        self.assertEqual(A, self.create(expected_caller=A)["caller"])

    def test_quota_never_evicts_receipts_and_replay_still_works(self):
        first = self.create()
        with mock.patch.object(provider, "MAX_RECORDS", 1):
            self.assertEqual(first, self.create())
            with self.assertRaisesRegex(ValueError, "quota"):
                self.create("new")
            self.assertEqual(first, self.lookup()["outcome"])

    def test_pages_are_bounded_and_complete_without_duplicates(self):
        for index in range(75):
            self.create(f"page-{index}", title="雪" * 300)
        cursor, ids = None, []
        while True:
            page = self.invoke({"action": "list", **({"after": cursor} if cursor else {})}, UNKNOWN)
            self.assertLessEqual(len(page["tasks"]), provider.PAGE_SIZE)
            self.assertLess(len(provider.canonical(page).encode()), provider.MAX_REPLY_BYTES)
            ids.extend(task["id"] for task in page["tasks"])
            cursor = page["next_cursor"]
            if cursor is None:
                break
        self.assertEqual(75, len(set(ids)))
        self.assertEqual(ids, sorted(set(ids)))

    def test_atomic_failure_has_no_partial_task_transition_or_receipt(self):
        self.store.db.execute("CREATE TRIGGER fail_transition BEFORE INSERT ON transitions BEGIN SELECT RAISE(ABORT, 'injected'); END")
        with self.assertRaises(sqlite3.Error):
            self.create()
        for table in ("tasks", "transitions", "operations"):
            self.assertEqual(0, self.store.db.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0])

    def test_commit_error_is_unknown_and_recovery_distinguishes_commit(self):
        for after in (False, True):
            with self.subTest(after=after), tempfile.TemporaryDirectory() as directory:
                store = provider.Store(directory)
                store.db = FaultCommit(store.db, after)
                with self.assertRaises(provider.OutcomeUnknown):
                    store.invoke({"caller": A, "payload": {"action": "create", "operation_id": "uncertain", "title": "work"}})
                reopened = provider.Store(directory)
                result = reopened.invoke({"caller": A, "payload": {"action": "op", "operation_id": "uncertain"}})
                self.assertEqual(after, result["found"])
                self.assertTrue(result["authoritative"])
                reopened.db.close()

    def test_wal_full_and_reopen_preserve_exact_receipt(self):
        self.assertEqual("wal", self.store.db.execute("PRAGMA journal_mode").fetchone()[0])
        self.assertEqual(2, self.store.db.execute("PRAGMA synchronous").fetchone()[0])
        first = self.create()
        self.store.db.close()
        self.store = provider.Store(self.root.name)
        self.assertEqual(first, self.lookup()["outcome"])

    def test_mutation_spoof_unknown_and_operation_id_boundaries_reject(self):
        with self.assertRaisesRegex(ValueError, "actor flags"):
            self.create(actor="human")
        with self.assertRaisesRegex(ValueError, "unknown caller"):
            self.create(caller=UNKNOWN)
        for value in ("", "a" * 129, "unicode-雪", "contains space", False):
            with self.subTest(value=value), self.assertRaises(ValueError):
                self.create(value)


if __name__ == "__main__":
    unittest.main()
