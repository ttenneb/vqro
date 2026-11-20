"""Bounded receipt projection of canonical agent-v0 item records, with no I/O.

A cursor means fully applied input, never received bytes. Feed is transactional:
if any line fails, none of that feed's projections/cursor changes are committed.
Snapshot receipt state is installed/emitted only at its end fence. Other entity
bodies are parsed for framing but are neither retained nor inferred as receipts.
"""
import copy
import json


class ReceiptError(Exception):
    def __init__(self, kind, message):
        super().__init__(message)
        self.kind = kind


def _error(kind, message):
    raise ReceiptError(kind, message)


def _integer(value, name, minimum=0):
    if type(value) is not int or value < minimum:
        _error("invalid", name + " must be an integer >= " + str(minimum))
    return value


def _string(value, name, limit=128):
    try:
        valid = isinstance(value, str) and bool(value) and len(value.encode("utf-8")) <= limit
    except UnicodeError:
        valid = False
    if not valid:
        _error("invalid", name + " must be a bounded nonempty string")
    return value


def _encoded(value):
    try:
        return json.dumps(value, ensure_ascii=False, allow_nan=False,
                          separators=(",", ":"), sort_keys=True).encode("utf-8")
    except (TypeError, ValueError, UnicodeError, RecursionError):
        _error("invalid", "receipt contains invalid JSON")


def _object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            _error("invalid", "duplicate JSON object key")
        result[key] = value
    return result


def _constant(_value):
    _error("invalid", "non-finite JSON number")


def _projection(value):
    if not isinstance(value, dict):
        _error("invalid", "receipt value must be an object")
    _string(value.get("delivery"), "delivery")
    _string(value.get("stage"), "receipt stage")
    return copy.deepcopy({k: value[k] for k in ("delivery", "stage", "run", "cause") if k in value})


def _append(value, pointer, text):
    """Apply only a checked JSON Pointer to an existing string; never invent bases."""
    if not isinstance(pointer, str) or not pointer.startswith("/") or not isinstance(text, str):
        _error("invalid", "receipt append needs a JSON Pointer and text")
    parts = []
    for raw in pointer[1:].split("/"):
        part, i = "", 0
        while i < len(raw):
            if raw[i] == "~":
                if i + 1 >= len(raw) or raw[i + 1] not in "01":
                    _error("invalid", "invalid JSON Pointer escape")
                part += "~" if raw[i + 1] == "0" else "/"
                i += 2
            else:
                part += raw[i]
                i += 1
        parts.append(part)
    result = copy.deepcopy(value)
    node = result
    for index, part in enumerate(parts):
        if isinstance(node, list):
            if not part.isascii() or not part.isdigit() or (len(part) > 1 and part[0] == "0"):
                _error("gap", "receipt append array index is not an existing index")
            # Limit before int(): malicious giant indices are not useful work.
            if len(part) > 10 or int(part) >= len(node):
                _error("gap", "receipt append index out of range")
            part = int(part)
        elif not isinstance(node, dict) or part not in node:
            _error("gap", "receipt append field does not exist")
        if index == len(parts) - 1:
            if not isinstance(node[part], str):
                _error("gap", "receipt append target is not text")
            node[part] += text
        else:
            node = node[part]
    return result


class _State:
    """Copy-on-write entity map: ordinary non-receipt frames copy no entity bodies."""
    def __init__(self, epoch, seq=None, entities=None, size=0, ready=False):
        self.epoch, self.seq = epoch, seq
        self.entities = {} if entities is None else entities
        self.size, self.ready = size, ready

    def clone(self):
        return _State(self.epoch, self.seq, self.entities, self.size, self.ready)


class ReceiptCollector:
    def __init__(self, epoch, after=None, entity_limit=256, byte_limit=262144, line_limit=65536):
        _string(epoch, "epoch")
        self.entity_limit = _integer(entity_limit, "entity_limit", 1)
        self.byte_limit = _integer(byte_limit, "byte_limit", 1)
        self.line_limit = _integer(line_limit, "line_limit", 1)
        if line_limit > 65536:
            _error("invalid", "canonical record limit is 65536 bytes")
        seq = None
        if after is not None:
            if not isinstance(after, dict) or set(after) != {"epoch", "seq"} or after["epoch"] != epoch:
                _error("invalid", "after must name the attached epoch and seq")
            seq = _integer(after["seq"], "after.seq")
        self._state = _State(epoch, seq, ready=after is not None)
        self._cut = None
        self._coverage = None
        self._session = None
        self._line = b""
        self._needs_snapshot = after is None

    @property
    def cursor(self):
        state = self._state
        return None if state.seq is None else {"epoch": state.epoch, "seq": state.seq}

    def discard_partial(self):
        """Stream close: preserve committed cursor, discard line and unfinished cut."""
        self._line = b""
        if self._cut is not None:
            self._needs_snapshot = True
        self._cut = None
        self._coverage = self._session = None

    def _store(self, state, identifier, revision, value):
        # Validate full receipt including native extras, but publish only projection.
        _projection(value)
        entity = {"rev": revision, "value": value}
        charge = len(_encoded([identifier, entity]))
        old = state.entities.get(identifier)
        size = state.size - (old["charge"] if old else 0) + charge
        if (identifier not in state.entities and len(state.entities) >= self.entity_limit) or size > self.byte_limit:
            _error("overflow", "receipt materialization exceeds its bound")
        state.entities = dict(state.entities)
        state.entities[identifier] = {**entity, "charge": charge}
        state.size = size

    def feed(self, data):
        if not isinstance(data, (bytes, bytearray, memoryview)):
            _error("invalid", "feed requires bytes")
        # Local staging prevents an error later in this frame from advancing the
        # cursor past an earlier projection the caller never received.
        state = self._state.clone()
        cut = self._cut.clone() if self._cut is not None else None
        coverage, session = self._coverage, self._session
        needs_snapshot = self._needs_snapshot
        line = self._line
        emitted, emitted_bytes = [], 2

        def emit(epoch, seq, value):
            nonlocal emitted_bytes
            event = {"epoch": epoch, "seq": seq, "receipt": _projection(value)}
            emitted_bytes += len(_encoded(event)) + 1
            if emitted_bytes > self.byte_limit:
                _error("overflow", "receipt projections exceed one feed's bound")
            emitted.append(event)

        def fields(record, required, optional=()):
            if not required <= record.keys() or record.keys() - required - set(optional):
                _error("invalid", "non-canonical record fields")

        try:
            offset = 0
            while offset < len(data):
                # Never concatenate an unbounded frame into the partial-line buffer.
                end = data.find(b"\n", offset) if isinstance(data, (bytes, bytearray)) else bytes(data[offset:offset + self.line_limit + 1]).find(b"\n")
                if isinstance(data, memoryview) and end >= 0:
                    end += offset
                stop = len(data) if end < 0 else end
                length = len(line) + stop - offset
                if length + (1 if end >= 0 else 0) > self.line_limit:
                    _error("overflow", "JSONL record exceeds line_limit")
                line += bytes(data[offset:stop])
                offset = stop + (1 if end >= 0 else 0)
                if end < 0:
                    break
                try:
                    record = json.loads(line.decode("utf-8"), object_pairs_hook=_object,
                                        parse_constant=_constant)
                except (ValueError, UnicodeError, RecursionError):
                    _error("invalid", "malformed UTF-8 JSONL record")
                line = b""
                if not isinstance(record, dict):
                    _error("invalid", "item record must be an object")
                op = record.get("op")
                if op == "notice":
                    fields(record, {"op", "value"})
                    continue
                if op == "snapshot":
                    phase = record.get("phase")
                    if phase == "begin":
                        fields(record, {"op", "phase", "epoch", "coverage"}, {"session"})
                        epoch = _string(record["epoch"], "snapshot epoch")
                        coverage = _string(record["coverage"], "coverage")
                        session = record.get("session")
                        if session is not None:
                            _string(session, "snapshot session")
                        cut = _State(epoch)
                        needs_snapshot = True
                    elif phase == "end":
                        fields(record, {"op", "phase", "seq"}, {"session"})
                        if cut is None:
                            _error("gap", "snapshot end without begin")
                        seq = _integer(record["seq"], "snapshot seq")
                        if "session" in record and session is not None and record["session"] != session:
                            _error("gap", "snapshot session fence mismatch")
                        if state.epoch == cut.epoch and state.seq is not None and seq < state.seq:
                            _error("gap", "snapshot cannot roll an applied cursor backwards")
                        for entity in cut.entities.values():
                            emit(cut.epoch, seq, entity["value"])
                        cut.seq, cut.ready = seq, True
                        state, cut = cut, None
                        coverage = session = None
                        needs_snapshot = False
                    else:
                        _error("invalid", "unknown snapshot phase")
                    continue
                if op not in ("put", "append", "remove"):
                    _error("invalid", "unknown canonical item operation")
                required = {"op", "kind", "id", "rev"}
                if op == "put":
                    required.add("value")
                elif op == "append":
                    required.update(("base", "field"))
                    variants = [field for field in ("text", "bytes") if field in record]
                    if len(variants) != 1:
                        _error("invalid", "append needs exactly one of text or bytes")
                    required.add(variants[0])
                if cut is None:
                    required.add("seq")
                fields(record, required)
                kind = _string(record["kind"], "entity kind")
                identifier = _string(record["id"], "entity id")
                revision = _integer(record["rev"], "entity rev", 1)
                if cut is not None:
                    if op != "put":
                        _error("gap", "snapshot contains only unsequenced puts")
                    target = cut
                    if kind == "receipt" and identifier in cut.entities:
                        _error("gap", "duplicate receipt in snapshot cut")
                else:
                    if needs_snapshot or not state.ready:
                        _error("gap", "new/reset stream requires a completed snapshot")
                    seq = _integer(record["seq"], "item seq", 1)
                    if seq <= state.seq:
                        continue
                    if seq != state.seq + 1:
                        _error("gap", "noncontiguous item seq")
                    target = state
                if kind == "receipt":
                    old = target.entities.get(identifier)
                    if op == "put":
                        if old is not None and revision <= old["rev"]:
                            _error("gap", "receipt revision did not increase")
                        self._store(target, identifier, revision, record["value"])
                    elif op == "append":
                        base = _integer(record["base"], "receipt base", 1)
                        if old is None or old["rev"] != base or revision <= base:
                            _error("gap", "receipt append has no matching base")
                        if "bytes" in record:
                            _error("invalid", "receipt has no canonical bytes field")
                        value = _append(old["value"], record["field"], record["text"])
                        self._store(target, identifier, revision, value)
                    else:
                        if old is not None:
                            if revision <= old["rev"]:
                                _error("gap", "receipt removal revision did not increase")
                            target.entities = dict(target.entities)
                            target.size -= target.entities.pop(identifier)["charge"]
                    if cut is None and op != "remove":
                        emit(state.epoch, record["seq"], target.entities[identifier]["value"])
                # Non-receipt bodies never materialize; their sequence still counts.
                if cut is None:
                    state.seq = record["seq"]
        except (ReceiptError, RecursionError, UnicodeError) as error:
            # Keep only state whose projections were returned by a previous feed.
            self._line = b""
            self._cut = None
            self._coverage = self._session = None
            self._needs_snapshot = True
            if isinstance(error, ReceiptError):
                raise
            raise ReceiptError("invalid", "receipt nesting or encoding exceeds safe parsing") from error
        self._state, self._cut = state, cut
        self._coverage, self._session = coverage, session
        self._line = line
        self._needs_snapshot = needs_snapshot
        return emitted
