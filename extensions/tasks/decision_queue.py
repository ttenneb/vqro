"""Tasks-owned structured choices and deterministic record-only deadline policy."""
import datetime as dt
import json

FIELDS = {"options", "default", "reversible", "waiting_cost", "auto_at"}
ACTIVE = {"open", "in_progress", "blocked"}
FINAL = {"reported", "accepted", "done"}


def time_value(value):
    if not isinstance(value, str) or len(value) > 64:
        raise ValueError("auto_at must be an ISO-8601 time with timezone")
    try:
        stamp = dt.datetime.fromisoformat(value.replace("Z", "+00:00"))
        if stamp.tzinfo is None:
            raise ValueError("timezone missing")
        return stamp.astimezone(dt.timezone.utc).isoformat(timespec="milliseconds")
    except (ValueError, OverflowError) as error:
        raise ValueError("auto_at must be an ISO-8601 time with timezone") from error


def text(value, maximum, name):
    if not isinstance(value, str) or not value.strip() or len(value.encode()) > maximum or any(not c.isprintable() for c in value):
        raise ValueError(f"{name} must be nonempty printable text, at most {maximum} bytes")
    return value


class Queue:
    def __init__(self, store, delay_seconds, unknown_error):
        if type(delay_seconds) is not int or not 1 <= delay_seconds <= 86400:
            raise ValueError("decision delay floor must be 1-86400 seconds")
        self.store = store
        self.delay_seconds = delay_seconds
        self.unknown_error = unknown_error

    @property
    def db(self):
        return self.store.db

    def migrate(self, columns):
        for name, kind in (("decision", "TEXT"), ("decision_due_at", "TEXT"), ("decision_timer_revision", "INTEGER")):
            if name not in columns:
                self.db.execute(f"ALTER TABLE tasks ADD COLUMN {name} {kind}")
        self.db.execute("CREATE INDEX IF NOT EXISTS decision_due ON tasks(decision_due_at) WHERE kind='decision' AND state='open' AND decision_due_at IS NOT NULL")
        self.db.execute("CREATE TABLE IF NOT EXISTS decision_log (seq INTEGER PRIMARY KEY REFERENCES transitions(seq), task_id TEXT NOT NULL REFERENCES tasks(id), kind TEXT NOT NULL, result TEXT NOT NULL)")
        self.db.execute("CREATE TABLE IF NOT EXISTS decision_seen (uid INTEGER PRIMARY KEY, seq INTEGER NOT NULL)")

    def deadline(self, value, at):
        if value is None:
            return None
        value = time_value(value)
        minimum = dt.datetime.fromisoformat(at) + dt.timedelta(seconds=self.delay_seconds)
        if dt.datetime.fromisoformat(value) < minimum:
            raise ValueError(f"auto_at must allow the configured {self.delay_seconds}s human window")
        return value

    def initial(self, payload, at):
        if not (FIELDS - {"auto_at"}) <= payload.keys():
            raise ValueError("structured decisions require options, default, reversible and waiting_cost")
        options = payload["options"]
        if not isinstance(options, list) or not 2 <= len(options) <= 4:
            raise ValueError("options must contain 2-4 key/label choices")
        keys = []
        for option in options:
            if not isinstance(option, dict) or set(option) != {"key", "label"}:
                raise ValueError("each option needs exactly key and label")
            key = text(option["key"], 32, "option key")
            if not all(c.isascii() and (c.isalnum() or c in "_-.") for c in key):
                raise ValueError("option key must be an ASCII token")
            text(option["label"], 256, "option label")
            keys.append(key)
        if len(set(keys)) != len(keys) or payload["default"] not in keys:
            raise ValueError("default must name one distinct listed option")
        if type(payload["reversible"]) is not bool:
            raise ValueError("reversible must be boolean")
        cost = text(payload["waiting_cost"], 512, "waiting_cost")
        due = self.deadline(payload.get("auto_at"), at)
        if due and not payload["reversible"]:
            raise ValueError("irreversible decisions cannot auto-apply")
        return {"options": options, "default": payload["default"], "reversible": payload["reversible"], "waiting_cost": cost, "resolution": None}, due

    def describe(self, task):
        value = json.loads(task["decision"]) if task["decision"] else None
        if value is not None:
            value.update(auto_at=task["decision_due_at"], timer_current=bool(task["decision_due_at"] and task["decision_timer_revision"] == task["revision"]))
        return value

    def cancel(self, task_id):
        self.db.execute("UPDATE tasks SET decision_due_at=NULL,decision_timer_revision=NULL WHERE id=?", (task_id,))

    def event(self, task_id, kind, result):
        seq = self.db.execute("SELECT MAX(seq) FROM transitions WHERE task_id=?", (task_id,)).fetchone()[0]
        self.db.execute("INSERT INTO decision_log VALUES (?,?,?,?)", (seq, task_id, kind, json.dumps(result, sort_keys=True)))

    def choice(self, task, payload, at, by):
        terms = task["decision"]
        key = payload.get("option", payload.get("text"))
        if key not in [option["key"] for option in terms["options"]]:
            raise ValueError("answer must name one listed option")
        note = payload.get("note", payload.get("text", "") if "option" in payload else "")
        if not isinstance(note, str) or len(note.encode()) > 4096:
            raise ValueError("answer note must be at most 4096 bytes")
        result = {"by": by, "option": key, "note": note, "applied_at": at, "filer": task["creator"], "default": terms["default"], "deadline": terms["auto_at"]}
        if by == "policy":
            result["operation_id"] = "decision-auto:" + task["id"] + ":" + str(task["revision"])
            result["policy_caller"] = {"class": "decision_policy", "decision_id": task["id"], "revision": task["revision"]}
            result["late_ms"] = max(0, int((dt.datetime.fromisoformat(at) - dt.datetime.fromisoformat(terms["auto_at"])).total_seconds() * 1000))
        terms = {k: v for k, v in terms.items() if k not in ("auto_at", "timer_current")}
        terms["resolution"] = result
        self.db.execute("UPDATE tasks SET decision=? WHERE id=?", (json.dumps(terms, sort_keys=True), task["id"]))
        self.cancel(task["id"])
        # Keep the task's bounded summary scalar; the full note lives in resolution/log.
        return key, result

    def change(self, task, payload, caller, at):
        action = payload["action"]
        terms = task["decision"]
        if task["kind"] != "decision" or not terms:
            raise ValueError("structured decision required")
        if action == "decision_timer":
            if task["state"] != "open" or (caller != task["creator"] and caller["class"] != "human_unverified"):
                raise ValueError("only original filer or cooperative human may schedule an open decision")
            due = self.deadline(payload.get("auto_at"), at)
            if due and not terms["reversible"]:
                raise ValueError("irreversible decisions cannot auto-apply")
            self.db.execute("UPDATE tasks SET decision_due_at=?,decision_timer_revision=? WHERE id=?", (due, task["revision"] + 1 if due else None, task["id"]))
            return "open", ""
        resolution = terms.get("resolution")
        if caller["class"] != "human_unverified" or task["state"] != "done" or not terms["reversible"] or not resolution or resolution["by"] != "policy":
            raise ValueError("only a cooperative human may revert a reversible policy answer")
        blockers = self.db.execute("SELECT id FROM tasks WHERE (id=? OR id IN (SELECT task_id FROM dependencies WHERE prerequisite=?)) AND state IN ('reported','accepted','done') ORDER BY id", (task["parent"], task["id"])).fetchall()
        if blockers:
            raise ValueError("revert blocked by completed/reported work: " + ",".join(row[0] for row in blockers[:32]) + (" (more blockers)" if len(blockers) > 32 else ""))
        # Existing active dependents are not undone; their report/done readiness gate remains.
        terms = {k: v for k, v in terms.items() if k not in ("auto_at", "timer_current")}
        terms["resolution"] = None
        self.db.execute("UPDATE tasks SET decision=? WHERE id=?", (json.dumps(terms, sort_keys=True), task["id"]))
        self.cancel(task["id"])
        return "open", ""

    def affected(self, task_id):
        rows = self.db.execute("SELECT id FROM tasks WHERE id IN (SELECT task_id FROM dependencies WHERE prerequisite=?) AND state='in_progress' ORDER BY id", (task_id,)).fetchall()
        return {"in_progress": [row[0] for row in rows[:32]], "count": len(rows), "warning": "Existing work is not undone; report/done waits for re-answer."}

    def apply_due(self, at, limit=8):
        # Deadline and request writes share this connection/event loop. Recheck inside TX
        # because another provider connection can have answered since the candidate scan.
        rows = self.db.execute("SELECT id FROM tasks WHERE kind='decision' AND state='open' AND decision_due_at<=? AND decision_timer_revision=revision AND NOT EXISTS (SELECT 1 FROM dependencies JOIN tasks p ON p.id=prerequisite WHERE task_id=tasks.id AND p.state NOT IN ('accepted','done')) ORDER BY decision_due_at,id LIMIT ?", (at, limit)).fetchall()
        applied = 0
        for row in rows:
            self.db.execute("BEGIN IMMEDIATE")
            try:
                task = self.store.task(row[0])
                terms = task["decision"]
                if task["state"] != "open" or not terms or not terms["reversible"] or not terms["timer_current"] or not terms["auto_at"] or terms["auto_at"] > at or task["waiting_on"]:
                    self.store.commit()
                    continue
                # Defensive parity with manual completion for legacy/corrupt records;
                # current graph admission forbids children of decisions.
                if self.db.execute("SELECT 1 FROM tasks WHERE parent=? AND state NOT IN ('accepted','done') LIMIT 1", (task["id"],)).fetchone():
                    self.store.commit()
                    continue
                self.store.quota({"operations": 1, "transitions": 1, "decision_log": 1})
                policy = {"class": "decision_policy", "decision_id": task["id"], "revision": task["revision"]}
                op = "decision-auto:" + task["id"] + ":" + str(task["revision"])
                detail, event = self.choice(task, {"option": terms["default"]}, at, "policy")
                self.db.execute("UPDATE tasks SET state='done',detail=?,revision=revision+1,updated_at=? WHERE id=? AND revision=?", (detail, at, task["id"], task["revision"]))
                caller = json.dumps(policy, sort_keys=True, separators=(",", ":"))
                self.db.execute("INSERT INTO transitions(task_id,state,at,caller) VALUES (?,'done',?,?)", (task["id"], at, caller))
                self.event(task["id"], "auto", event)
                outcome = {"task": self.store.task(task["id"]), "operation_id": op, "caller": policy}
                self.db.execute("INSERT INTO operations VALUES (?,?,?,?,?)", (caller, op, "recorded-decision-default-v1", json.dumps(outcome, sort_keys=True, separators=(",", ":")), at))
                self.store.commit()
                applied += 1
            except self.unknown_error:
                raise
            except Exception:
                self.store.rollback()
                raise
        return applied

    def next_delay(self, at, applied=0):
        row = self.db.execute("SELECT MIN(decision_due_at) FROM tasks WHERE kind='decision' AND state='open' AND decision_timer_revision=revision").fetchone()[0]
        if row is None:
            return None
        delay = (dt.datetime.fromisoformat(row) - dt.datetime.fromisoformat(at)).total_seconds()
        if delay <= 0 and applied < 8:
            # Deferred prerequisites/quota must not turn idle time into a 1Hz loop.
            # Requests wake select immediately; still honor an upcoming future deadline.
            future = self.db.execute("SELECT MIN(decision_due_at) FROM tasks WHERE kind='decision' AND state='open' AND decision_timer_revision=revision AND decision_due_at>?", (at,)).fetchone()[0]
            return 30 if future is None else max(1, min(30, (dt.datetime.fromisoformat(future) - dt.datetime.fromisoformat(at)).total_seconds()))
        return max(1, min(30, delay))

    def log(self, after=0):
        if type(after) is not int or after < 0:
            raise ValueError("decision log cursor must be a nonnegative sequence")
        rows = self.db.execute("SELECT * FROM decision_log WHERE seq>? ORDER BY seq LIMIT 33", (after,)).fetchall()
        events, used = [], 4096
        for row in rows[:32]:
            event = {"seq": row["seq"], "id": row["task_id"], "kind": row["kind"], "result": json.loads(row["result"])}
            size = len(json.dumps(event).encode()) + 1
            if used + size > 128 * 1024:
                break
            events.append(event)
            used += size
        if rows and not events:
            raise ValueError("decision log entry exceeds reply bound")
        return {"events": events, "next_cursor": events[-1]["seq"] if len(rows) > len(events) else None}

    def acknowledge(self, caller, through):
        if caller.get("class") != "human_unverified":
            raise ValueError("human-only explicit last-look acknowledgement")
        current = self.db.execute("SELECT COALESCE(MAX(seq),0) FROM transitions").fetchone()[0]
        previous = self.db.execute("SELECT seq FROM decision_seen WHERE uid=?", (caller["uid"],)).fetchone()
        if type(through) is not int or through < 0 or through > current or (previous and through < previous[0]):
            raise ValueError("last-look sequence cannot be future or regress")
        if previous is None:
            self.store.quota({"decision_seen": 1})
        self.db.execute("INSERT INTO decision_seen VALUES (?,?) ON CONFLICT(uid) DO UPDATE SET seq=excluded.seq", (caller["uid"], through))
        return {"uid": caller["uid"], "seen_seq": through}

    def digest(self, caller):
        # One read snapshot; policy writes run outside this handler. Polling never ACKs.
        self.db.execute("BEGIN")
        try:
            sequence = self.db.execute("SELECT COALESCE(MAX(seq),0) FROM transitions").fetchone()[0]
            uid = caller.get("uid") if caller.get("class") == "human_unverified" else None
            looked = self.db.execute("SELECT seq FROM decision_seen WHERE uid=?", (uid,)).fetchone() if uid is not None else None
            seen = looked[0] if looked else 0
            result = {"sequence": sequence, "seen_seq": seen, "last_look_known": uid is not None,
                      "needs_you_now": [], "auto_since_look": [], "more": {},
                      "counts": {kind: self.db.execute("SELECT COUNT(DISTINCT task_id) FROM decision_log WHERE kind=?", (kind,)).fetchone()[0] for kind in ("auto", "revert")}}
            rows = self.db.execute("SELECT id FROM tasks WHERE kind='decision' AND state='open' ORDER BY created_at,id LIMIT 17").fetchall()
            used = 4096
            for row in rows[:16]:
                task = self.store.task(row[0])
                size = len(json.dumps(task).encode()) + 1
                if used + size > 32768:
                    break
                result["needs_you_now"].append(task); used += size
            result["more"]["needs_you_now"] = len(rows) > len(result["needs_you_now"])
            rows = self.db.execute("SELECT * FROM decision_log WHERE seq>? AND seq<=? AND kind='auto' ORDER BY seq LIMIT 17", (seen, sequence)).fetchall()
            used = 4096
            for row in rows[:16]:
                event = {"seq": row["seq"], "id": row["task_id"], "result": json.loads(row["result"])}
                size = len(json.dumps(event).encode()) + 1
                if used + size > 32768:
                    break
                result["auto_since_look"].append(event); used += size
            result["more"]["auto_since_look"] = len(rows) > len(result["auto_since_look"])
            # A suggested ACK never hides unseen remainder of a truncated auto log.
            result["ack_through"] = result["auto_since_look"][-1]["seq"] if result["more"]["auto_since_look"] else sequence
            self.store.commit()
            return result
        except self.unknown_error:
            raise
        except Exception:
            self.store.rollback()
            raise
