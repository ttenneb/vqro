"""Bounded public JSON-RPC client and independent item-stream oracle.

The peer follows the existing test_vqro_provider.Core request/future pattern,
without importing an adapter. Inputs are bytes split on LF, never text splitlines.
"""
import asyncio
import base64
from collections import deque
from copy import deepcopy
import json
import re

MAX_RECORD = 1 << 20
MAX_ITEM = 64 << 10


class WireFailure(RuntimeError):
    pass


class RpcError(WireFailure):
    def __init__(self, error):
        self.error = error
        self.data = error.get("data") or {}
        self.kind = self.data.get("kind", "unknown")
        super().__init__(error.get("message", self.kind))


class Peer:
    """One connection; bounded messages and requests; no automatic request replay."""

    def __init__(self, reader, writer, *, max_record=MAX_RECORD, max_pending=128,
                 max_notifications=256, notification_bytes=4 << 20):
        self.reader, self.writer = reader, writer
        self.max_record, self.max_pending = max_record, max_pending
        self.max_notifications, self.notification_bytes = max_notifications, notification_bytes
        self.pending, self.notifications = {}, deque()
        self.notification_size = 0
        self.serial, self.failure = 0, None
        self.callbacks = {}
        self.changed = asyncio.Event()
        self.write_lock = asyncio.Lock()
        self.task = asyncio.create_task(self._read())

    @classmethod
    async def connect(cls, endpoint, **kwargs):
        reader, writer = await asyncio.open_unix_connection(str(endpoint), limit=MAX_RECORD + 1)
        return cls(reader, writer, **kwargs)

    def on(self, method, callback):
        self.callbacks[method] = callback

    def _signal(self):
        old, self.changed = self.changed, asyncio.Event()
        old.set()

    async def _read(self):
        try:
            while True:
                line = await self.reader.readline()
                if not line:
                    raise WireFailure("connection closed; pending effects are not replayed")
                if len(line) > self.max_record or not line.endswith(b"\n"):
                    raise WireFailure("unterminated or oversized JSON-RPC record")
                row = json.loads(line)
                if not isinstance(row, dict) or row.get("jsonrpc") != "2.0":
                    raise WireFailure("invalid JSON-RPC envelope")
                if "method" not in row:
                    future = self.pending.pop(row.get("id"), None)
                    if future is not None and not future.done():
                        if ("result" in row) == ("error" in row):
                            future.set_exception(WireFailure("reply needs exactly one result/error"))
                        elif "error" in row:
                            future.set_exception(RpcError(row["error"]))
                        else:
                            future.set_result(row["result"])
                elif "id" in row:
                    raise WireFailure("ordinary test viewer did not register request handlers")
                else:
                    method, params = row["method"], row.get("params", {})
                    callback = self.callbacks.get(method)
                    if callback is not None:
                        callback(params)
                    else:
                        size = len(line)
                        if len(self.notifications) >= self.max_notifications or self.notification_size + size > self.notification_bytes:
                            raise WireFailure("test viewer notification bound exceeded")
                        self.notifications.append((row, size))
                        self.notification_size += size
                self._signal()
        except asyncio.CancelledError:
            pass
        except (OSError, ValueError, TypeError, RecursionError, WireFailure) as error:
            self.failure = error
        finally:
            error = self.failure or WireFailure("peer stopped")
            for future in self.pending.values():
                if not future.done():
                    future.set_exception(error)
            self.pending.clear()
            self._signal()

    async def send(self, row):
        data = json.dumps(row, ensure_ascii=False, separators=(",", ":")).encode() + b"\n"
        if len(data) > self.max_record:
            raise WireFailure("outbound test record exceeds bound")
        if self.failure is not None:
            raise WireFailure(str(self.failure))
        async with self.write_lock:
            self.writer.write(data)
            await self.writer.drain()

    async def call(self, method, params=None, *, timeout=2):
        if len(self.pending) >= self.max_pending:
            raise WireFailure("test viewer pending-call bound exceeded")
        self.serial += 1
        ident = self.serial
        future = asyncio.get_running_loop().create_future()
        self.pending[ident] = future
        try:
            await self.send({"jsonrpc": "2.0", "id": ident, "method": method, "params": params or {}})
            return await asyncio.wait_for(future, timeout)
        finally:
            self.pending.pop(ident, None)

    async def note(self, method, params=None):
        await self.send({"jsonrpc": "2.0", "method": method, "params": params or {}})

    def take_notifications(self, method=None):
        taken, kept = [], deque()
        while self.notifications:
            row, size = self.notifications.popleft()
            if method is None or row["method"] == method:
                taken.append(row)
                self.notification_size -= size
            else:
                kept.append((row, size))
        self.notifications = kept
        return taken

    async def wait(self, predicate, *, timeout=2):
        async def waiting():
            while True:
                changed = self.changed
                result = predicate()
                if result:
                    return result
                if self.failure is not None:
                    raise WireFailure(str(self.failure))
                await changed.wait()
        return await asyncio.wait_for(waiting(), timeout)

    async def close(self):
        self.writer.close()
        try:
            await self.writer.wait_closed()
        except OSError:
            pass
        self.task.cancel()
        await asyncio.gather(self.task, return_exceptions=True)


class ItemReducer:
    """Independent oracle: a snapshot is installed only at its matching end.

    Keeps bounded item state and a bounded trace. A gap invalidates a provisional
    reconstruction, never silently repairs an append or promotes tail to complete.
    """

    def __init__(self, *, max_record=MAX_ITEM, max_entities=1024,
                 max_entity=MAX_ITEM, max_bytes=2 << 20, trace_records=128, trace_bytes=128 << 10):
        self.max_record, self.max_entities = max_record, max_entities
        self.max_entity, self.max_bytes = max_entity, max_bytes
        self.entities, self.staged = {}, None
        self.epoch, self.sequence = None, None
        self.coverage, self.session = None, None
        self.snapshot_epoch, self.snapshot_coverage, self.snapshot_session = None, None, None
        self.pending = bytearray()
        self.trace = deque()
        self.trace_records, self.trace_bytes = trace_records, trace_bytes
        self.trace_size, self.trace_dropped = 0, 0
        self.notices = deque(maxlen=64)
        self.failure = None
        self.offset = 0
        self.snapshots = 0

    @staticmethod
    def _size(value):
        return len(json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode())

    def _budget(self, target):
        if len(target) > self.max_entities or sum(self._size(v) for v in target.values()) > self.max_bytes:
            raise WireFailure("item reconstruction exceeds the test viewer's declared bound")
        if any(self._size(v) > self.max_entity for v in target.values()):
            raise WireFailure("item entity exceeds the 64 KiB bound")

    @staticmethod
    def _field(value, pointer):
        if not isinstance(pointer, str) or not pointer.startswith("/") or re.search(r"~(?:[^01]|$)", pointer):
            raise WireFailure("append needs a valid JSON Pointer")
        parts = [p.replace("~1", "/").replace("~0", "~") for p in pointer[1:].split("/")]
        def index(target, part):
            if not isinstance(target, list):
                return part
            if not part.isascii() or not part.isdigit() or (len(part) > 1 and part.startswith("0")):
                raise WireFailure("invalid JSON Pointer array index")
            return int(part)
        target = value
        try:
            for part in parts[:-1]:
                target = target[index(target, part)]
            last = index(target, parts[-1])
            if not isinstance(target[last], str):
                raise WireFailure("append target is not an existing string/bytes field")
            return target, last
        except (KeyError, IndexError, TypeError, ValueError) as error:
            raise WireFailure("append target does not exist") from error

    @classmethod
    def _append(cls, entity, kind, row):
        pointer = row.get("field")
        target, key = cls._field(entity["value"], pointer)
        binary = entity.setdefault("bytes_fields", [])
        if "bytes" in row:
            if "text" in row or not isinstance(row["bytes"], str):
                raise WireFailure("bytes append is not exactly one base64 chunk")
            try:
                data = base64.b64decode(target[key], validate=True) + base64.b64decode(row["bytes"], validate=True)
            except (ValueError, UnicodeError) as error:
                raise WireFailure("invalid base64 bytes field/chunk") from error
            target[key] = base64.b64encode(data).decode("ascii")
            if pointer not in binary:
                binary.append(pointer)
        else:
            if not isinstance(row.get("text"), str):
                raise WireFailure("text append needs a string")
            if pointer in binary or (kind == "terminal" and pointer == "/output"):
                raise WireFailure("text append cannot apply to a bytes field")
            target[key] += row["text"]

    def accept(self, row):
        if not isinstance(row, dict):
            raise WireFailure("item record is not an object")
        size = self._size(row)
        while self.trace and (len(self.trace) >= self.trace_records or self.trace_size + size > self.trace_bytes):
            self.trace_size -= self._size(self.trace.popleft())
            self.trace_dropped += 1
        if size <= self.trace_bytes and self.trace_records:
            self.trace.append(deepcopy(row))
            self.trace_size += size
        else:
            self.trace_dropped += 1
        op = row.get("op")
        # Normative §4 spells the record names; accept no adapter-specific payload.
        if op == "notice":
            if "seq" in row:
                raise WireFailure("live notice must not consume a sequence")
            self.notices.append(deepcopy(row.get("value")))
            return
        if op == "snapshot":
            phase = row.get("phase")
            if phase == "begin":
                if self.staged is not None:
                    raise WireFailure("overlapping unfinished snapshots")
                if not isinstance(row.get("epoch"), str) or row.get("coverage") not in ("all_retained", "tail", "state_only"):
                    raise WireFailure("snapshot lacks epoch/coverage")
                self.staged = {}
                self.snapshot_epoch, self.snapshot_coverage = row["epoch"], row["coverage"]
                self.snapshot_session = row.get("session")
            elif phase == "end":
                if self.staged is None or not isinstance(row.get("seq"), int) or isinstance(row.get("seq"), bool) or row["seq"] < 0:
                    raise WireFailure("snapshot end without begin/cut")
                if row.get("session") != self.snapshot_session:
                    raise WireFailure("snapshot end belongs to a different session")
                self.entities, self.staged = self.staged, None
                self.epoch, self.coverage = self.snapshot_epoch, self.snapshot_coverage
                self.sequence = row["seq"]
                self.session = self.snapshot_session
                self.snapshots += 1
            else:
                raise WireFailure("unknown snapshot phase")
            return
        if op not in ("put", "append", "remove"):
            raise WireFailure("unknown required item record")
        if self.staged is None:
            seq = row.get("seq")
            if self.epoch is None or not isinstance(seq, int) or isinstance(seq, bool) or seq < 0:
                raise WireFailure("live record before a fenced epoch/cursor")
            if self.sequence is not None and seq <= self.sequence:
                return
            if self.sequence is not None and seq != self.sequence + 1:
                raise WireFailure("noncontiguous item sequence")
            target = self.entities
        else:
            if op != "put" or "seq" in row:
                raise WireFailure("snapshot must contain unsequenced full replacements")
            target = self.staged
        kind, ident, rev = row.get("kind"), row.get("id"), row.get("rev")
        if not isinstance(kind, str) or not isinstance(ident, str) or len(ident.encode()) > 128 or not isinstance(rev, int) or isinstance(rev, bool) or rev < 0:
            raise WireFailure("entity lacks typed kind/id/revision")
        key = (kind, ident)
        previous = target.get(key)
        if previous is not None and rev <= previous["rev"]:
            raise WireFailure("entity revision did not increase")
        candidate = deepcopy(target)
        if op == "put":
            if not isinstance(row.get("value"), dict):
                raise WireFailure("put value is not an object")
            value = deepcopy(row["value"])
            if previous is not None and "position" in previous["value"] and value.get("position") != previous["value"]["position"]:
                raise WireFailure("timeline position changed")
            binary = ["/output"] if kind == "terminal" and "output" in value else []
            for pointer in (previous or {}).get("bytes_fields", []):
                try:
                    self._field(value, pointer)
                except WireFailure:
                    continue
                if pointer not in binary:
                    binary.append(pointer)
            for pointer in binary:
                target_field, field_key = self._field(value, pointer)
                try:
                    base64.b64decode(target_field[field_key], validate=True)
                except (ValueError, UnicodeError) as error:
                    raise WireFailure("put bytes field is not base64") from error
            candidate[key] = {"rev": rev, "value": value, "bytes_fields": binary}
        elif op == "append":
            if previous is None or row.get("base") != previous["rev"]:
                raise WireFailure("append base is a gap")
            self._append(candidate[key], kind, row)
            candidate[key]["rev"] = rev
        else:
            candidate.pop(key, None)  # a tail/state-only viewer may not retain it
        self._budget(candidate)
        if self.staged is None:
            self.entities = candidate
            self.sequence = row["seq"]
        else:
            self.staged = candidate

    def feed(self, offset, data):
        if self.failure is not None:
            return
        try:
            if offset != self.offset:
                raise WireFailure("stream byte offset is not contiguous")
            self.offset += len(data)
            self.pending.extend(data)
            while b"\n" in self.pending:
                line, _, rest = self.pending.partition(b"\n")
                self.pending = bytearray(rest)
                if len(line) > self.max_record:
                    raise WireFailure("item line exceeds 64 KiB")
                self.accept(json.loads(line.decode("utf-8")))
            if len(self.pending) > self.max_record:
                raise WireFailure("unterminated item exceeds 64 KiB")
        except (UnicodeError, ValueError, TypeError, RecursionError, WireFailure) as error:
            self.failure = str(error)
            self.staged = None
            self.pending.clear()

    def stream_data(self, params):
        try:
            data = base64.b64decode(params["data"], validate=True)
            if len(data) > 64 << 10:
                raise WireFailure("stream frame exceeds 64 KiB")
            self.feed(params["offset"], data)
        except (KeyError, ValueError, WireFailure) as error:
            self.failure = str(error)
            self.staged = None

    def ended(self, reason):
        # No partial line or snapshot survives a stream close, even unknown gaps.
        self.pending.clear()
        self.staged = None
        if reason not in ("closed", "exited", "killed"):
            self.failure = str(reason)

    @property
    def cursor(self):
        return None if self.epoch is None else {"epoch": self.epoch, "seq": self.sequence}
