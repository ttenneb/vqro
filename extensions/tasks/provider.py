#!/usr/bin/env python3
"""Trusted executable task authority. Host supplies caller facts, not task policy."""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import sys
import uuid

SERVICE = "vqro.tasks.core"
MAX_RECORDS = 10000


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"))


def timestamp():
    return datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="milliseconds")


def binding(caller):
    if caller.get("class") == "agent":
        return {key: caller[key] for key in ("class", "terminal_id", "agent", "execution")}
    if caller.get("class") == "human_unverified":
        return {key: caller[key] for key in ("class", "terminal_id", "uid")}
    raise ValueError("unknown caller cannot mutate tasks")


class Store:
    def __init__(self, directory):
        Path(directory).mkdir(parents=True, exist_ok=True, mode=0o700)
        self.db = sqlite3.connect(Path(directory) / "tasks.sqlite", timeout=1, isolation_level=None)
        self.db.row_factory = sqlite3.Row
        self.db.execute("PRAGMA journal_mode=WAL")
        self.db.execute("PRAGMA synchronous=FULL")
        self.db.execute("PRAGMA foreign_keys=ON")
        self.db.executescript("""
            CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY, parent TEXT REFERENCES tasks(id), title TEXT NOT NULL,
                kind TEXT NOT NULL, owner TEXT NOT NULL, state TEXT NOT NULL,
                revision INTEGER NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS transitions (
                seq INTEGER PRIMARY KEY, task_id TEXT NOT NULL REFERENCES tasks(id),
                state TEXT NOT NULL, at TEXT NOT NULL, caller TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS operations (
                caller TEXT NOT NULL, operation_id TEXT NOT NULL, payload TEXT NOT NULL,
                outcome TEXT NOT NULL, at TEXT NOT NULL, PRIMARY KEY(caller, operation_id)
            );
        """)

    def task(self, task_id):
        row = self.db.execute("SELECT * FROM tasks WHERE id=?", (task_id,)).fetchone()
        if row is None:
            raise ValueError("task not found")
        value = dict(row)
        value["owner"] = json.loads(value["owner"])
        return value

    def invoke(self, params):
        if set(params) != {"caller", "payload"}:
            raise ValueError("invalid host action envelope")
        caller, payload = params["caller"], params["payload"]
        if not isinstance(payload, dict):
            raise ValueError("payload must be an object")
        action = payload.get("action")
        fields = {
            "list": {"action"}, "get": {"action", "id"},
            "op": {"action", "operation_id"},
            "create": {"action", "operation_id", "title", "parent", "kind"},
        }
        if action not in fields or set(payload) - fields[action]:
            raise ValueError("unknown action or field (actor flags are not accepted)")
        if action == "list":
            ids = self.db.execute("SELECT id FROM tasks ORDER BY created_at,id").fetchall()
            return {"tasks": [self.task(row["id"]) for row in ids], "caller": caller}
        if action == "get":
            return {"task": self.task(payload["id"])}
        if action == "op":
            rows = self.db.execute("SELECT caller,outcome FROM operations WHERE operation_id=?", (payload["operation_id"],)).fetchall()
            return {"found": bool(rows), "outcomes": [{"caller": json.loads(row["caller"]), "outcome": json.loads(row["outcome"])} for row in rows]}
        owner = binding(caller)
        caller_key = canonical(owner)
        operation_id = payload.get("operation_id")
        if not isinstance(operation_id, str) or not 1 <= len(operation_id) <= 128:
            raise ValueError("operation_id must be 1..128 characters")
        payload_key = hashlib.sha256(canonical(payload).encode()).hexdigest()
        self.db.execute("BEGIN IMMEDIATE")
        try:
            old = self.db.execute("SELECT payload,outcome FROM operations WHERE caller=? AND operation_id=?", (caller_key, operation_id)).fetchone()
            if old is not None:
                if old["payload"] != payload_key:
                    raise ValueError("operation payload conflict")
                self.db.execute("COMMIT")
                return json.loads(old["outcome"])
            for table in ("tasks", "operations", "transitions"):
                if self.db.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0] >= MAX_RECORDS:
                    raise ValueError("store quota reached; no receipts are evicted")
            title = payload.get("title")
            if not isinstance(title, str) or not title.strip() or len(title.encode()) > 1024:
                raise ValueError("title must be nonempty and at most 1024 bytes")
            kind = payload.get("kind", "task")
            if kind not in ("task", "decision"):
                raise ValueError("invalid task kind")
            parent = payload.get("parent")
            if parent is not None:
                parent_task = self.task(parent)
                if owner["class"] == "agent" and parent_task["owner"] != owner:
                    raise ValueError("parent is not owned by this execution")
            task_id, at = "task_" + uuid.uuid4().hex, timestamp()
            self.db.execute("INSERT INTO tasks VALUES (?,?,?,?,?,?,?,?,?)", (task_id, parent, title.strip(), kind, caller_key, "open", 1, at, at))
            self.db.execute("INSERT INTO transitions(task_id,state,at,caller) VALUES (?,?,?,?)", (task_id, "open", at, caller_key))
            outcome = {"task": self.task(task_id), "operation_id": operation_id}
            self.db.execute("INSERT INTO operations VALUES (?,?,?,?,?)", (caller_key, operation_id, payload_key, canonical(outcome), at))
            self.db.execute("COMMIT")
            return outcome
        except Exception:
            if self.db.in_transaction:
                self.db.execute("ROLLBACK")
            raise


def emit(value):
    sys.stdout.write(canonical(value) + "\n")
    sys.stdout.flush()


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--data-dir")
    args = parser.parse_args()
    directory = args.data_dir or str(Path(os.environ.get("XDG_STATE_HOME", str(Path.home() / ".local/state"))) / "vqro/extensions/vqro.tasks")
    store = Store(directory)
    emit({"type": "hello", "protocol": 2, "service_id": SERVICE, "requested_capabilities": ["action_invoke"], "methods": ["action.invoke.v1"]})
    welcome = json.loads(sys.stdin.readline())
    if welcome.get("type") != "welcome" or welcome.get("granted_capabilities") != ["action_invoke"] or "action.invoke.v1" not in welcome.get("methods", []):
        raise ValueError("action capability not granted")
    for line in sys.stdin:
        request = json.loads(line)
        if request.get("type") != "request" or request.get("method") != "action.invoke.v1":
            raise ValueError("unexpected request")
        try:
            result = {"ok": True, "value": store.invoke(request["params"])}
        except (ValueError, KeyError, TypeError, sqlite3.Error) as error:
            result = {"ok": False, "error": str(error)}
        # A successful mutation reaches this reply only after the FULL commit.
        emit({"type": "response", "id": request["id"], "result": result})


if __name__ == "__main__":
    main()
