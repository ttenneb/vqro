"""Durable addressed messages owned by the tasks extension.

Messages are a second record family in the same store. The provider never sees
agent names: a recipient is an exact agent-process binding supplied by the caller
(the CLI resolves it with `agent get`), and only the host-attributed caller whose
full normalized terminal/agent/execution binding matches can list or claim.
Sender-owned history remains visible independently of the recipient. Sender
provenance is the host caller binding.
"""
import uuid

ACTIVATION_OFF_LABEL = "Automatic message delivery is off"
ACTIVATION_OFF_DETAIL = "Waiting messages will not start agent turns. Enable in a human shell: vqro message activation on"

MAX_BODY_BYTES = 32 * 1024
MAX_PAGE = 32
# Bounds count encoded record JSON (the real result bytes), not just bodies. A
# shared serialized budget covers waiting+history; a large UTF-8 body can
# canonicalize to roughly twice its raw bytes, so the budget stays well under the
# provider's 128 KiB cap.
MAX_LIST_BYTES = 96 * 1024
MAX_CLAIM = 8
MAX_CLAIM_BYTES = 96 * 1024
STATES = ("waiting", "claimed")
MAX_ATTEMPTS = 32
ATTEMPT_OUTCOMES = ("no_effect", "submitted", "uncertain", "context_prepared")
# Shared by bounded CLI history and complete, body-free document evidence.
DELIVERY_ATTENTION_SQL = """(state='claimed' AND resolved_at IS NULL AND NOT EXISTS
    (SELECT 1 FROM message_attempts a WHERE a.rowid=
     (SELECT MAX(b.rowid) FROM message_attempts b WHERE b.message_id=messages.id)
     AND a.outcome='submitted'))"""


def _bounded(canonical, records, budget):
    kept, total = [], 0
    for record in records:
        size = len(canonical(record).encode())
        if total + size > budget:
            break
        total += size
        kept.append(record)
    return kept


def _text(value, name, limit=MAX_BODY_BYTES):
    if not isinstance(value, str) or not value or len(value.encode()) > limit:
        raise ValueError(f"{name} must be 1..{limit} bytes")
    return value


def _agent_binding(value):
    if not isinstance(value, dict) or value.get("class") != "agent":
        raise ValueError("recipient must be an exact agent-process binding")
    keys = {"class", "terminal_id", "agent", "execution"}
    if set(value) != keys:
        raise ValueError("recipient binding must be an exact agent-process binding")
    for key in keys - {"class"}:
        if not isinstance(value[key], str) or not 1 <= len(value[key].encode()) <= 256:
            raise ValueError("invalid recipient binding field")
    return value


class Messages:
    def __init__(self, store):
        self.store = store

    @property
    def db(self):
        return self.store.db

    def create_schema(self):
        # No executescript: it would implicitly commit the caller's open migration
        # transaction. Individual statements keep the migration atomic.
        self.db.execute("""CREATE TABLE IF NOT EXISTS messages (
                id TEXT PRIMARY KEY, sender TEXT NOT NULL, recipient TEXT NOT NULL,
                recipient_execution TEXT NOT NULL, body TEXT NOT NULL, reply_to TEXT,
                state TEXT NOT NULL, revision INTEGER NOT NULL,
                created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
                claimed_at TEXT)""")
        columns = {row["name"] for row in self.db.execute("PRAGMA table_info(messages)")}
        if "resolved_at" not in columns:
            self.db.execute("ALTER TABLE messages ADD COLUMN resolved_at TEXT")
        self.db.execute("""CREATE TABLE IF NOT EXISTS message_attempts (
                id TEXT PRIMARY KEY, message_id TEXT NOT NULL REFERENCES messages(id),
                outcome TEXT NOT NULL, created_at TEXT NOT NULL, updated_at TEXT NOT NULL)""")
        self.db.execute("CREATE INDEX IF NOT EXISTS message_attempt_history ON message_attempts(message_id, created_at, id)")
        self.db.execute("""CREATE TABLE IF NOT EXISTS message_recoveries (
                message_id TEXT NOT NULL REFERENCES messages(id), revision INTEGER NOT NULL,
                mode TEXT NOT NULL, caller TEXT NOT NULL, at TEXT NOT NULL,
                PRIMARY KEY(message_id, revision))""")
        self.db.execute("CREATE INDEX IF NOT EXISTS message_recipient_execution ON messages(recipient_execution, state)")
        self.db.execute("CREATE INDEX IF NOT EXISTS message_sender ON messages(sender, created_at)")
        self.db.execute("""CREATE TABLE IF NOT EXISTS message_policy (
                id INTEGER PRIMARY KEY CHECK (id = 1), activation INTEGER NOT NULL,
                updated_at TEXT NOT NULL, updated_by TEXT NOT NULL)""")
        if self.db.execute("SELECT 1 FROM message_policy WHERE id=1").fetchone() is None:
            # A fresh Tasks store starts with delivery activation on. The row is
            # inserted once, so an existing store keeps whatever recorded value
            # it already has; only a human may change it afterwards.
            self.db.execute(
                "INSERT INTO message_policy(id,activation,updated_at,updated_by) VALUES (1,1,?,?)",
                (self.store.initialized_at, "{}"),
            )

    def activation(self):
        return bool(self.db.execute("SELECT activation FROM message_policy WHERE id=1").fetchone()[0])

    def _attempt(self, message_id):
        row = self.db.execute("SELECT * FROM message_attempts WHERE message_id=? ORDER BY rowid DESC LIMIT 1", (message_id,)).fetchone()
        return dict(row) if row is not None else None

    def _record(self, row):
        attempt = self._attempt(row["id"])
        return {
            "id": row["id"], "sender": row["sender"], "recipient": row["recipient"],
            "recipient_execution": row["recipient_execution"],
            "body": row["body"], "reply_to": row["reply_to"], "state": row["state"],
            "revision": row["revision"], "created_at": row["created_at"], "updated_at": row["updated_at"],
            "claimed_at": row["claimed_at"], "attempt": attempt,
            "resolved_at": row["resolved_at"],
            # No attempt is the legacy claimed crash cut. Pending is deliberately
            # uncertain after reopen: neither age nor a new process proves no effect.
            "attention": row["state"] == "claimed" and row["resolved_at"] is None
                and (attempt is None or attempt["outcome"] != "submitted"),
        }

    def read(self, payload, caller):
        binding = self.store.binding(caller)
        key = self.store.canonical(binding)
        if payload["action"] == "message_get":
            row = self._owned(payload["id"], key)
            return {"message": self._record(row), "attempts": [dict(item) for item in self.db.execute(
                "SELECT * FROM message_attempts WHERE message_id=? ORDER BY rowid", (row["id"],))],
                # Actor roles reference the immutable full bindings in this same
                # record; avoid repeating large escaped bindings 32 times.
                "recoveries": [{"revision": item["revision"], "mode": item["mode"], "at": item["at"],
                                "caller_role": "sender" if item["caller"] == row["sender"] else "recipient"}
                               for item in self.db.execute(
                    "SELECT * FROM message_recoveries WHERE message_id=? ORDER BY revision", (row["id"],))]}
        waiting_rows = []
        clause, params = "sender=?", [key]
        if binding.get("class") == "agent":
            clause, params = "(sender=? OR (recipient_execution=? AND recipient=?))", [key, binding["execution"], key]
            waiting_rows = self.db.execute(
                "SELECT * FROM messages WHERE recipient_execution=? AND recipient=? AND state='waiting' ORDER BY created_at ASC, id ASC LIMIT ?",
                (binding["execution"], key, MAX_PAGE),
            ).fetchall()
        history_rows = self.db.execute(
            f"SELECT * FROM messages WHERE {clause} ORDER BY {DELIVERY_ATTENTION_SQL} DESC, created_at DESC, id DESC LIMIT ?",
            (*params, MAX_PAGE),
        ).fetchall()
        waiting_ids = {row["id"] for row in waiting_rows}
        # Waiting pickup first, then history without duplicating it, under one
        # shared serialized budget the whole result must fit.
        selected = [self._record(row) for row in waiting_rows]
        selected += [self._record(row) for row in history_rows if row["id"] not in waiting_ids]
        kept = _bounded(self.store.canonical, selected, MAX_LIST_BYTES)
        waiting = [record for record in kept if record["id"] in waiting_ids]
        messages = [record for record in kept if record["id"] not in waiting_ids]
        return {"activation": self.activation(), "waiting": waiting, "messages": messages}

    def _owned(self, message_id, key):
        row = self.db.execute("SELECT * FROM messages WHERE id=?", (message_id,)).fetchone()
        if row is None or key not in (row["sender"], row["recipient"]):
            raise ValueError("message unavailable to this exact sender/recipient binding")
        return row

    def mutate(self, payload, caller, at):
        action = payload["action"]
        binding = self.store.binding(caller)
        key = self.store.canonical(binding)
        if action == "message_send":
            recipient = _agent_binding(payload["recipient"])
            body = _text(payload["body"], "body")
            reply_to = payload.get("reply_to")
            if reply_to is not None:
                if not isinstance(reply_to, str) or self.db.execute("SELECT 1 FROM messages WHERE id=?", (reply_to,)).fetchone() is None:
                    raise ValueError("reply_to must reference an existing message")
            message_id = "msg_" + uuid.uuid4().hex
            self.db.execute(
                "INSERT INTO messages(id,sender,recipient,recipient_execution,body,reply_to,state,revision,created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?,?)",
                (message_id, key, self.store.canonical(recipient), recipient["execution"], body, reply_to, "waiting", 1, at, at),
            )
            row = self.db.execute("SELECT * FROM messages WHERE id=?", (message_id,)).fetchone()
            return {"message": self._record(row)}
        if action == "message_edit":
            row = self.db.execute("SELECT * FROM messages WHERE id=?", (payload["id"],)).fetchone()
            if row is None:
                raise ValueError("unknown message")
            if row["sender"] != key:
                raise ValueError("only the original sender may edit a message")
            if row["state"] != "waiting" or row["claimed_at"] is not None:
                raise ValueError("a previously claimed message can no longer be edited")
            if type(payload["revision"]) is not int or payload["revision"] != row["revision"]:
                raise ValueError("stale message revision")
            body = _text(payload["body"], "body")
            self.db.execute(
                "UPDATE messages SET body=?,revision=revision+1,updated_at=? WHERE id=? AND revision=?",
                (body, at, row["id"], row["revision"]),
            )
            updated = self.db.execute("SELECT * FROM messages WHERE id=?", (row["id"],)).fetchone()
            return {"message": self._record(updated)}
        if action == "message_claim":
            if binding.get("class") != "agent":
                raise ValueError("only the addressed agent process can claim messages")
            # An idle activation wake is gated atomicly on the current policy at
            # claim time; a turn already starting is a different pickup path.
            if payload.get("idle") and not self.activation():
                return {"claimed": [], "activation": False}
            rows = self.db.execute(
                "SELECT * FROM messages WHERE recipient_execution=? AND recipient=? AND state='waiting' ORDER BY created_at ASC, id ASC LIMIT ?",
                (binding["execution"], key, MAX_CLAIM),
            ).fetchall()
            records = _bounded(self.store.canonical, [self._record(row) for row in rows], MAX_CLAIM_BYTES)
            claimed = []
            for record in records:
                self.db.execute(
                    "UPDATE messages SET state='claimed',claimed_at=COALESCE(claimed_at,?),revision=revision+1,updated_at=? WHERE id=? AND state='waiting'",
                    (at, at, record["id"]),
                )
                # Same transaction as claim and operation receipt: no second
                # claim-to-attempt commit gap, including lost claim acknowledgements.
                self.db.execute("INSERT INTO message_attempts VALUES (?,?,?,?,?)",
                                ("attempt_" + uuid.uuid4().hex, record["id"], "pending", at, at))
                claimed.append(self._record(self.db.execute("SELECT * FROM messages WHERE id=?", (record["id"],)).fetchone()))
            return {"claimed": claimed, "activation": self.activation()}
        if action == "message_outcomes":
            attempts = payload.get("attempts")
            if not isinstance(attempts, list) or not 1 <= len(attempts) <= MAX_CLAIM:
                raise ValueError("outcomes requires 1..8 exact message/attempt pairs")
            if any(not isinstance(item, dict) or set(item) != {"id", "attempt"}
                   or any(not isinstance(value, str) or not 1 <= len(value) <= 128 for value in item.values())
                   for item in attempts):
                raise ValueError("invalid outcome message/attempt pair")
            if len({item["id"] for item in attempts}) != len(attempts):
                raise ValueError("duplicate outcome message")
            outcomes = []
            for item in attempts:
                result = self.mutate({"action": "message_outcome", **item, "outcome": payload["outcome"]}, caller, at)["message"]
                outcomes.append({"id": result["id"], "attempt": result["attempt"]})
            return {"outcomes": outcomes}
        if action == "message_outcome":
            row = self._owned(payload["id"], key)
            if row["recipient"] != key:
                raise ValueError("only the exact recipient can report an adapter outcome")
            attempt = self._attempt(row["id"])
            if row["state"] != "claimed" or row["resolved_at"] is not None or attempt is None or attempt["id"] != payload["attempt"]:
                raise ValueError("stale delivery attempt")
            outcome = payload["outcome"]
            if (outcome not in ATTEMPT_OUTCOMES or attempt["outcome"] not in ("pending", "context_prepared")
                    or (attempt["outcome"] == "context_prepared" and outcome != "no_effect")):
                raise ValueError("invalid delivery outcome transition")
            self.db.execute("UPDATE message_attempts SET outcome=?,updated_at=? WHERE id=?", (outcome, at, attempt["id"]))
            self.db.execute("UPDATE messages SET revision=revision+1,updated_at=? WHERE id=?", (at, row["id"]))
            return {"message": self._record(self.db.execute("SELECT * FROM messages WHERE id=?", (row["id"],)).fetchone())}
        if action == "message_recover":
            row = self._owned(payload["id"], key)
            if type(payload["revision"]) is not int or payload["revision"] != row["revision"]:
                raise ValueError("stale message revision; inspect the original record")
            if row["state"] != "claimed" or row["resolved_at"] is not None:
                raise ValueError("only an unresolved original claim can be recovered")
            mode = payload["mode"]
            if mode == "retry":
                attempt = self._attempt(row["id"])
                if (attempt is None or attempt["outcome"] != "no_effect") and payload.get("may_duplicate") is not True:
                    raise ValueError("delivery may already have happened; explicit --may-duplicate required")
                if self.db.execute("SELECT COUNT(*) FROM message_attempts WHERE message_id=?", (row["id"],)).fetchone()[0] >= MAX_ATTEMPTS:
                    raise ValueError("message attempt limit reached; resolve the original record")
                # Original body, sender, recipient, execution, id and history stay
                # unchanged. There is intentionally no target parameter.
                self.db.execute("UPDATE messages SET state='waiting',revision=revision+1,updated_at=? WHERE id=?", (at, row["id"]))
            elif mode == "resolve":
                self.db.execute("UPDATE messages SET resolved_at=?,revision=revision+1,updated_at=? WHERE id=?", (at, at, row["id"]))
            else:
                raise ValueError("recovery mode must be retry or resolve")
            self.db.execute("INSERT INTO message_recoveries VALUES (?,?,?,?,?)", (row["id"], row["revision"], mode, key, at))
            return {"message": self._record(self.db.execute("SELECT * FROM messages WHERE id=?", (row["id"],)).fetchone())}
        if action == "message_activation":
            if binding.get("class") != "human_unverified":
                raise ValueError("only a human may change the activation setting")
            value = payload["activation"]
            if type(value) is not bool:
                raise ValueError("activation must be true or false")
            self.db.execute("UPDATE message_policy SET activation=?,updated_at=?,updated_by=? WHERE id=1", (1 if value else 0, at, key))
            return {"activation": value}
        raise ValueError("unknown message action")
