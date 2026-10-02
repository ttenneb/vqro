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
FINAL_STATES = {"accepted", "done"}
HUMAN_ROLE = {"role": "human_unverified"}


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
        self.migrate()

    def migrate(self):
        # Only this extension owns this database. Preserve original spike bindings.
        self.db.execute("BEGIN IMMEDIATE")
        try:
            columns = {row["name"] for row in self.db.execute("PRAGMA table_info(tasks)")}
            for name, definition in (("creator", "TEXT"), ("delegator", "TEXT"), ("detail", "TEXT NOT NULL DEFAULT ''")):
                if name not in columns:
                    self.db.execute(f"ALTER TABLE tasks ADD COLUMN {name} {definition}")
            self.db.execute("CREATE INDEX IF NOT EXISTS task_parent ON tasks(parent)")
            self.db.execute("CREATE INDEX IF NOT EXISTS task_transition ON transitions(task_id,seq)")
            self.db.execute("CREATE INDEX IF NOT EXISTS operation_lookup ON operations(operation_id,caller)")
            self.db.execute("UPDATE tasks SET creator=owner WHERE creator IS NULL")
            self.db.execute("PRAGMA user_version=1")
            self.commit()
        except OutcomeUnknown:
            raise
        except Exception:
            self.rollback()
            raise

    def task(self, task_id):
        row = self.db.execute("SELECT * FROM tasks WHERE id=?", (task_id,)).fetchone()
        if row is None:
            raise ValueError("task not found")
        value = dict(row)
        for key in ("owner", "creator", "delegator"):
            value[key] = json.loads(value[key]) if value[key] is not None else None
        if value["state"] in FINAL_STATES:
            owed_to, owed_action = None, None
        elif value["kind"] == "decision":
            owed_to, owed_action = HUMAN_ROLE, "answer"
        elif value["state"] == "reported":
            owed_to, owed_action = HUMAN_ROLE, "accept"
        else:
            owed_to = value["owner"]
            owed_action = {"open": "start", "in_progress": "work/report", "blocked": "unblock"}.get(value["state"])
        value.update(owed_to=owed_to, owed_action=owed_action, owed_since=value["updated_at"] if owed_to else None)
        last = self.db.execute("SELECT state,at,caller FROM transitions WHERE task_id=? ORDER BY seq DESC LIMIT 1", (task_id,)).fetchone()
        value["last_transition"] = {"state": last["state"], "at": last["at"], "caller": json.loads(last["caller"])} if last else None
        return value

    def commit(self):
        try:
            self.db.execute("COMMIT")
        except sqlite3.Error as error:
            try:
                self.db.close()
            finally:
                # Cleanup failure must never turn an uncertain commit into rejection.
                raise OutcomeUnknown("commit outcome unknown; inspect the original caller's operation") from error

    def rollback(self):
        try:
            if self.db.in_transaction:
                self.db.execute("ROLLBACK")
        except sqlite3.Error as error:
            try:
                self.db.close()
            finally:
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

    def parent_for_create(self, parent, caller):
        if parent is None:
            return
        task = self.task(parent)
        if task["kind"] != "task" or task["state"] not in ("open", "in_progress", "blocked"):
            raise ValueError("parent is not open for new work")
        if caller["class"] == "agent" and task["owner"] != caller:
            raise ValueError("parent is not owned by this execution")
        seen = set()
        while parent is not None:
            if parent in seen:
                raise ValueError("parent cycle rejected")
            seen.add(parent)
            row = self.db.execute("SELECT parent FROM tasks WHERE id=?", (parent,)).fetchone()
            if row is None:
                raise ValueError("parent not found")
            parent = row["parent"]

    def require_owner(self, task, caller, delegation=False):
        if caller["class"] == "human_unverified" or task["owner"] == caller:
            return
        if delegation and (task["creator"] == caller or task["delegator"] == caller
                           or (task["parent"] and self.task(task["parent"])["owner"] == caller)):
            return
        raise ValueError("task is not owned or delegated by this execution")

    def transition(self, payload, caller, at):
        task = self.task(payload.get("id"))
        revision = payload.get("revision")
        if type(revision) is not int or revision != task["revision"]:
            raise ValueError("stale task revision")
        action = payload["action"]
        human = caller["class"] == "human_unverified"
        if action in ("accept", "answer") and not human:
            raise ValueError("human-only acceptance/answer (cooperative, unverified)")
        if task["kind"] == "decision" and action != "answer":
            raise ValueError("decision tasks close only by a human answer")
        if task["kind"] != "decision" and action == "answer":
            raise ValueError("only a decision can be answered")
        if action not in ("accept", "answer"):
            self.require_owner(task, caller, delegation=action == "delegate")
        state, owner, delegator, detail = task["state"], task["owner"], task["delegator"], task["detail"]
        allowed = {
            "start": ({"open", "blocked"}, "in_progress"),
            "block": ({"open", "in_progress"}, "blocked"),
            "report": ({"open", "in_progress", "blocked"}, "reported"),
            "accept": ({"reported"}, "accepted"),
            "done": ({"accepted"}, "done"),
            "answer": ({"open"}, "done"),
            "delegate": ({"open", "in_progress", "blocked"}, "open"),
        }
        sources, destination = allowed[action]
        if state not in sources:
            raise ValueError(f"{action} is not allowed from {state}")
        if action in ("report", "answer") and self.db.execute("SELECT 1 FROM tasks WHERE parent=? AND state NOT IN ('accepted','done') LIMIT 1", (task["id"],)).fetchone():
            raise ValueError("unfinished children must be accepted/done first")
        if action in ("block", "report", "answer"):
            detail = payload.get("text")
            if not isinstance(detail, str) or not detail.strip() or len(detail.encode()) > 4096:
                raise ValueError("text must be nonempty and at most 4096 bytes")
        if action == "start":
            detail = ""
        if action == "delegate":
            owner = checked_binding(payload.get("owner"))
            delegator, detail = caller, ""
        self.db.execute("UPDATE tasks SET state=?,owner=?,delegator=?,detail=?,revision=revision+1,updated_at=? WHERE id=? AND revision=?",
                        (destination, canonical(owner), canonical(delegator) if delegator else None, detail, at, task["id"], revision))
        self.db.execute("INSERT INTO transitions(task_id,state,at,caller) VALUES (?,?,?,?)", (task["id"], destination, at, canonical(caller)))
        return task["id"]

    def create_task(self, payload, caller, at):
        title = payload.get("title")
        if not isinstance(title, str) or not title.strip() or len(title.encode()) > 1024:
            raise ValueError("title must be nonempty and at most 1024 bytes")
        kind = "decision" if payload["action"] == "decision" else payload.get("kind", "task")
        if kind not in ("task", "decision"):
            raise ValueError("invalid task kind")
        parent = payload.get("parent")
        self.parent_for_create(parent, caller)
        task_id = "task_" + uuid.uuid4().hex
        owner = HUMAN_ROLE if kind == "decision" else caller
        self.db.execute("INSERT INTO tasks(id,parent,title,kind,owner,state,revision,created_at,updated_at,creator,detail) VALUES (?,?,?,?,?,'open',1,?,?,?,'')",
                        (task_id, parent, title.strip(), kind, canonical(owner), at, at, canonical(caller)))
        self.db.execute("INSERT INTO transitions(task_id,state,at,caller) VALUES (?,?,?,?)", (task_id, "open", at, canonical(caller)))
        return task_id

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
            "decision": {"action", "operation_id", "title", "parent", "expected_caller"},
            **{action: {"action", "operation_id", "id", "revision", "expected_caller"} | ({"owner"} if action == "delegate" else {"text"} if action in ("block", "report", "answer") else set())
               for action in ("delegate", "start", "block", "report", "accept", "done", "answer")},
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
            tables = ("tasks", "operations", "transitions") if action in ("create", "decision") else ("operations", "transitions")
            for table in tables:
                if self.db.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0] >= MAX_RECORDS:
                    raise ValueError("store quota reached; no receipts are evicted")
            at = timestamp()
            task_id = self.create_task(payload, owner, at) if action in ("create", "decision") else self.transition(payload, owner, at)
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
