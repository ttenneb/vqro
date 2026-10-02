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
PAGE_SIZE = 32
MAX_REPLY_BYTES = 128 * 1024


class OutcomeUnknown(Exception):
    """A commit/rollback error cannot be reported as a safe-to-retry rejection."""


def operation_id(value):
    if not isinstance(value, str) or not 1 <= len(value) <= 128 or not all(char.isascii() and (char.isalnum() or char in "._:-") for char in value):
        raise ValueError("operation_id must be 1..128 ASCII letters, digits, . _ : or -")
    return value


def checked_binding(value):
    if not isinstance(value, dict):
        raise ValueError("caller binding must be an object")
    keys = {"class", "terminal_id", "agent", "execution"} if value.get("class") == "agent" else {"class", "terminal_id", "uid"}
    if set(value) != keys or value.get("class") not in ("agent", "human_unverified"):
        raise ValueError("invalid caller binding")
    for key in keys - {"uid"}:
        if not isinstance(value[key], str) or not 1 <= len(value[key].encode()) <= 256:
            raise ValueError("invalid caller binding field")
    if "uid" in keys and (type(value["uid"]) is not int or not 0 <= value["uid"] <= 0xffffffff):
        raise ValueError("invalid caller uid")
    return value


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
        self.db.execute("PRAGMA max_page_count=262144")
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

    def commit(self):
        try:
            self.db.execute("COMMIT")
        except sqlite3.Error as error:
            self.db.close()
            raise OutcomeUnknown("commit outcome unknown; inspect the original caller's operation") from error

    def rollback(self):
        try:
            if self.db.in_transaction:
                self.db.execute("ROLLBACK")
        except sqlite3.Error as error:
            self.db.close()
            raise OutcomeUnknown("rollback outcome unknown; inspect the original caller's operation") from error

    def list_tasks(self, after=None):
        if after is not None and (not isinstance(after, str) or len(after) > 128):
            raise ValueError("invalid task cursor")
        rows = self.db.execute("SELECT id FROM tasks WHERE id>? ORDER BY id LIMIT ?", (after or "", PAGE_SIZE + 1)).fetchall()
        tasks, used = [], 4096
        for row in rows[:PAGE_SIZE]:
            task = self.task(row["id"])
            size = len(canonical(task).encode()) + 1
            if used + size > MAX_REPLY_BYTES:
                break
            tasks.append(task)
            used += size
        if rows and not tasks:
            raise ValueError("task exceeds response budget")
        more = len(rows) > len(tasks)
        return {"tasks": tasks, "next_cursor": tasks[-1]["id"] if more else None}

    def lookup(self, payload, caller):
        op = operation_id(payload.get("operation_id"))
        explicit = "lookup_caller" in payload
        selected = checked_binding(payload["lookup_caller"]) if explicit else binding(caller)
        key = canonical(selected)
        self.db.execute("BEGIN IMMEDIATE")
        try:
            row = self.db.execute("SELECT outcome FROM operations WHERE caller=? AND operation_id=?", (key, op)).fetchone()
            other = self.db.execute("SELECT 1 FROM operations WHERE operation_id=? AND caller<>? LIMIT 1", (op, key)).fetchone() is not None
            self.commit()
            return {"operation_id": op, "caller": selected, "found": row is not None,
                    "authoritative": explicit or row is not None or not other,
                    "other_caller_exists": other, "outcome": json.loads(row["outcome"]) if row else None}
        except OutcomeUnknown:
            raise
        except Exception:
            self.rollback()
            raise

    def invoke(self, params):
        if set(params) != {"caller", "payload"}:
            raise ValueError("invalid host action envelope")
        caller, payload = params["caller"], params["payload"]
        if not isinstance(payload, dict):
            raise ValueError("payload must be an object")
        action = payload.get("action")
        fields = {
            "list": {"action", "after"}, "get": {"action", "id"},
            "whoami": {"action"},
            "op": {"action", "operation_id", "lookup_caller"},
            "create": {"action", "operation_id", "title", "parent", "kind", "expected_caller"},
        }
        if action not in fields or set(payload) - fields[action]:
            raise ValueError("unknown action or field (actor flags are not accepted)")
        if action == "whoami":
            return {"caller": binding(caller) if caller.get("class") != "unknown" else {"class": "unknown"}}
        if action == "list":
            return self.list_tasks(payload.get("after"))
        if action == "get":
            return {"task": self.task(payload["id"])}
        if action == "op":
            return self.lookup(payload, caller)
        owner = binding(caller)
        if "expected_caller" in payload and checked_binding(payload["expected_caller"]) != owner:
            raise ValueError("caller changed; this is not the original operation scope")
        caller_key = canonical(owner)
        op = operation_id(payload.get("operation_id"))
        # Scope already keys the receipt. This client precondition is not new intent.
        intent = {key: value for key, value in payload.items() if key != "expected_caller"}
        payload_key = hashlib.sha256(canonical(intent).encode()).hexdigest()
        self.db.execute("BEGIN IMMEDIATE")
        try:
            old = self.db.execute("SELECT payload,outcome FROM operations WHERE caller=? AND operation_id=?", (caller_key, op)).fetchone()
            if old is not None:
                if old["payload"] != payload_key:
                    raise ValueError("operation payload conflict")
                self.commit()
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
            outcome = {"task": self.task(task_id), "operation_id": op, "caller": owner}
            self.db.execute("INSERT INTO operations VALUES (?,?,?,?,?)", (caller_key, op, payload_key, canonical(outcome), at))
            self.commit()
            return outcome
        except OutcomeUnknown:
            raise
        except Exception:
            self.rollback()
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
        except OutcomeUnknown as error:
            emit({"type": "response", "id": request["id"], "error": str(error)})
            return
        except (ValueError, KeyError, TypeError, sqlite3.Error) as error:
            result = {"ok": False, "error": str(error)}
        if len(canonical(result).encode()) > MAX_REPLY_BYTES:
            emit({"type": "response", "id": request["id"], "error": "response exceeds bounded task result; inspect operation"})
            return
        # A successful mutation reaches this reply only after the FULL commit.
        emit({"type": "response", "id": request["id"], "result": result})


if __name__ == "__main__":
    main()
