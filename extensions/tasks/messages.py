"""Durable addressed messages owned by the tasks extension.

Messages are a second record family in the same store. The provider never sees
agent names: a recipient is an exact agent-process binding supplied by the caller
(the CLI resolves it with `agent get`), and only the host-attributed caller whose
execution matches can claim. Sender provenance is the host caller binding.
"""
import uuid

MAX_BODY_BYTES = 32 * 1024
MAX_PAGE = 32
STATES = ("waiting", "claimed")


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
                sender_address TEXT, state TEXT NOT NULL, revision INTEGER NOT NULL,
                created_at TEXT NOT NULL, updated_at TEXT NOT NULL,
                claimed_at TEXT, claimed_by TEXT)""")
        self.db.execute("CREATE INDEX IF NOT EXISTS message_recipient_execution ON messages(recipient_execution, state)")
        self.db.execute("CREATE INDEX IF NOT EXISTS message_sender ON messages(sender, created_at)")
        self.db.execute("""CREATE TABLE IF NOT EXISTS message_policy (
                id INTEGER PRIMARY KEY CHECK (id = 1), activation INTEGER NOT NULL,
                updated_at TEXT NOT NULL, updated_by TEXT NOT NULL)""")
        self.db.execute("""CREATE TABLE IF NOT EXISTS message_policy_log (
                seq INTEGER PRIMARY KEY, activation INTEGER NOT NULL,
                at TEXT NOT NULL, caller TEXT NOT NULL)""")
        if self.db.execute("SELECT 1 FROM message_policy WHERE id=1").fetchone() is None:
            self.db.execute(
                "INSERT INTO message_policy(id,activation,updated_at,updated_by) VALUES (1,0,?,?)",
                (self.store.initialized_at, "{}"),
            )

    def activation(self):
        return bool(self.db.execute("SELECT activation FROM message_policy WHERE id=1").fetchone()[0])

    def _record(self, row):
        return {
            "id": row["id"], "sender": row["sender"], "recipient": row["recipient"],
            "recipient_execution": row["recipient_execution"], "sender_address": row["sender_address"],
            "body": row["body"], "reply_to": row["reply_to"], "state": row["state"],
            "revision": row["revision"], "created_at": row["created_at"], "updated_at": row["updated_at"],
            "claimed_at": row["claimed_at"],
        }

    def read(self, payload, caller):
        binding = self.store.binding(caller)
        key = self.store.canonical(binding)
        clause, params = "sender=?", [key]
        if binding.get("class") == "agent":
            clause, params = "(sender=? OR recipient_execution=?)", [key, binding["execution"]]
        rows = self.db.execute(
            f"SELECT * FROM messages WHERE {clause} ORDER BY created_at DESC, id DESC LIMIT ?",
            (*params, MAX_PAGE),
        ).fetchall()
        return {"activation": self.activation(), "messages": [self._record(row) for row in rows]}

    def mutate(self, payload, caller, at):
        action = payload["action"]
        binding = self.store.binding(caller)
        key = self.store.canonical(binding)
        if action == "message_send":
            recipient = _agent_binding(payload["recipient"])
            body = _text(payload["body"], "body")
            sender_address = payload.get("sender_address")
            if sender_address is not None and (not isinstance(sender_address, str) or not 1 <= len(sender_address.encode()) <= 256):
                raise ValueError("sender_address must be 1..256 bytes")
            reply_to = payload.get("reply_to")
            if reply_to is not None:
                if not isinstance(reply_to, str) or self.db.execute("SELECT 1 FROM messages WHERE id=?", (reply_to,)).fetchone() is None:
                    raise ValueError("reply_to must reference an existing message")
            message_id = "msg_" + uuid.uuid4().hex
            self.db.execute(
                "INSERT INTO messages(id,sender,recipient,recipient_execution,body,reply_to,sender_address,state,revision,created_at,updated_at) VALUES (?,?,?,?,?,?,?,?,?,?,?)",
                (message_id, key, self.store.canonical(recipient), recipient["execution"], body, reply_to, sender_address, "waiting", 1, at, at),
            )
            row = self.db.execute("SELECT * FROM messages WHERE id=?", (message_id,)).fetchone()
            return {"message": self._record(row)}
        if action == "message_edit":
            row = self.db.execute("SELECT * FROM messages WHERE id=?", (payload["id"],)).fetchone()
            if row is None:
                raise ValueError("unknown message")
            if row["sender"] != key:
                raise ValueError("only the original sender may edit a message")
            if row["state"] != "waiting":
                raise ValueError("a claimed message can no longer be edited")
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
            rows = self.db.execute(
                "SELECT * FROM messages WHERE recipient_execution=? AND state='waiting' ORDER BY created_at ASC, id ASC",
                (binding["execution"],),
            ).fetchall()
            claimed = []
            for row in rows:
                self.db.execute(
                    "UPDATE messages SET state='claimed',claimed_at=?,claimed_by=?,revision=revision+1,updated_at=? WHERE id=? AND state='waiting'",
                    (at, key, at, row["id"]),
                )
                claimed.append(self._record(self.db.execute("SELECT * FROM messages WHERE id=?", (row["id"],)).fetchone()))
            return {"claimed": claimed, "activation": self.activation()}
        if action == "message_activation":
            if binding.get("class") != "human_unverified":
                raise ValueError("only a human may change the activation setting")
            value = payload["activation"]
            if type(value) is not bool:
                raise ValueError("activation must be true or false")
            self.db.execute("UPDATE message_policy SET activation=?,updated_at=?,updated_by=? WHERE id=1", (1 if value else 0, at, key))
            seq = self.db.execute("SELECT COALESCE(MAX(seq),0)+1 FROM message_policy_log").fetchone()[0]
            self.db.execute("INSERT INTO message_policy_log VALUES (?,?,?,?)", (seq, 1 if value else 0, at, key))
            return {"activation": value}
        raise ValueError("unknown message action")
