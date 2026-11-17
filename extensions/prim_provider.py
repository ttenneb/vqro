"""Bounded stdlib asyncio provider SDK: provider-v0.md section 9, no replay.

Importing prim_ext reuses its managed stdin EOF watcher without connecting.
"""
import asyncio
from dataclasses import dataclass
import inspect
import json
import math
import os
import prim_ext  # managed extension lifetime watcher

MAX_RECORD = 1048576
MAX_DATA = 65536
OUTCOMES = frozenset({"accepted", "observed", "rejected"})


class ProviderError(Exception):
    def __init__(self, kind, message):
        super().__init__(message)
        self.kind = kind


@dataclass(frozen=True)
class Limits:
    max_streams: int = 512
    stream_window_bytes: int = 262144
    pending: int = 64
    identities: int = 65536
    outgoing_bytes: int = 2097152
    stream_queue_bytes: int = 262144
    stream_queue_items: int = 128
    deadline: float = 30.0

    def __post_init__(self):
        if any(type(v) is not int or v <= 0 for k, v in vars(self).items() if k != "deadline"):
            raise ValueError("count/byte limits must be positive integers")
        if not isinstance(self.deadline, (int, float)) or not math.isfinite(self.deadline) or self.deadline <= 0:
            raise ValueError("deadline must be finite and positive")


@dataclass(frozen=True)
class Effect:
    id: str
    op: str
    ref: str | None
    incarnation: str
    deadline: float

    @property
    def remaining(self):
        return max(0, self.deadline - asyncio.get_running_loop().time())

    @property
    def expired(self):
        return self.remaining == 0


def integer(value, minimum=0, maximum=2**64 - 1):
    if type(value) is not int or not minimum <= value <= maximum:
        raise ProviderError("invalid", "integer outside allowed range")
    return value


def reason(value):
    if not isinstance(value, dict) or not isinstance(value.get("reason"), str):
        raise ProviderError("invalid", "reason must contain reason string")
    return value


def facts(value):
    if not isinstance(value, dict):
        raise ProviderError("invalid", "facts must be an object")
    for key in ("ref", "kind"):
        if not isinstance(value.get(key), str) or not value[key]:
            raise ProviderError("invalid", f"facts requires {key}")
    if value.get("state") not in ("live", "finished") or not isinstance(value.get("incarnation", ""), str):
        raise ProviderError("invalid", "bad state/incarnation")
    for key in ("name", "title", "cwd"):
        if value.get(key) is not None and not isinstance(value[key], str):
            raise ProviderError("invalid", f"bad {key}")
    argv = value.get("argv", [])
    if not isinstance(argv, list) or any(not isinstance(v, str) for v in argv):
        raise ProviderError("invalid", "argv must be strings")
    for key in ("status", "exit"):
        if value.get(key) is not None and not isinstance(value[key], dict):
            raise ProviderError("invalid", f"bad {key}")
    data = value.get("data", {})
    if not isinstance(data, dict) or len(json.dumps(data, ensure_ascii=False, allow_nan=False).encode()) > MAX_DATA:
        raise ProviderError("invalid", "facts data must be object <=64 KiB")
    return value


async def maybe_await(value):
    return await value if inspect.isawaitable(value) else value


class Provider:
    def __init__(self, name, instance, capabilities, *, role="backend", software=None,
                 provider_version="0.1.0", token=None, limits=None):
        if not isinstance(capabilities, dict) or any(not isinstance(v, dict) for v in capabilities.values()):
            raise ValueError("capability values must be objects")
        self.name, self.instance, self.role = name, instance, role
        self.capabilities = capabilities
        self.software = software or {"name": name, "version": "0"}
        self.provider_version, self.token = provider_version, token
        self.limits = limits or Limits()
        self.generation = None
        self.window = min(self.limits.stream_window_bytes, 262144)
        self.max_record = MAX_RECORD
        self.effects = {}
        self.open_stream = None
        self.streams = {}
        self._effect_ids, self._stream_ids = set(), set()
        self._pending, self._tasks, self._workers = {}, set(), set()
        self._sequence = 0
        self._outgoing = 0
        self._writer_lock = asyncio.Lock()
        self._closed = asyncio.Event()
        self._reader_task = None
        self._sync_started = False
        self.failure = None

    def effect(self, op):
        def register(fn):
            self.effects[op] = fn
            return fn
        return register

    async def connect(self, path=None, *, sock=None):
        if self._reader_task is not None:
            raise ProviderError("conflict", "connect once; create a new Provider to reconnect")
        if sock is not None:
            self.reader, self.writer = await asyncio.open_connection(sock=sock, limit=MAX_RECORD + 1)
        else:
            self.reader, self.writer = await asyncio.open_unix_connection(path or os.environ["PRIM_SOCKET"], limit=MAX_RECORD + 1)
        self._reader_task = asyncio.create_task(self._read())
        try:
            await self.call("hello", {"extension": os.environ.get("PRIM_EXTENSION", self.name)})
            params = {"name": self.name, "instance": self.instance, "role": self.role,
                      "software": self.software, "provider_version": self.provider_version,
                      "capabilities": self.capabilities,
                      "limits": {"max_streams": self.limits.max_streams, "stream_window_bytes": self.window}}
            if self.token is not None:
                params["token"] = self.token
            result = await self.call("provider.register", params)
            self.generation = integer(result["generation"], 1)
            self.window = integer(result["limits"]["stream_window_bytes"], 1, self.window)
            self.max_record = integer(result["limits"]["max_record_bytes"], 1, MAX_RECORD)
            return self
        except BaseException:
            await self.close()
            raise

    def _spawn(self, coroutine):
        task = asyncio.create_task(coroutine)
        self._tasks.add(task)
        task.add_done_callback(lambda done: self._completed(self._tasks, done))
        return task

    def _work(self, coroutine):
        task = asyncio.create_task(coroutine)
        self._workers.add(task)
        task.add_done_callback(lambda done: self._completed(self._workers, done))
        return task

    def _completed(self, collection, task):
        collection.discard(task)
        if not task.cancelled() and task.exception() is not None:
            self._disconnect(f"handler failure: {task.exception()}")

    async def _write(self, record, deadline=None):
        if self._closed.is_set():
            raise ProviderError("unavailable", self.failure or "closed")
        wire = json.dumps(record, ensure_ascii=False, allow_nan=False).encode() + b"\n"
        if len(wire) > self.max_record:
            raise ProviderError("invalid", "record exceeds negotiated byte limit")
        if self._outgoing + len(wire) > self.limits.outgoing_bytes:
            raise ProviderError("overloaded", "outgoing byte bound full; not written")
        self._outgoing += len(wire)
        wrote = False
        try:
            deadline = deadline if deadline is not None else asyncio.get_running_loop().time() + self.limits.deadline
            async with asyncio.timeout_at(deadline):
                async with self._writer_lock:
                    if self._closed.is_set():
                        raise ProviderError("unavailable", "connection closed")
                    wrote = True
                    self.writer.write(wire)
                    await self.writer.drain()
        except asyncio.CancelledError:
            if wrote:
                self._disconnect("write cancelled; delivery unknown")
            raise
        except (TimeoutError, OSError):
            self._disconnect("writer failed; delivery unknown")
            raise ProviderError("unavailable", "writer failed; delivery unknown") from None
        finally:
            self._outgoing -= len(wire)

    async def call(self, method, params, timeout=None):
        timeout = self.limits.deadline if timeout is None else timeout
        if not isinstance(timeout, (int, float)) or not math.isfinite(timeout) or timeout <= 0:
            raise ProviderError("invalid", "timeout must be finite and positive")
        deadline = asyncio.get_running_loop().time() + timeout
        if len(self._pending) >= self.limits.pending:
            raise ProviderError("overloaded", "pending call bound full")
        if self.generation is not None:
            params = {**params, "generation": self.generation}
        self._sequence += 1
        ident = f"p-{self._sequence}"
        future = asyncio.get_running_loop().create_future()
        self._pending[ident] = (method, future)
        try:
            await self._write({"jsonrpc": "2.0", "id": ident, "method": method, "params": params}, deadline)
            return await asyncio.wait_for(asyncio.shield(future), max(0, deadline - asyncio.get_running_loop().time()))
        except TimeoutError:
            raise ProviderError("timeout", "answer deadline; delivery unknown, never resent") from None
        finally:
            self._pending.pop(ident, None)
            if not future.done():
                future.cancel()
            elif not future.cancelled():
                future.exception()  # retrieve failure even when the write raised first

    async def notify(self, method, params):
        if self.generation is None:
            raise ProviderError("unavailable", "not registered")
        await self._write({"jsonrpc": "2.0", "method": method, "params": {**params, "generation": self.generation}})

    async def sync(self, resources, *, final=True):
        rows = [facts(row) for row in resources]
        if not isinstance(final, bool):
            raise ProviderError("invalid", "final must be a boolean")
        result = await self.call("provider.sync", {"generation": self.generation, "resources": rows, "final": final})
        self._sync_started = True
        return result

    async def full_sync(self, resources):
        chunk, size = [], 0
        for row in resources:
            row = facts(row)
            length = len(json.dumps(row, ensure_ascii=False).encode())
            if chunk and size + length > self.max_record - 512:
                await self.sync(chunk, final=False)
                chunk, size = [], 0
            chunk.append(row)
            size += length + 1
        await self.sync(chunk, final=True)

    async def upsert(self, value):
        if not self._sync_started:
            raise ProviderError("conflict", "first sync must start before upsert")
        await self.notify("resource.upsert", {"facts": facts(value)})

    async def gone(self, ref, incarnation="", why=None):
        await self.notify("resource.gone", {"ref": ref, "incarnation": incarnation,
                                         "reason": reason(why or {"reason": "closed"})})

    async def _reply(self, ident, result=None, error=None):
        row = {"jsonrpc": "2.0", "id": ident}
        if error is None:
            row["result"] = result
        else:
            row["error"] = {"code": -32601 if error.kind == "unknown_method" else -32602 if error.kind == "invalid" else -32000,
                            "message": str(error), "data": {"kind": error.kind}}
        await self._write(row)

    def _generation(self, params):
        if self.generation is None or type(params.get("generation")) is not int or params["generation"] != self.generation:
            raise ProviderError("stale_generation", "wrong provider generation")

    async def _read(self):
        try:
            while True:
                wire = await self.reader.readuntil(b"\n")
                if len(wire) > self.max_record:
                    raise ProviderError("invalid", "record exceeds byte limit")
                row = json.loads(wire, parse_constant=lambda x: (_ for _ in ()).throw(ValueError(x)))
                if not isinstance(row, dict) or row.get("jsonrpc") != "2.0":
                    raise ProviderError("invalid", "invalid JSON-RPC record")
                if "method" not in row:
                    ident = row.get("id")
                    pending = self._pending.get(ident) if isinstance(ident, (str, int)) else None
                    if pending is not None:
                        pending_method, future = pending
                    if pending is not None and not future.done():
                        if "error" in row:
                            error = row["error"]
                            future.set_exception(ProviderError(error.get("data", {}).get("kind", "backend"), error.get("message", "core error")))
                        else:
                            result = row.get("result")
                            if pending_method == "provider.register":
                                # Activate before reading the next record: a core
                                # request may follow this reply in the same read.
                                self.generation = integer(result["generation"], 1)
                                self.window = integer(result["limits"]["stream_window_bytes"], 1, self.window)
                                self.max_record = integer(result["limits"]["max_record_bytes"], 1, MAX_RECORD)
                            future.set_result(result)
                    continue
                params = row.get("params", {})
                if not isinstance(params, dict):
                    raise ProviderError("invalid", "params must be object")
                try:
                    self._generation(params)
                except ProviderError as error:
                    if "id" not in row:
                        raise
                    await self._reply(row["id"], error=error)
                    continue
                if "id" in row:
                    if len(self._tasks) >= self.limits.pending:
                        await self._reply(row["id"], error=ProviderError("overloaded", "handler bound full"))
                    else:
                        self._spawn(self._request(row["id"], row["method"], params))
                else:
                    self._notification(row["method"], params)
        except (asyncio.IncompleteReadError, asyncio.LimitOverrunError, ValueError, TypeError, KeyError, AttributeError, RecursionError, ProviderError, OSError) as error:
            self._disconnect(str(error) or "connection EOF")

    async def _request(self, ident, method, params):
        stream = None
        try:
            if method == "effect.invoke":
                result = await self._effect(params)
            elif method == "stream.open":
                stream, result = await self._open(params)
            else:
                raise ProviderError("unknown_method", f"unknown method {method}")
            await self._reply(ident, result)
            if stream is not None:
                stream._ready.set()
        except asyncio.CancelledError:
            raise
        except Exception as error:
            if stream is not None:
                stream._finish()
            error = error if isinstance(error, ProviderError) else ProviderError("backend", str(error))
            if not self._closed.is_set():
                await self._reply(ident, error=error)

    async def _effect(self, params):
        ident, op, args = params.get("effect"), params.get("op"), params.get("args")
        if not isinstance(ident, str) or not isinstance(op, str) or not isinstance(args, dict):
            raise ProviderError("invalid", "effect/op/args required")
        duration = integer(params.get("deadline_ms"), 1, 300000)
        if ident in self._effect_ids:
            raise ProviderError("conflict", "duplicate effect; never executed again")
        if len(self._effect_ids) >= self.limits.identities:
            raise ProviderError("overloaded", "effect identity ledger full")
        if op not in self.capabilities or op not in self.effects:
            raise ProviderError("unsupported", f"unsupported op {op}")
        self._effect_ids.add(ident)
        if params.get("ref") is not None and not isinstance(params["ref"], str):
            raise ProviderError("invalid", "ref must be a string")
        if not isinstance(params.get("incarnation", ""), str):
            raise ProviderError("invalid", "incarnation must be a string")
        ctx = Effect(ident, op, params.get("ref"), params.get("incarnation", ""),
                     asyncio.get_running_loop().time() + duration / 1000)
        # Exactly one eventual reply, even after the core deadline. Never cancel
        # or retry a possibly dispatched backend effect merely because it expired.
        result = await maybe_await(self.effects[op](ctx, args))
        if not isinstance(result, dict) or result.get("outcome") not in OUTCOMES:
            raise ProviderError("backend", "handler did not return a v0 native outcome")
        if result["outcome"] == "rejected":
            error = result.get("error")
            if not isinstance(error, dict) or not isinstance(error.get("kind"), str) or not isinstance(error.get("message"), str):
                raise ProviderError("backend", "rejected outcome needs kind/message")
        return result

    async def _open(self, params):
        from prim_provider_stream import Stream
        if "terminal.output" not in self.capabilities or self.open_stream is None:
            raise ProviderError("unsupported", "terminal output unsupported")
        ident = integer(params.get("stream"))
        if ident in self._stream_ids:
            raise ProviderError("conflict", "stream id already used")
        if len(self.streams) >= self.limits.max_streams or len(self._stream_ids) >= self.limits.identities:
            raise ProviderError("overloaded", "stream bound full")
        integer(params.get("cols"), 1, 9999)
        integer(params.get("rows"), 1, 9999)
        integer(params.get("history_lines"), 0, 2**32 - 1)
        integer(params.get("window_bytes"), 1, self.window)
        if not isinstance(params.get("ref"), str) or not isinstance(params.get("incarnation"), str):
            raise ProviderError("invalid", "ref/incarnation required")
        self._stream_ids.add(ident)
        stream = Stream(self, params)
        self.streams[ident] = stream
        try:
            initial = await maybe_await(self.open_stream(stream))
            if initial not in ("none", "history", "repaint"):
                raise ProviderError("backend", "bad initial kind")
            return stream, {"initial": initial}
        except BaseException:
            stream._finish()
            raise

    def _notification(self, method, params):
        if method not in {"stream.credit", "stream.input", "stream.resize", "stream.close"}:
            raise ProviderError("unknown_method", f"unknown notification {method}")
        stream = self.streams.get(integer(params.get("stream")))
        if stream is None:
            return
        try:
            if method == "stream.credit":
                stream._credit(integer(params.get("bytes"), 1, 2**32 - 1))
            else:
                stream._enqueue(method, params)
        except ProviderError as error:
            if len(self._tasks) >= self.limits.pending:
                raise ProviderError("overloaded", "notification error bound full")
            self._spawn(stream.error(error))

    def _disconnect(self, message):
        if self._closed.is_set():
            return
        self.failure = message
        self._closed.set()
        for _, future in self._pending.values():
            if not future.done():
                future.set_exception(ProviderError("unavailable", message))
        for stream in list(self.streams.values()):
            stream._finish()
        for task in list(self._tasks):
            if task is not asyncio.current_task():
                task.cancel()
        self.writer.close()

    async def wait_closed(self):
        await self._closed.wait()

    async def close(self):
        if self._reader_task is None:
            return
        self._disconnect("provider closed")
        if self._reader_task is not asyncio.current_task():
            self._reader_task.cancel()
            await asyncio.gather(self._reader_task, return_exceptions=True)
        tasks = list(self._tasks | self._workers)
        for task in tasks:
            task.cancel()
        if tasks:
            await asyncio.gather(*tasks, return_exceptions=True)
        try:
            await asyncio.wait_for(self.writer.wait_closed(), 1)
        except (TimeoutError, OSError):
            pass
