"""Extension-only characterization; the real host/Pi demo is separate acceptance."""
import json
from pathlib import Path
import sqlite3
import tempfile
import unittest
import os
import subprocess
import sys
import threading
from concurrent.futures import ThreadPoolExecutor
from unittest import mock

import provider

A = {"class": "agent", "terminal_id": "term_a", "agent": "pi", "execution": "boot:10:100"}
B = {"class": "agent", "terminal_id": "term_b", "agent": "pi", "execution": "boot:20:200"}
UNKNOWN = {"class": "unknown"}
HUMAN = {"class": "human_unverified", "terminal_id": "term_human", "uid": 1000}


class FaultCommit:
    def __init__(self, db, after, close_error=False, rollback_error=False):
        self.db, self.after = db, after
        self.close_error, self.rollback_error = close_error, rollback_error

    def __getattr__(self, name):
        return getattr(self.db, name)

    def close(self):
        self.db.close()
        if self.close_error:
            raise sqlite3.OperationalError("injected cleanup failure")

    def execute(self, sql, *args):
        if sql == "ROLLBACK" and self.rollback_error:
            raise sqlite3.OperationalError("injected rollback failure")
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

    def invoke(self, payload, caller=HUMAN):
        if payload["action"] in ("create", "decision"):
            payload = {"done_when": "Result is recorded.", **payload}
        return self.store.invoke({"caller": caller, "payload": payload})

    def create(self, op="create-1", caller=HUMAN, **extra):
        return self.invoke({"action": "create", "operation_id": op, "title": "work", **extra}, caller)

    def lookup(self, op="create-1", caller=HUMAN, **extra):
        return self.invoke({"action": "op", "operation_id": op, **extra}, caller)

    def change(self, task, action, caller=HUMAN, op=None, **extra):
        if action == "report" and "text" in extra:
            extra["text"] = "> " + task["done_when"] + "\n" + extra["text"]
        return self.invoke({"action": action, "id": task["id"], "revision": task["revision"],
                            "operation_id": op or f"{action}-{task['revision']}-{task['id']}", **extra}, caller)["task"]

    def test_delegated_child_report_human_acceptance_and_debts(self):
        root = self.create()["task"]
        child = self.create("child", parent=root["id"])["task"]
        with self.assertRaisesRegex(ValueError, "unfinished children"):
            self.change(root, "report", text="parent report")
        child = self.change(child, "delegate", owner=B)
        self.assertEqual(B, child["owner"])
        self.assertEqual(HUMAN, child["creator"])
        self.assertEqual(HUMAN, child["delegator"])
        self.assertEqual(B, child["owed_to"])
        child = self.change(child, "start", caller=B)
        self.assertEqual("work/report", child["owed_action"])
        child = self.change(child, "block", caller=B, text="needs input")
        self.assertEqual("unblock", child["owed_action"])
        self.assertEqual(child["updated_at"], child["owed_since"])
        with self.assertRaisesRegex(ValueError, "not owned"):
            self.change(child, "report", caller=A, text="forged other-agent report")
        child = self.change(child, "report", caller=B, text="proof complete")
        self.assertEqual({"role": "acceptors", "bindings": [HUMAN]}, child["owed_to"])
        self.assertEqual("accept", child["owed_action"])
        for caller in (A, B):
            with self.assertRaisesRegex(ValueError, "own work|acceptor"):
                self.change(child, "accept", caller=caller)
        child = self.change(child, "accept", caller=HUMAN)
        self.assertEqual("accepted", child["state"])
        self.assertEqual(HUMAN, child["last_transition"]["caller"])
        self.assertIsNone(child["owed_to"])
        self.assertEqual("> Result is recorded.\nproof complete", child["detail"])
        child = self.change(child, "done", caller=B)
        self.assertEqual("done", child["state"])
        root = self.change(root, "report", text="child outcome accepted")
        self.assertEqual("accept", root["owed_action"])
        root = self.change(root, "accept", caller=HUMAN)
        self.assertIsNone(root["owed_since"])

    def test_decision_is_a_task_kind_with_one_human_answer_path(self):
        decision = self.invoke({"action": "decision", "operation_id": "decision-1", "title": "Which direction?"})["task"]
        self.assertEqual("decision", decision["kind"])
        self.assertEqual(HUMAN, decision["creator"])
        self.assertEqual(provider.HUMAN_ROLE, decision["owner"])
        self.assertEqual("answer", decision["owed_action"])
        for caller in (A, B):
            with self.assertRaisesRegex(ValueError, "human-only"):
                self.change(decision, "answer", caller=caller, text="agent cannot answer")
        with self.assertRaisesRegex(ValueError, "only by a human answer"):
            self.change(decision, "done", caller=HUMAN)
        answered = self.change(decision, "answer", caller=HUMAN, text="Use the smaller path")
        self.assertEqual("done", answered["state"])
        self.assertEqual("Use the smaller path", answered["detail"])
        self.assertIsNone(answered["owed_to"])
        self.assertEqual(2, self.store.db.execute("SELECT COUNT(*) FROM transitions").fetchone()[0])
        self.assertEqual(2, self.store.db.execute("SELECT COUNT(*) FROM operations").fetchone()[0])

    def test_replacement_never_inherits_but_explicit_reassignment_works(self):
        root = self.create()["task"]
        child = self.create("child", parent=root["id"])["task"]
        child = self.change(child, "delegate", owner=B)
        replacement = {**B, "execution": "boot:21:201"}
        with self.assertRaisesRegex(ValueError, "not owned"):
            self.change(child, "start", caller=replacement)
        child = self.change(child, "delegate", owner=replacement)
        self.assertEqual(replacement, child["owner"])
        self.assertEqual(HUMAN, child["creator"])
        with self.assertRaisesRegex(ValueError, "not owned"):
            self.change(child, "start", caller=B)
        child = self.change(child, "start", caller=replacement)
        self.assertEqual("in_progress", child["state"])

    def test_stale_revision_and_parent_cycle_reject_without_partial_writes(self):
        root = self.create()["task"]
        started = self.change(root, "start")
        with self.assertRaisesRegex(ValueError, "stale"):
            self.change(root, "block", text="stale")
        current = self.invoke({"action": "get", "id": root["id"]})["task"]
        # Elapsed is a refreshed observation, not persisted mutation state.
        self.assertEqual({key: value for key, value in started.items() if key != "elapsed_ms"},
                         {key: value for key, value in current.items() if key != "elapsed_ms"})
        child = self.create("child", parent=root["id"])["task"]
        self.store.db.execute("UPDATE tasks SET parent=? WHERE id=?", (child["id"], root["id"]))
        with self.assertRaisesRegex(ValueError, "cycle"):
            self.create("cycle", parent=root["id"])
        self.assertEqual(2, self.store.db.execute("SELECT COUNT(*) FROM tasks").fetchone()[0])

    def test_two_concurrent_revisions_have_one_winner(self):
        task = self.create()["task"]
        barrier = threading.Barrier(2)
        def writer(action):
            store = provider.Store(self.root.name)
            barrier.wait(timeout=5)
            try:
                return store.invoke({"caller": HUMAN, "payload": {"action": action, "id": task["id"], "revision": 1, "operation_id": action, **({"text": "blocked"} if action == "block" else {})}})
            except ValueError as error:
                return str(error)
            finally:
                store.db.close()
        with ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(writer, ("start", "block")))
        self.assertEqual(1, sum(isinstance(result, dict) for result in results))
        self.assertEqual(1, sum("stale task revision" in str(result) for result in results))
        self.assertEqual(2, self.store.db.execute("SELECT COUNT(*) FROM operations").fetchone()[0])

    def test_two_concurrent_same_operations_commit_once(self):
        barrier = threading.Barrier(2)
        def writer(_):
            store = provider.Store(self.root.name)
            barrier.wait(timeout=5)
            try:
                return store.invoke({"caller": HUMAN, "payload": {"action": "create", "title": "once", "done_when": "Result is recorded.", "operation_id": "same"}})
            finally:
                store.db.close()
        with ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(writer, (1, 2)))
        self.assertEqual(results[0], results[1])
        for table in ("tasks", "transitions", "operations"):
            self.assertEqual(1, self.store.db.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0])

    def test_subprocess_crash_before_and_after_commit_recovers_atomic_outcome(self):
        for after in (False, True):
            with self.subTest(after=after), tempfile.TemporaryDirectory() as directory:
                code = '''import os,sys,provider
s=provider.Store(sys.argv[1])
a={"class":"human_unverified","terminal_id":"term_human","uid":1000}
p={"action":"create","title":"crash","done_when":"Result is recorded.","operation_id":"crash"}
if sys.argv[2]=="after": s.invoke({"caller":a,"payload":p})
else:
 s.db.execute("BEGIN IMMEDIATE")
 s.create_task(p,a,provider.timestamp())
os._exit(17)
'''
                result = subprocess.run([sys.executable, "-B", "-c", code, directory, "after" if after else "before"], env={"PYTHONPATH": str(Path(__file__).parent.resolve())}, timeout=10, capture_output=True)
                self.assertEqual(17, result.returncode, result.stderr)
                store = provider.Store(directory)
                receipt = store.invoke({"caller": HUMAN, "payload": {"action": "op", "operation_id": "crash"}})
                self.assertEqual(after, receipt["found"])
                for table in ("tasks", "transitions", "operations"):
                    self.assertEqual(int(after), store.db.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0])
                store.db.close()

    def test_large_report_pages_stay_under_frame_budget(self):
        for index in range(40):
            task = self.create(f"large-{index}", title="雪" * 300)["task"]
            self.change(task, "report", text="雪" * 1300)
        cursor, count = None, 0
        while True:
            page = self.invoke({"action": "list", **({"after": cursor} if cursor else {})}, UNKNOWN)
            self.assertLess(len(provider.canonical(page).encode()), provider.MAX_REPLY_BYTES)
            count += len(page["tasks"])
            cursor = page["next_cursor"]
            if cursor is None:
                break
        self.assertEqual(40, count)

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
        other = {**HUMAN, "terminal_id": "another-human-pane"}
        second = self.create(caller=other)
        self.assertNotEqual(first["task"]["id"], second["task"]["id"])
        self.assertEqual(first, self.lookup()["outcome"])
        self.assertEqual(second, self.lookup(caller=other)["outcome"])

    def test_unknown_read_requires_explicit_original_operation_scope(self):
        first = self.create()
        with self.assertRaisesRegex(ValueError, "unknown caller"):
            self.lookup(caller=UNKNOWN)
        result = self.lookup(caller=UNKNOWN, lookup_caller=HUMAN)
        self.assertTrue(result["authoritative"])
        self.assertEqual(HUMAN, result["caller"])
        self.assertEqual(first, result["outcome"])
        absent = self.lookup("absent", caller=UNKNOWN, lookup_caller=HUMAN)
        self.assertTrue(absent["authoritative"])
        self.assertFalse(absent["found"])

    def test_expected_caller_is_a_precondition_not_authority(self):
        with self.assertRaisesRegex(ValueError, "caller changed"):
            self.create(expected_caller=B)
        with self.assertRaisesRegex(ValueError, "unknown caller"):
            self.create(caller=UNKNOWN, expected_caller=A)
        self.assertEqual(HUMAN, self.create(expected_caller=HUMAN)["caller"])

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
                    store.invoke({"caller": HUMAN, "payload": {"action": "create", "operation_id": "uncertain", "title": "work", "done_when": "Result is recorded."}})
                reopened = provider.Store(directory)
                result = reopened.invoke({"caller": HUMAN, "payload": {"action": "op", "operation_id": "uncertain"}})
                self.assertEqual(after, result["found"])
                self.assertTrue(result["authoritative"])
                reopened.db.close()

    def test_commit_and_cleanup_fault_preserves_unknown_not_rejection(self):
        self.store.db = FaultCommit(self.store.db, True, close_error=True)
        with self.assertRaises(provider.OutcomeUnknown):
            self.create("uncertain-cleanup")
        reopened = provider.Store(self.root.name)
        result = reopened.invoke({"caller": HUMAN, "payload": {"action": "op", "operation_id": "uncertain-cleanup"}})
        self.assertTrue(result["found"])
        reopened.db.close()

    def test_rollback_and_cleanup_fault_preserves_unknown_not_rejection(self):
        self.store.db = FaultCommit(self.store.db, False, close_error=True, rollback_error=True)
        with self.assertRaises(provider.OutcomeUnknown):
            self.create("rollback-cleanup", title="")
        reopened = provider.Store(self.root.name)
        result = reopened.invoke({"caller": HUMAN, "payload": {"action": "op", "operation_id": "rollback-cleanup"}})
        self.assertFalse(result["found"])
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
