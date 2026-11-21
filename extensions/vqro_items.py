"""A viewer's reconstruction of one agent's item stream (docs/spec/agent-v0.md section 4).

The Python twin of the Rust SDK's `ItemView` (src/provider_client/items.rs): the same records, the same bounds and the
same error counts, so the two implementations can be checked against each other. Stdlib only.

Feed it the stream's bytes in any split. A snapshot is installed only at its `end`; live records apply in `seq` order.
After any error that loses data (`broken`), the caller closes the stream and reattaches with `after()`; the installed
state is never patched from a stream that lost records. A missing `seq`, or a live record that can't be applied whole
(an append to an unknown entity, a stale `base`, a bad field or payload, a put or append over the bounds, an unknown
op), applies nothing, breaks the view and leaves `after()` at the last record applied (parity with spike-rev-a's
Rust fix). A remove of an unknown entity is counted but advances: the entity is absent either way. Bounded like the producer: a longer line, more entities or
more entity bytes than declared is an error, never unbounded growth.
"""
import base64
import binascii
import copy
import json

MAX_RECORD_BYTES = 64 * 1024
MAX_ENTITIES = 512
MAX_ENTITY_BYTES = 512 * 1024

ERRORS = ("duplicate", "missing", "stale_base", "unknown_entity", "bad_utf8", "bad_json", "bad_record",
          "out_of_snapshot", "oversize", "over_bound")
LOSSY = ("missing", "stale_base", "over_bound", "unknown_entity", "bad_record")  # as the Rust ItemView (7a8d0fd)
# Rejections that repeat unchanged on every reattach (agent-v0 section 4, "Incompatible stream"): a record this view
# can't read (malformed or an unknown op), data beyond its bounds, a line beyond its record bound.
INCOMPATIBLE = ("bad_record", "over_bound", "oversize")
KNOWN_OPS = ("notice", "snapshot", "put", "remove", "append")


def encoded_size(value):
    return len(json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode())


def _pointer(value, field):
    """The parent container and key of a JSON Pointer (RFC 6901) that names an existing string, else None."""
    if not isinstance(field, str) or not field.startswith("/"):
        return None
    parts = [p.replace("~1", "/").replace("~0", "~") for p in field[1:].split("/")]
    node = value
    for part in parts[:-1]:
        node = _step(node, part)
        if node is None:
            return None
    last = parts[-1]
    if isinstance(node, dict) and isinstance(node.get(last), str):
        return node, last
    index = _index(last)
    if isinstance(node, list) and index is not None and index < len(node) and isinstance(node[index], str):
        return node, index
    return None


def _index(part):
    """An RFC 6901 array index: ASCII digits without a leading zero (as serde_json), else None."""
    return int(part) if part == "0" or (part.isascii() and part.isdigit() and part[0] != "0") else None


def _step(node, part):
    if isinstance(node, dict):
        return node.get(part)
    index = _index(part)
    if isinstance(node, list) and index is not None and index < len(node):
        return node[index]
    return None


class ItemView:
    def __init__(self, max_record_bytes=MAX_RECORD_BYTES, max_entities=MAX_ENTITIES, max_entity_bytes=MAX_ENTITY_BYTES):
        self.max_record_bytes, self.max_entities, self.max_entity_bytes = max_record_bytes, max_entities, max_entity_bytes
        self.epoch = None
        self.seq = 0
        self.coverage = None
        self.entities = {}  # (kind, id) -> (rev, value)
        self.entity_bytes = 0
        self.errors = dict.fromkeys(ERRORS, 0)
        self.records = 0  # live records applied
        self.snapshots = 0  # snapshots installed
        self.broken = False
        self.rejection = None  # the latest incompatible rejection: {kind, op?, unknown_op?, seq?, limits}
        self._pending = None  # [epoch, coverage, entities, entity bytes] while a snapshot is being received
        self._overflow = False
        self._buf = bytearray()

    def after(self):
        """The `items.after` to reattach with, or None before any installed snapshot."""
        return {"epoch": self.epoch, "seq": self.seq} if self.epoch is not None else None

    def stream_ended(self):
        """The stream closed or gapped: drop a partial line and any unfinished snapshot; keep the installed state."""
        self._buf.clear()
        self._pending = None
        self._overflow = False
        self.broken = False

    def feed(self, data, epochs=False):
        """Feed stream bytes; returns the notices received, in order. With `epochs`, each comes as (epoch, value): the
        epoch installed when that notice record was applied, so a caller can drop a notice from before a new snapshot
        whatever the byte chunking (a notice inside an unfinished snapshot still counts for the installed epoch)."""
        self._buf.extend(data)
        notices = []
        while True:
            pos = self._buf.find(b"\n")
            if pos < 0:
                if len(self._buf) > self.max_record_bytes:  # a partial line never grows past the bound
                    self._buf.clear()
                    self._error("oversize")
                    self._reject("oversize", None)
                    self.broken = True
                break
            line = bytes(self._buf[:pos])
            del self._buf[:pos + 1]
            if pos >= self.max_record_bytes:
                self._error("oversize")
                self._reject("oversize", None)
                self.broken = True
                continue
            if self.broken:
                continue  # after data loss nothing more is applied: reattach
            try:
                text = line.decode("utf-8")
            except UnicodeDecodeError:
                self._error("bad_utf8")
                continue
            try:
                record = json.loads(text)
            except ValueError:
                self._error("bad_json")
                continue
            before = {e: self.errors[e] for e in INCOMPATIBLE}
            lossy = sum(self.errors[e] for e in LOSSY)
            notice = self._apply(record) if isinstance(record, dict) else self._error("bad_record")
            if notice is not None:
                notices.append((self.epoch, notice) if epochs else notice)
            if sum(self.errors[e] for e in LOSSY) != lossy:
                self.broken = True
            rejected = [e for e in INCOMPATIBLE if self.errors[e] != before[e]]
            if rejected:
                self._reject(rejected[0], record)
        return notices

    def _reject(self, kind, record):
        """Keep the rejection detail an incompatible-stream report needs (never used to skip or relax anything)."""
        op = record.get("op") if isinstance(record, dict) else None
        self.rejection = {"kind": kind, "op": op if isinstance(op, str) else None,
                          "unknown_op": isinstance(op, str) and op not in KNOWN_OPS,
                          "seq": record.get("seq") if isinstance(record, dict) and type(record.get("seq")) is int else None,
                          "limits": {"record_bytes": self.max_record_bytes, "entities": self.max_entities,
                                     "entity_bytes": self.max_entity_bytes}}

    def _error(self, kind):
        self.errors[kind] += 1

    def _admit(self, entities, total, key, value):
        """The new entity-bytes total if `value` (new or replacing `key`) fits the bounds, else None."""
        size = encoded_size(value)
        old = entities.get(key)
        if old is None and len(entities) >= self.max_entities:
            return None
        total = max(0, total - (encoded_size(old[1]) if old else 0)) + size
        return total if total <= self.max_entity_bytes else None

    def _apply(self, r):
        op = r.get("op")
        if op == "notice":
            return r.get("value")
        if op == "snapshot":
            pending, self._pending = self._pending, None
            phase = r.get("phase")
            if phase == "begin":
                if isinstance(r.get("epoch"), str) and isinstance(r.get("coverage"), str):
                    self._pending = [r["epoch"], r["coverage"], {}, 0]
                else:
                    self._error("bad_record")
            elif phase == "end" and pending is not None:
                if self._overflow:  # an overflowing snapshot is dropped whole (the view is broken: reattach)
                    self._overflow = False
                elif type(r.get("seq")) is int and r["seq"] >= 0:
                    # Installed only now: epoch, every entity and the cut's seq.
                    self.epoch, self.coverage, self.entities, self.entity_bytes = pending
                    self.seq = r["seq"]
                    self.snapshots += 1
                else:
                    self._error("bad_record")
            else:
                self._error("out_of_snapshot")
            return None
        key = (r.get("kind"), r.get("id"))
        valid_key = isinstance(key[0], str) and isinstance(key[1], str)
        rev = r.get("rev")
        if self._pending is not None:  # inside a snapshot: only puts without seq
            entities = self._pending[2]
            if op == "put" and valid_key and type(rev) is int and "seq" not in r and "value" in r:
                total = self._admit(entities, self._pending[3], key, r["value"])  # a running total: linear
                if total is None:
                    self._error("over_bound")
                    self._overflow = True
                else:
                    entities[key], self._pending[3] = (rev, r["value"]), total
            else:
                self._error("out_of_snapshot")
            return None
        seq = r.get("seq")
        if type(seq) is not int or not valid_key or type(rev) is not int:
            self._error("bad_record")
            return None
        if self.epoch is None:
            self._error("out_of_snapshot")
            return None
        if seq <= self.seq:
            self._error("duplicate")
            return None
        if seq != self.seq + 1:
            # Records were lost: apply nothing and keep the cursor at the last record applied, so after() never
            # points past the loss (the caller reattaches from it, or from a fresh snapshot).
            self._error("missing")
            return None
        # Validate the whole record before anything moves: a record that can't be applied is a loss (the view
        # breaks and after() stays at the last record applied, so a reattach replays from there), never skipped.
        if op == "put" and "value" in r:
            total = self._admit(self.entities, self.entity_bytes, key, r["value"])
            if total is None:
                return self._loss("over_bound")
            self.entity_bytes, self.entities[key] = total, (rev, r["value"])
        elif op == "remove":
            old = self.entities.pop(key, None)
            if old is None:
                self._error("unknown_entity")  # absent either way: counted (a loss), but the cursor advances
            else:
                self.entity_bytes = max(0, self.entity_bytes - encoded_size(old[1]))
        elif op == "append":
            current = self.entities.get(key)
            if current is None:
                return self._loss("unknown_entity")
            if type(r.get("base")) is not int or r["base"] != current[0]:  # 1.0 or "1" is not a revision
                return self._loss("stale_base")
            if ("text" in r) == ("bytes" in r):  # exactly one payload key, even if the other is null
                return self._loss("bad_record")
            candidate = copy.deepcopy(current[1])  # staged: nothing changes until the result is admitted
            target = _pointer(candidate, r.get("field"))
            if target is None:
                return self._loss("bad_record")
            node, at = target
            if isinstance(r.get("text"), str):
                node[at] += r["text"]
            elif isinstance(r.get("bytes"), str):
                try:  # bytes fields are one base64 buffer: decode, append, re-encode (never concatenate text)
                    node[at] = base64.b64encode(base64.b64decode(node[at], validate=True) + base64.b64decode(r["bytes"], validate=True)).decode()
                except (binascii.Error, ValueError):
                    return self._loss("bad_record")
            else:
                return self._loss("bad_record")
            total = self._admit(self.entities, self.entity_bytes, key, candidate)  # encoded JSON bytes, escapes included
            if total is None:
                return self._loss("over_bound")
            self.entity_bytes, self.entities[key] = total, (rev, candidate)
        else:
            return self._loss("bad_record")
        self.seq = seq
        self.records += 1
        return None

    def _loss(self, kind):
        """A live record that can't be applied: count it and break the view without moving the cursor."""
        self._error(kind)
        self.broken = True
        return None
