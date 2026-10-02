"""Persisted Slice 1 data and historical receipts must survive graph migrations."""
import hashlib
import json
from pathlib import Path
import sqlite3
import tempfile
import unittest
from unittest import mock

import provider


class MigrationTests(unittest.TestCase):
    def test_slice_one_database_keeps_records_and_historical_operation_outcomes(self):
        with tempfile.TemporaryDirectory() as root:
            db = sqlite3.connect(Path(root) / "tasks.sqlite")
            db.executescript("""
                CREATE TABLE tasks (id TEXT PRIMARY KEY, parent TEXT REFERENCES tasks(id),
                  title TEXT NOT NULL, kind TEXT NOT NULL, owner TEXT NOT NULL,
                  state TEXT NOT NULL, revision INTEGER NOT NULL, created_at TEXT NOT NULL,
                  updated_at TEXT NOT NULL, creator TEXT, delegator TEXT, detail TEXT NOT NULL DEFAULT '');
                CREATE TABLE transitions (seq INTEGER PRIMARY KEY, task_id TEXT NOT NULL REFERENCES tasks(id),
                  state TEXT NOT NULL, at TEXT NOT NULL, caller TEXT NOT NULL);
                CREATE TABLE operations (caller TEXT NOT NULL, operation_id TEXT NOT NULL, payload TEXT NOT NULL,
                  outcome TEXT NOT NULL, at TEXT NOT NULL, PRIMARY KEY(caller,operation_id));
                PRAGMA user_version=1;
            """)
            caller = {"class": "agent", "terminal_id": "old", "agent": "pi", "execution": "boot:10:100"}
            encoded = provider.canonical(caller)
            historical = {"task": {"id": "old-root", "state": "reported", "revision": 2}, "caller": caller, "operation_id": "old-report"}
            db.execute("INSERT INTO tasks VALUES (?,NULL,?,'task',?,'reported',2,?,?,?,NULL,?)",
                       ("old-root", "Legacy reported work", encoded, "2026-01-01T00:00:00+00:00", "2026-01-01T01:00:00+00:00", encoded, "Original evidence"))
            db.execute("INSERT INTO transitions VALUES (1,?,'reported',?,?)", ("old-root", "2026-01-01T01:00:00+00:00", encoded))
            db.execute("INSERT INTO operations VALUES (?,?,?,?,?)", (encoded, "old-report", "old-payload", provider.canonical(historical), "old-at"))
            original = {"action": "create", "title": "Legacy reported work", "operation_id": "old-create"}
            creation = {"task": {"id": "old-root", "state": "open", "revision": 1}, "caller": caller, "operation_id": "old-create"}
            db.execute("INSERT INTO operations VALUES (?,?,?,?,?)", (encoded, "old-create", hashlib.sha256(provider.canonical(original).encode()).hexdigest(), provider.canonical(creation), "old-at"))
            db.commit()
            db.close()
            store = provider.Store(root)
            try:
                task = store.task("old-root")
                self.assertEqual(("reported", 2, "Original evidence"), (task["state"], task["revision"], task["detail"]))
                self.assertIsNone(task.get("done_when"))
                self.assertEqual(caller, task["owner"])
                self.assertEqual(creation, store.invoke({"caller": caller, "payload": original}))
                self.assertEqual("acceptor_missing", task["owed_action"])
                self.assertEqual(historical, store.lookup({"operation_id": "old-report", "lookup_caller": caller}, {"class": "unknown"})["outcome"])
                self.assertEqual(1, store.db.execute("SELECT COUNT(*) FROM tasks").fetchone()[0])
                self.assertEqual(1, store.db.execute("SELECT COUNT(*) FROM transitions").fetchone()[0])
            finally:
                store.db.close()

    def test_legacy_first_start_uses_sequence_not_backwards_wall_clock(self):
        with tempfile.TemporaryDirectory() as root:
            db = sqlite3.connect(Path(root) / "tasks.sqlite")
            db.executescript("""
                CREATE TABLE tasks (id TEXT PRIMARY KEY,parent TEXT,title TEXT,kind TEXT,owner TEXT,state TEXT,
                  revision INTEGER,created_at TEXT,updated_at TEXT,creator TEXT,delegator TEXT,detail TEXT);
                CREATE TABLE transitions (seq INTEGER PRIMARY KEY,task_id TEXT,state TEXT,at TEXT,caller TEXT);
                CREATE TABLE operations (caller TEXT,operation_id TEXT,payload TEXT,outcome TEXT,at TEXT,PRIMARY KEY(caller,operation_id));
                PRAGMA user_version=1;
            """)
            caller = {"class": "human_unverified", "terminal_id": "legacy-human", "uid": 1000}
            encoded = provider.canonical(caller)
            first, resume = "2026-01-01T10:00:00+00:00", "2026-01-01T09:00:00+00:00"
            db.execute("INSERT INTO tasks VALUES ('timing',NULL,'Timing','task',?,'blocked',4,?,?,?,NULL,'pause')", (encoded, first, resume, encoded))
            for seq, state, at in ((1, "in_progress", first), (2, "blocked", first), (3, "in_progress", resume), (4, "blocked", resume)):
                db.execute("INSERT INTO transitions VALUES (?,'timing',?,?,?)", (seq, state, at, encoded))
            historical = json.dumps({"task": {"id": "timing", "state": "blocked", "revision": 4}}, indent=2)
            db.execute("INSERT INTO operations VALUES (?,'old','old-hash',?,'old-at')", (encoded, historical))
            db.commit()
            db.close()
            store = provider.Store(root)
            self.assertEqual(first, store.task("timing")["started_at"])
            self.assertEqual(("old-hash", historical), tuple(store.db.execute("SELECT payload,outcome FROM operations WHERE operation_id='old'").fetchone()))
            with mock.patch.object(provider, "timestamp", return_value="2026-01-01T08:00:00+00:00"):
                result = store.invoke({"caller": caller, "payload": {"action": "start", "id": "timing", "revision": 4, "operation_id": "resume"}})
            self.assertEqual(first, result["task"]["started_at"])
            store.db.close()
            reopened = provider.Store(root)
            self.assertEqual(first, reopened.task("timing")["started_at"])
            self.assertEqual(3, reopened.db.execute("PRAGMA user_version").fetchone()[0])
            self.assertEqual(("old-hash", historical), tuple(reopened.db.execute("SELECT payload,outcome FROM operations WHERE operation_id='old'").fetchone()))
            reopened.db.close()

    def test_failed_additive_migration_rolls_back_schema_data_and_version(self):
        class FailSQL:
            def __init__(self, db, needle):
                self.db, self.needle = db, needle
            def __getattr__(self, name):
                return getattr(self.db, name)
            def execute(self, sql, *args):
                if self.needle in sql:
                    raise sqlite3.OperationalError("injected migration failure")
                return self.db.execute(sql, *args)
        for needle in ("ADD COLUMN actual_work_ms", "CREATE TABLE IF NOT EXISTS dependencies", "ADD COLUMN decision", "CREATE TABLE IF NOT EXISTS decision_log", "PRAGMA user_version=3"):
            with self.subTest(needle=needle), tempfile.TemporaryDirectory() as root:
                db = sqlite3.connect(Path(root) / "tasks.sqlite", isolation_level=None)
                db.row_factory = sqlite3.Row
                db.executescript("""
                    CREATE TABLE tasks (id TEXT PRIMARY KEY,parent TEXT,title TEXT,kind TEXT,owner TEXT,state TEXT,
                      revision INTEGER,created_at TEXT,updated_at TEXT,creator TEXT,delegator TEXT,detail TEXT);
                    CREATE TABLE transitions (seq INTEGER PRIMARY KEY,task_id TEXT,state TEXT,at TEXT,caller TEXT);
                    CREATE TABLE operations (caller TEXT,operation_id TEXT,payload TEXT,outcome TEXT,at TEXT,PRIMARY KEY(caller,operation_id));
                    PRAGMA user_version=1;
                """)
                before = [tuple(row) for row in db.execute("SELECT name,sql FROM sqlite_master ORDER BY name")]
                store = provider.Store.__new__(provider.Store)
                store.db = FailSQL(db, needle)
                store.decisions = provider.decision_queue.Queue(store, 900, provider.OutcomeUnknown)
                with self.assertRaisesRegex(sqlite3.Error, "injected"):
                    store.migrate()
                self.assertFalse(db.in_transaction)
                self.assertEqual(1, db.execute("PRAGMA user_version").fetchone()[0])
                self.assertEqual(before, [tuple(row) for row in db.execute("SELECT name,sql FROM sqlite_master ORDER BY name")])
                db.close()
                reopened = provider.Store(root)
                self.assertEqual(3, reopened.db.execute("PRAGMA user_version").fetchone()[0])
                reopened.db.close()

    def test_future_database_version_rejected_without_schema_mutation(self):
        with tempfile.TemporaryDirectory() as root:
            db = sqlite3.connect(Path(root) / "tasks.sqlite")
            db.execute("PRAGMA user_version=4")
            db.execute("CREATE TABLE future_data (value TEXT)")
            db.execute("INSERT INTO future_data VALUES ('preserve')")
            db.commit()
            before = db.execute("SELECT name,sql FROM sqlite_master").fetchall()
            with self.assertRaisesRegex(ValueError, "future"):
                provider.Store(root)
            self.assertEqual(4, db.execute("PRAGMA user_version").fetchone()[0])
            self.assertEqual(before, db.execute("SELECT name,sql FROM sqlite_master").fetchall())
            self.assertEqual("preserve", db.execute("SELECT value FROM future_data").fetchone()[0])
            db.close()
