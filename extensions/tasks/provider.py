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

import graph
import decision_queue
import select

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
    def __init__(self, directory, decision_delay_seconds=900):
        Path(directory).mkdir(parents=True, exist_ok=True, mode=0o700)
        self.db = sqlite3.connect(Path(directory) / "tasks.sqlite", timeout=1, isolation_level=None)
        self.db.row_factory = sqlite3.Row
        self.decisions = decision_queue.Queue(self, decision_delay_seconds, OutcomeUnknown)
        if self.db.execute("PRAGMA user_version").fetchone()[0] > 3:
            self.db.close()
            raise ValueError("unsupported future Tasks database version; no migration attempted")
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
        # Successful migration is atomic; don't repeat historical backfill on restarts.
        if self.db.execute("PRAGMA user_version").fetchone()[0] == 3:
            return
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
            for name in sorted(graph.META_FIELDS | {"started_at"}):
                if name not in columns:
                    definition = "INTEGER" if name in graph.MILLISECONDS else "TEXT"
                    self.db.execute(f"ALTER TABLE tasks ADD COLUMN {name} {definition}")
            self.db.execute("""CREATE TABLE IF NOT EXISTS dependencies (
                task_id TEXT NOT NULL REFERENCES tasks(id), prerequisite TEXT NOT NULL REFERENCES tasks(id),
                PRIMARY KEY(task_id,prerequisite), CHECK(task_id<>prerequisite))""")
            self.db.execute("CREATE INDEX IF NOT EXISTS dependency_prerequisite ON dependencies(prerequisite)")
            self.db.execute("UPDATE tasks SET started_at=(SELECT at FROM transitions WHERE task_id=tasks.id AND state='in_progress' ORDER BY seq LIMIT 1) WHERE started_at IS NULL")
            self.decisions.migrate(columns)
            self.db.execute("PRAGMA user_version=3")
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
        value["decision"] = self.decisions.describe(value)
        value.pop("decision_due_at")
        value.pop("decision_timer_revision")
        for key in ("owner", "creator", "delegator"):
            value[key] = json.loads(value[key]) if value[key] is not None else None
        if value["state"] in FINAL_STATES:
            owed_to, owed_action = None, None
        elif value["kind"] == "decision":
            owed_to, owed_action = HUMAN_ROLE, "answer"
        elif value["state"] == "reported":
            acceptors = self.acceptors(value)
            owed_to = {"role": "acceptors", "bindings": acceptors}
            owed_action = "accept" if acceptors else "acceptor_missing"
        else:
            owed_to = value["owner"]
            owed_action = {"open": "start", "in_progress": "work/report", "blocked": "unblock"}.get(value["state"])
        value.update(owed_to=owed_to, owed_action=owed_action, owed_since=value["updated_at"] if owed_to else None)
        value["acceptors"] = self.acceptors(value)
        value["depends_on"] = [row[0] for row in self.db.execute("SELECT prerequisite FROM dependencies WHERE task_id=? ORDER BY prerequisite", (task_id,))]
        value["waiting_on"] = [row[0] for row in self.db.execute("SELECT prerequisite FROM dependencies JOIN tasks ON tasks.id=prerequisite WHERE task_id=? AND state NOT IN ('accepted','done') ORDER BY prerequisite", (task_id,))]
        value["ready"] = value["kind"] == "task" and value["state"] == "open" and not value["waiting_on"]
        value["criteria_missing"] = value["done_when"] is None
        value["elapsed_ms"] = None
        if value["started_at"]:
            end = value["updated_at"] if value["state"] in FINAL_STATES else timestamp()
            value["elapsed_ms"] = max(0, int((datetime.datetime.fromisoformat(end) - datetime.datetime.fromisoformat(value["started_at"])).total_seconds() * 1000))
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

    def list_tasks(self, after=None, ready=False):
        if after is not None and (not isinstance(after, str) or len(after) > 128):
            raise ValueError("invalid task cursor")
        predicate = """AND kind='task' AND state='open' AND NOT EXISTS (
            SELECT 1 FROM dependencies JOIN tasks AS prerequisite ON prerequisite.id=dependencies.prerequisite
            WHERE task_id=tasks.id AND prerequisite.state NOT IN ('accepted','done'))""" if ready else ""
        rows = self.db.execute(f"SELECT id FROM tasks WHERE id>? {predicate} ORDER BY id LIMIT ?", (after or "", PAGE_SIZE + 1)).fetchall()
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

    def parent_for_create(self, parent, caller, check_owner=True):
        if parent is None:
            return
        task = self.task(parent)
        if task["kind"] != "task" or task["state"] not in ("open", "in_progress", "blocked"):
            raise ValueError("parent is not open for new work")
        if check_owner and caller["class"] == "agent" and task["owner"] != caller:
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

    @staticmethod
    def accept_match(left, right):
        if left.get("class") == right.get("class") == "human_unverified":
            return left["uid"] == right["uid"]
        return left == right

    def acceptors(self, task):
        if task["kind"] == "decision":
            return []
        candidates = [task["delegator"]]
        if task["parent"]:
            row = self.db.execute("SELECT owner FROM tasks WHERE id=?", (task["parent"],)).fetchone()
            candidates.append(json.loads(row[0]) if row else None)
        if task["owner"].get("class") == "human_unverified":
            candidates.append(task["owner"])
        result = []
        for candidate in candidates:
            if not candidate or candidate.get("class") not in ("agent", "human_unverified"):
                continue
            if candidate.get("class") == "agent" and candidate == task["owner"]:
                continue
            if not any(self.accept_match(candidate, existing) for existing in result):
                result.append(candidate)
        return result

    def own_step(self, task):
        if task["kind"] != "task" or task["delegator"] is not None or task["parent"] is None:
            return False
        parent = self.db.execute("SELECT owner FROM tasks WHERE id=?", (task["parent"],)).fetchone()
        return parent is not None and json.loads(parent[0]) == task["owner"]

    def check_graph(self):
        parents = {row[0]: row[1] for row in self.db.execute("SELECT id,parent FROM tasks")}
        graph.check_cycles(parents, list(self.db.execute("SELECT task_id,prerequisite FROM dependencies")))

    def set_dependencies(self, task_id, references, check=True):
        references = graph.dependencies(references)
        for prerequisite in references:
            if prerequisite == task_id or self.db.execute("SELECT 1 FROM tasks WHERE id=?", (prerequisite,)).fetchone() is None:
                raise ValueError("self or missing dependency rejected")
        old = self.db.execute("SELECT COUNT(*) FROM dependencies WHERE task_id=?", (task_id,)).fetchone()[0]
        self.quota({"dependencies": len(references) - old})
        self.db.execute("DELETE FROM dependencies WHERE task_id=?", (task_id,))
        self.db.executemany("INSERT INTO dependencies VALUES (?,?)", ((task_id, ref) for ref in references))
        if check:
            self.check_graph()

    def set_metadata(self, task_id, values):
        if values:
            keys = sorted(values)
            self.db.execute("UPDATE tasks SET " + ",".join(f"{key}=?" for key in keys) + " WHERE id=?", [*(values[key] for key in keys), task_id])

    def quota(self, additions):
        for table, count in additions.items():
            if self.db.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0] + count > MAX_RECORDS:
                raise ValueError("store quota reached; no receipts are evicted")

    def import_plan(self, payload, caller, at):
        nodes = payload.get("nodes")
        if not isinstance(nodes, list) or not 1 <= len(nodes) <= graph.MAX_PLAN_NODES:
            raise ValueError("plan must contain 1-100 nodes (also bounded by 64 KiB request)")
        parent = payload.get("parent")
        if parent is None and caller["class"] != "human_unverified":
            raise ValueError("root plan import requires human approval")
        if parent is not None:
            self.parent_for_create(parent, caller)
            if type(payload.get("revision")) is not int or payload["revision"] != self.task(parent)["revision"]:
                raise ValueError("stale plan parent revision")
        mapping, checked, edge_count = {}, {}, 0
        for node in nodes:
            if not isinstance(node, dict) or set(node) - (graph.META_FIELDS | decision_queue.FIELDS | {"key", "title", "kind", "owner", "parent", "depends_on"}):
                raise ValueError("unknown plan node field")
            key = node.get("key")
            if not isinstance(key, str) or not 1 <= len(key) <= 64 or not all(char.isascii() and (char.isalnum() or char in "_.-") for char in key) or key in mapping:
                raise ValueError("plan keys must be unique bounded ASCII identifiers")
            if self.db.execute("SELECT 1 FROM tasks WHERE id=?", (key,)).fetchone() is not None:
                raise ValueError("local plan key collides with a durable task ID")
            graph.metadata(node, required=True)
            refs = graph.dependencies(node.get("depends_on", []))
            edge_count += len(refs)
            if "owner" in node:
                checked_binding(node["owner"])
                if node.get("kind") == "decision":
                    raise ValueError("decision owner is the logical human role")
            mapping[key] = "task_" + uuid.uuid4().hex
            checked[key] = node
        if edge_count > graph.MAX_PLAN_EDGES:
            raise ValueError("plan exceeds 400 edges")
        self.quota({"tasks": len(nodes), "transitions": len(nodes), "dependencies": edge_count, "operations": 1})
        pending = dict(checked)
        while pending:
            progress = False
            for key, node in list(pending.items()):
                local_parent = node.get("parent")
                if local_parent is not None and local_parent not in mapping:
                    raise ValueError("plan parent must reference a local node")
                if local_parent in pending:
                    continue
                actual_parent = mapping[local_parent] if local_parent is not None else parent
                # Whole-import authority was checked at the existing attachment above. No node
                # may attach elsewhere; newly created descendants can have explicitly chosen owners.
                task_id = self.create_task({**node, "action": "decision" if node.get("kind") == "decision" else "create", "parent": actual_parent}, caller, at, task_id=mapping[key], importing=True)
                if "owner" in node:
                    owner = node["owner"]
                    self.db.execute("UPDATE tasks SET owner=?,delegator=? WHERE id=?", (canonical(owner), canonical(caller) if owner != caller else None, task_id))
                pending.pop(key)
                progress = True
            if not progress:
                raise ValueError("plan parent cycle rejected")
        for key, node in checked.items():
            self.set_dependencies(mapping[key], [mapping.get(ref, ref) for ref in node.get("depends_on", [])], check=False)
        self.check_graph()
        return {"tasks": mapping, "counts": {"tasks": len(nodes), "dependencies": edge_count}}

    def transition(self, payload, caller, at):
        task = self.task(payload.get("id"))
        revision = payload.get("revision")
        if type(revision) is not int or revision != task["revision"]:
            raise ValueError("stale task revision")
        action = payload["action"]
        if action in ("decision_timer", "decision_revert"):
            destination, detail = self.decisions.change(task, payload, caller, at)
            self.db.execute("UPDATE tasks SET state=?,detail=?,revision=revision+1,updated_at=? WHERE id=? AND revision=?", (destination, detail, at, task["id"], revision))
            self.db.execute("INSERT INTO transitions(task_id,state,at,caller) VALUES (?,?,?,?)", (task["id"], destination, at, canonical(caller)))
            if action == "decision_revert":
                self.decisions.event(task["id"], "revert", {"at": at, "caller": caller, "policy_result": task["decision"]["resolution"], "affected_work": self.decisions.affected(task["id"])})
            else:
                self.decisions.event(task["id"], "timer", {"at": at, "caller": caller, "auto_at": self.task(task["id"])["decision"]["auto_at"], "delay_floor_seconds": self.decisions.delay_seconds})
            return task["id"]
        human = caller["class"] == "human_unverified"
        if action == "answer" and not human:
            raise ValueError("human-only answer (cooperative, unverified)")
        if action == "accept":
            if caller["class"] == "agent" and task["owner"] == caller:
                raise ValueError("agent cannot accept its own work")
            if not any(self.accept_match(candidate, caller) for candidate in self.acceptors(task)):
                raise ValueError("caller is not the parent owner or stored delegator acceptor")
        if task["kind"] == "decision" and action not in ("answer", "update", "depends"):
            raise ValueError("decision tasks close only by a human answer")
        if task["kind"] != "decision" and action == "answer":
            raise ValueError("only a decision can be answered")
        if action not in ("accept", "answer"):
            self.require_owner(task, caller, delegation=action in ("delegate", "depends", "update"))
        state, owner, delegator, detail = task["state"], task["owner"], task["delegator"], task["detail"]
        if action in ("update", "depends"):
            if state not in ("open", "in_progress", "blocked"):
                raise ValueError("metadata/dependencies are frozen after reporting")
            if action == "update":
                values = graph.metadata(payload)
                if not values:
                    raise ValueError("update needs at least one metadata field")
                self.set_metadata(task["id"], values)
            else:
                self.set_dependencies(task["id"], payload.get("depends_on"))
            destination = state
        else:
            destination = None
        allowed = {
            "start": ({"open", "blocked"}, "in_progress"),
            "block": ({"open", "in_progress"}, "blocked"),
            "report": ({"open", "in_progress", "blocked"}, "reported"),
            "accept": ({"reported"}, "accepted"),
            "done": ({"accepted"}, "done"),
            "answer": ({"open"}, "done"),
            "delegate": ({"open", "in_progress", "blocked"}, "open"),
        }
        direct_done = action == "done" and state == "in_progress" and task["owner"] == caller and self.own_step(task)
        if destination is None:
            sources, destination = allowed[action]
            if state not in sources and not direct_done:
                raise ValueError(f"{action} is not allowed from {state}")
        if action in ("start", "report", "answer", "done") and task["waiting_on"]:
            raise ValueError("unfinished dependencies must be accepted/done first")
        if action in ("report", "answer", "done") and self.db.execute("SELECT 1 FROM tasks WHERE parent=? AND state NOT IN ('accepted','done') LIMIT 1", (task["id"],)).fetchone():
            raise ValueError("unfinished children must be accepted/done first")
        structured_answer = action == "answer" and task["decision"] is not None
        decision_result = None
        if structured_answer:
            detail, decision_result = self.decisions.choice(task, payload, at, "human")
        elif action in ("block", "report", "answer"):
            detail = payload.get("text")
            if not isinstance(detail, str) or not detail.strip() or len(detail.encode()) > 4096:
                raise ValueError("text must be nonempty and at most 4096 bytes")
        if action == "report" or direct_done:
            detail = graph.evidence(task["done_when"], payload.get("text"))
        if action == "report" and not self.acceptors(task):
            raise ValueError("no independent acceptor; own steps finish with done and evidence")
        if action == "start":
            detail = ""
            self.db.execute("UPDATE tasks SET started_at=COALESCE(started_at,?) WHERE id=?", (at, task["id"]))
        if action == "delegate":
            owner = checked_binding(payload.get("owner"))
            # Self-assignment is takeover, not a new independent handoff. Preserve prior
            # oversight (including None on an own step); never erase a human delegator.
            delegator, detail = (task["delegator"] if owner == caller else caller), ""
        self.db.execute("UPDATE tasks SET state=?,owner=?,delegator=?,detail=?,revision=revision+1,updated_at=? WHERE id=? AND revision=?",
                        (destination, canonical(owner), canonical(delegator) if delegator else None, detail, at, task["id"], revision))
        self.db.execute("INSERT INTO transitions(task_id,state,at,caller) VALUES (?,?,?,?)", (task["id"], destination, at, canonical(caller)))
        if task["kind"] == "decision" and action in ("update", "depends"):
            self.decisions.cancel(task["id"])
        if decision_result:
            self.decisions.event(task["id"], "human", decision_result)
        return task["id"]

    def create_task(self, payload, caller, at, task_id=None, importing=False):
        title = payload.get("title")
        if not isinstance(title, str) or not title.strip() or len(title.encode()) > 1024:
            raise ValueError("title must be nonempty and at most 1024 bytes")
        kind = "decision" if payload["action"] == "decision" else payload.get("kind", "task")
        if kind not in ("task", "decision"):
            raise ValueError("invalid task kind")
        parent = payload.get("parent")
        if kind == "task" and parent is None and caller["class"] != "human_unverified" and not importing:
            raise ValueError("ordinary root creation requires human approval")
        self.parent_for_create(parent, caller, check_owner=not importing)
        values = graph.metadata(payload, required=True)
        task_id = task_id or "task_" + uuid.uuid4().hex
        owner = HUMAN_ROLE if kind == "decision" else caller
        self.db.execute("INSERT INTO tasks(id,parent,title,kind,owner,state,revision,created_at,updated_at,creator,detail) VALUES (?,?,?,?,?,'open',1,?,?,?,'')",
                        (task_id, parent, title.strip(), kind, canonical(owner), at, at, canonical(caller)))
        self.set_metadata(task_id, values)
        if decision_queue.FIELDS & payload.keys() and kind != "decision":
            raise ValueError("decision fields require kind decision")
        if kind == "decision":
            terms, due = self.decisions.initial(payload, at)
            self.db.execute("UPDATE tasks SET decision=?,decision_due_at=?,decision_timer_revision=? WHERE id=?", (canonical(terms) if terms else None, due, 1 if due else None, task_id))
        self.db.execute("INSERT INTO transitions(task_id,state,at,caller) VALUES (?,?,?,?)", (task_id, "open", at, canonical(caller)))
        if "depends_on" in payload and not importing:
            self.set_dependencies(task_id, payload["depends_on"])
        return task_id

    def invoke(self, params):
        if set(params) != {"caller", "payload"}:
            raise ValueError("invalid host action envelope")
        caller, payload = params["caller"], params["payload"]
        if not isinstance(payload, dict):
            raise ValueError("payload must be an object")
        action = payload.get("action")
        fields = {
            "list": {"action", "after"}, "ready": {"action", "after"}, "get": {"action", "id"},
            "whoami": {"action"}, "status": {"action"},
            "decision_log": {"action", "after"}, "decision_digest": {"action"},
            "decision_seen": {"action", "operation_id", "through", "expected_caller"},
            "op": {"action", "operation_id", "lookup_caller"},
            "create": {"action", "operation_id", "title", "parent", "kind", "expected_caller", "depends_on"} | graph.META_FIELDS | decision_queue.FIELDS,
            "decision": {"action", "operation_id", "title", "parent", "expected_caller", "depends_on"} | graph.META_FIELDS | decision_queue.FIELDS,
            "decision_timer": {"action", "operation_id", "id", "revision", "auto_at", "expected_caller"},
            "decision_revert": {"action", "operation_id", "id", "revision", "expected_caller"},
            "plan": {"action", "operation_id", "nodes", "parent", "revision", "expected_caller"},
            "depends": {"action", "operation_id", "id", "revision", "depends_on", "expected_caller"},
            "update": {"action", "operation_id", "id", "revision", "expected_caller"} | graph.META_FIELDS,
            **{action: {"action", "operation_id", "id", "revision", "expected_caller"} | ({"owner"} if action == "delegate" else {"option", "text"} if action == "answer" else {"text"} if action in ("block", "report", "done") else set())
               for action in ("delegate", "start", "block", "report", "accept", "done", "answer")},
        }
        if action not in fields or set(payload) - fields[action]:
            raise ValueError("unknown action or field (actor flags are not accepted)")
        if action == "decision_digest":
            return self.decisions.digest(caller)
        if action == "decision_log":
            return self.decisions.log(payload.get("after", 0))
        if action == "status":
            return {"revision": self.db.execute("SELECT COALESCE(MAX(seq),0) FROM transitions").fetchone()[0]}
        if action == "whoami":
            return {"caller": binding(caller) if caller.get("class") != "unknown" else {"class": "unknown"}}
        if action in ("list", "ready"):
            return self.list_tasks(payload.get("after"), ready=action == "ready")
        if action == "get":
            return {"task": self.task(payload["id"])}
        if action == "op":
            return self.lookup(payload, caller)
        if len(canonical(payload).encode()) > 64 * 1024:
            raise ValueError("plan/action exceeds 64 KiB request size")
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
            at = timestamp()
            if action == "decision_seen":
                self.quota({"operations": 1})
                result = self.decisions.acknowledge(owner, payload.get("through"))
            elif action == "plan":
                result = self.import_plan(payload, owner, at)
            else:
                tables = ("tasks", "operations", "transitions") if action in ("create", "decision") else ("operations", "transitions")
                counts = dict.fromkeys(tables, 1)
                if action in ("decision_revert", "decision_timer") or (action == "answer" and self.task(payload.get("id"))["decision"] is not None):
                    counts["decision_log"] = 1
                self.quota(counts)
                task_id = self.create_task(payload, owner, at) if action in ("create", "decision") else self.transition(payload, owner, at)
                result = {"task": self.task(task_id)}
                if action == "decision_revert":
                    result["affected_work"] = self.decisions.affected(task_id)
            outcome = {**result, "operation_id": op, "caller": owner}
            if len(canonical(outcome).encode()) > MAX_REPLY_BYTES:
                raise ValueError("operation outcome exceeds response budget")
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
    parser.add_argument("--decision-delay-seconds", type=int, default=900)
    args = parser.parse_args()
    directory = args.data_dir or str(Path(os.environ.get("XDG_STATE_HOME", str(Path.home() / ".local/state"))) / "vqro/extensions/vqro.tasks")
    store = Store(directory, args.decision_delay_seconds)
    emit({"type": "hello", "protocol": 2, "service_id": SERVICE, "requested_capabilities": ["action_invoke"], "methods": ["action.invoke.v1"]})
    pending = bytearray()
    def read_line(timeout):
        if b"\n" not in pending:
            if not select.select([sys.stdin.fileno()], [], [], timeout)[0]:
                return None
            block = os.read(sys.stdin.fileno(), 65536)
            if not block:
                raise EOFError("provider input closed")
            pending.extend(block)
            if len(pending) > 262144:
                raise ValueError("provider input frame exceeds bound")
        if b"\n" not in pending:
            return None
        line, _, rest = pending.partition(b"\n")
        pending[:] = rest
        return line
    welcome_line = None
    while welcome_line is None:
        welcome_line = read_line(None)
    welcome = json.loads(welcome_line)
    if welcome.get("type") != "welcome" or welcome.get("granted_capabilities") != ["action_invoke"] or "action.invoke.v1" not in welcome.get("methods", []):
        raise ValueError("action capability not granted")
    while True:
        applied = 0
        try:
            applied = store.decisions.apply_due(timestamp())
        except OutcomeUnknown:
            raise  # Never retry an uncertain internal commit in this process.
        except (ValueError, sqlite3.Error) as error:
            # Keep read-only inspection usable when a bounded policy tick cannot write.
            print("decision policy delayed: " + str(error), file=sys.stderr)
        try:
            line = read_line(store.decisions.next_delay(timestamp(), applied))
        except EOFError:
            return
        if line is None:
            continue
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
