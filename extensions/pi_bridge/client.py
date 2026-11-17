"""Bounded, stdlib-only asynchronous client for Pi 1.5's JSONL RPC (not JSON-RPC)."""
import asyncio
from collections import deque
from copy import deepcopy
from dataclasses import dataclass
import json
import math


@dataclass(frozen=True)
class Limits:
    record_bytes: int = 262144
    pending: int = 32
    event_bytes: int = 524288
    stderr_bytes: int = 65536
    requests: int = 32
    deadline: float = 5.0

    def __post_init__(self):
        for name in ("record_bytes", "pending", "event_bytes", "stderr_bytes", "requests"):
            if not isinstance(getattr(self, name), int) or getattr(self, name) <= 0:
                raise ValueError(f"{name} must be positive")
        if not math.isfinite(self.deadline) or self.deadline <= 0:
            raise ValueError("deadline must be finite and positive")


DIALOGS = frozenset({"select", "confirm", "input", "editor"})
NOTICES = frozenset({"notify", "setStatus", "setWidget", "setTitle", "set_editor_text"})
EFFECTS = frozenset({"prompt", "steer", "follow_up", "abort", "clear_queue", "answer"})


class PiRpcClient:
    """One child, one event loop, no automatic retries or reconnects.

    Construct with ``await PiRpcClient.start(argv, cwd=..., env={...})`` inside
    the backend-owned bridge, never in prim core. ``request`` is the metadata
    escape hatch; ``invoke`` limits the public effects to native Pi commands.
    """

    @classmethod
    async def start(cls, argv, *, cwd, env, limits=None, supported_dialogs=DIALOGS):
        self = cls(limits or Limits(), supported_dialogs)
        self.process = await asyncio.create_subprocess_exec(
            *argv, cwd=cwd, env=dict(env), stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE,
            limit=self.limits.record_bytes + 1,
        )
        self._stdout = asyncio.create_task(self._read_stdout())
        self._stderr = asyncio.create_task(self._read_stderr())
        self._watcher = asyncio.create_task(self._watch())
        return self

    def __init__(self, limits, supported_dialogs):
        self.limits = limits
        self.supported_dialogs = DIALOGS.intersection(supported_dialogs)
        self._facts = {
            "available": True, "status": "unknown", "stale": False,
            "run": {"active": None, "settled": None, "aborted": None},
            "queues": {"steering": [], "follow_up": []}, "requests": [],
        }
        self._waiting = {}
        self._writer = asyncio.Lock()
        self._next_id = 0
        self._sequence = 0
        self._lost_through = 0
        self._events = deque()
        self._event_bytes = 0
        self._stderr_tail = bytearray()
        self._stderr_total = 0
        self._changed = asyncio.Event()
        self._ui_tasks = set()
        self._answering = set()
        self._dialog_timeouts = {}
        self._closing = False
        self._failure = None

    def snapshot(self):
        """Detached current facts. Unknown/stale is not silently called idle."""
        return deepcopy(self._facts)

    def events(self, after=0):
        """Bounded journal; ``gap`` requires a new snapshot, not raw replay.

        Events are session-level. Pi provides no per-prompt completion IDs.
        """
        return {"cursor": self._sequence, "gap": after < self._lost_through or after > self._sequence,
                "events": [deepcopy(row) for _, row in self._events if row["seq"] > after]}

    def diagnostics(self):
        return {"stderr": bytes(self._stderr_tail), "stderr_total": self._stderr_total,
                "event_bytes": self._event_bytes, "pending": len(self._waiting),
                "stdin_buffer_bytes": self.process.stdin.transport.get_write_buffer_size(),
                "failure": self._failure}

    async def wait_for_facts(self, predicate, timeout=5):
        async def wait():
            while True:
                facts = self.snapshot()
                if predicate(facts):
                    return facts
                changed = self._changed
                await changed.wait()
        return await asyncio.wait_for(wait(), timeout)

    def _publish(self, event):
        self._sequence += 1
        row = {**event, "seq": self._sequence}
        size = len(json.dumps(row, ensure_ascii=True).encode())
        # Oversized events cause an explicit journal gap, not unbounded retention.
        while self._events and self._event_bytes + size > self.limits.event_bytes:
            old_size, old = self._events.popleft()
            self._event_bytes -= old_size
            self._lost_through = old["seq"]
        if size <= self.limits.event_bytes:
            self._events.append((size, row))
            self._event_bytes += size
        else:
            self._lost_through = self._sequence
        self._changed.set()
        self._changed = asyncio.Event()

    def _disconnect(self, reason):
        if self._failure is not None:
            return
        self._failure = reason
        self._facts.update(available=False, stale=True, status="unknown")
        for handle in self._dialog_timeouts.values():
            handle.cancel()
        self._dialog_timeouts.clear()
        for ident, (_, future) in list(self._waiting.items()):
            if not future.done():
                future.set_result({"outcome": "indeterminate", "id": ident, "reason": reason})
        self._publish({"type": "disconnected", "reason": reason})

    def _encode(self, record):
        wire = json.dumps(record, ensure_ascii=False, allow_nan=False).encode("utf-8") + b"\n"
        if len(wire) > self.limits.record_bytes:
            raise ValueError("command exceeds record byte limit")
        return wire

    async def _write(self, record, deadline):
        """Serialize complete records; bound lock acquisition and pipe drain."""
        wire = self._encode(record)
        remaining = lambda: max(0, deadline - asyncio.get_running_loop().time())
        await asyncio.wait_for(self._writer.acquire(), remaining())
        try:
            if not self._facts["available"] or self._closing:
                raise ConnectionError(self._failure or "client closing")
            try:
                self.process.stdin.write(wire)
                await asyncio.wait_for(self.process.stdin.drain(), remaining())
            except (TimeoutError, ConnectionError, OSError, asyncio.CancelledError):
                # A timed-out drain may leave bytes in the transport. Refuse all
                # further writes so repeated deadlines cannot grow that buffer.
                self._disconnect("stdin write interrupted; delivery unknown")
                raise
        finally:
            self._writer.release()

    def _timeout(self, timeout):
        timeout = self.limits.deadline if timeout is None else timeout
        if not isinstance(timeout, (int, float)) or not math.isfinite(timeout) or timeout <= 0:
            raise ValueError("timeout must be finite and positive")
        return asyncio.get_running_loop().time() + timeout

    async def request(self, command, args=None, *, timeout=None):
        """Send once. Native success is acceptance, not completion.

        On deadline/cancellation the command may have reached Pi. A late response
        is journaled as late_response, never resent and never a second result.
        """
        if not isinstance(command, str) or (args is not None and not isinstance(args, dict)):
            return {"outcome": "rejected", "reason": "invalid command/arguments"}
        if not self._facts["available"] or self._closing:
            return {"outcome": "rejected", "reason": "not connected; not sent"}
        if len(self._waiting) >= self.limits.pending:
            return {"outcome": "rejected", "reason": "pending limit; not sent"}
        try:
            deadline = self._timeout(timeout)
            self._next_id += 1
            ident = f"pi-{self._next_id}"
            record = {**(args or {}), "id": ident, "type": command}
            self._encode(record)
        except (ValueError, TypeError, UnicodeError, RecursionError) as error:
            return {"outcome": "rejected", "reason": str(error)}
        future = asyncio.get_running_loop().create_future()
        self._waiting[ident] = (command, future)
        try:
            await self._write(record, deadline)
            return await asyncio.wait_for(asyncio.shield(future), max(0, deadline - asyncio.get_running_loop().time()))
        except (TimeoutError, ConnectionError, OSError, asyncio.CancelledError) as error:
            # Conservative even if the write was interrupted: never replay.
            if future.done():
                return future.result()
            return {"outcome": "indeterminate", "id": ident,
                    "reason": ("cancelled" if isinstance(error, asyncio.CancelledError) else
                               "deadline" if isinstance(error, TimeoutError) else str(error))}
        finally:
            self._waiting.pop(ident, None)

    async def invoke(self, op, args=None, *, timeout=None):
        """Native effects for a future provider layer. Does not connect to prim."""
        if not isinstance(op, str) or op not in EFFECTS:
            return {"outcome": "rejected", "reason": "unsupported effect"}
        if op == "answer":
            return await self.answer(args or {}, timeout=timeout)
        return await self.request(op, args, timeout=timeout)

    async def answer(self, args, *, timeout=None):
        """Pi has no answer acknowledgment: successful pipe write is dispatched."""
        if not isinstance(args, dict):
            return {"outcome": "rejected", "reason": "invalid answer"}
        if not self._facts["available"] or self._closing:
            return {"outcome": "rejected", "reason": "not connected; not sent"}
        ident = args.get("id")
        req = next((r for r in self._facts["requests"] if r["id"] == ident), None)
        if req is None or ident in self._answering:
            return {"outcome": "rejected", "reason": "request is not pending or already answering"}
        cancelled = args.get("cancelled") is True
        if not cancelled:
            field = "confirmed" if req["kind"] == "confirm" else "value"
            if field not in args or not isinstance(args[field], bool if field == "confirmed" else str):
                return {"outcome": "rejected", "reason": f"answer needs {field}"}
        wire = {"type": "extension_ui_response", "id": ident}
        wire.update({"cancelled": True} if cancelled else {field: args[field]})
        self._answering.add(ident)
        try:
            await self._write(wire, self._timeout(timeout))
        except (ValueError, TypeError, UnicodeError, RecursionError) as error:
            return {"outcome": "rejected", "reason": str(error)}
        except (TimeoutError, ConnectionError, OSError, asyncio.CancelledError) as error:
            return {"outcome": "indeterminate", "id": ident, "reason": str(error) or "interrupted"}
        finally:
            self._answering.discard(ident)
        self._remove_request(ident)
        self._status()
        self._publish({"type": "answer_dispatched", "id": ident})
        return {"outcome": "dispatched", "id": ident}

    def _remove_request(self, ident):
        self._facts["requests"] = [r for r in self._facts["requests"] if r["id"] != ident]
        handle = self._dialog_timeouts.pop(ident, None)
        if handle is not None:
            handle.cancel()

    def _expire_request(self, ident):
        # Pi's dialog timeout auto-resolves natively; this mirrors its declared
        # deadline, not an acknowledgment that an answer was consumed.
        self._remove_request(ident)
        self._status()
        self._publish({"type": "dialog_timeout", "id": ident})

    def _status(self):
        if not self._facts["available"]:
            self._facts["status"] = "unknown"
        elif self._facts["requests"]:
            self._facts["status"] = "blocked"
        elif self._facts["run"]["active"]:
            self._facts["status"] = "working"
        elif self._facts["run"]["settled"]:
            self._facts["status"] = "done"
        elif self._facts["run"]["active"] is False:
            self._facts["status"] = "idle"
        else:
            self._facts["status"] = "unknown"

    def _state_response(self, command, data):
        if command == "get_state" and isinstance(data, dict):
            if not all(isinstance(data.get(k), bool) for k in ("isStreaming", "isCompacting")):
                raise ValueError("invalid get_state activity")
            active = data["isStreaming"] or data["isCompacting"]
            self._facts["run"]["active"] = active
            if active:
                self._facts["run"].update(settled=False, aborted=None)
            self._facts["session"] = {k: data[k] for k in ("sessionId", "sessionFile", "sessionName") if k in data}
            # A metadata idle observation alone is not agent_settled.
            self._status()

    async def _read_stdout(self):
        try:
            while True:
                # Only LF is a boundary; U+2028/U+2029 are ordinary UTF-8 bytes.
                wire = await self.process.stdout.readuntil(b"\n")
                if len(wire) > self.limits.record_bytes:
                    raise ValueError("Pi record exceeds byte limit")
                record = json.loads(wire, parse_constant=lambda x: (_ for _ in ()).throw(ValueError(x)))
                if not isinstance(record, dict) or not isinstance(record.get("type"), str):
                    raise ValueError("Pi record is not a typed object")
                self._record(record)
        except asyncio.IncompleteReadError as error:
            self._disconnect("truncated stdout record" if error.partial else "stdout EOF")
        except (ValueError, UnicodeError, RecursionError, asyncio.LimitOverrunError) as error:
            self._disconnect(f"protocol error: {error}")
            self._signal_child()

    def _signal_child(self, kill=False):
        if self.process.returncode is None:
            try:
                (self.process.kill if kill else self.process.terminate)()
            except ProcessLookupError:
                pass  # It exited between the returncode check and the signal.

    def _record(self, record):
        kind = record["type"]
        if kind == "response":
            ident = record.get("id")
            pending = self._waiting.get(ident) if isinstance(ident, str) else None
            if pending is None:
                self._publish({"type": "late_response", "response": record})
                return
            command, future = pending
            if record.get("command") != command or not isinstance(record.get("success"), bool):
                raise ValueError("mismatched command response")
            if future.done():
                self._publish({"type": "late_response", "response": record})
                return
            result = {"id": ident, "outcome": "accepted" if record["success"] else "rejected"}
            if "data" in record:
                result["data"] = record["data"]
            if "error" in record:
                result["error"] = record["error"]
            if record["success"]:
                self._state_response(command, record.get("data"))
            future.set_result(result)
        elif kind == "agent_start":
            self._facts["run"] = {"active": True, "settled": False, "aborted": None}
        elif kind == "agent_settled":
            self._facts["run"] = {"active": False, "settled": True, "aborted": record.get("aborted")}
        elif kind == "queue_update":
            if not isinstance(record.get("steering"), list) or not isinstance(record.get("followUp"), list):
                raise ValueError("invalid queue update")
            self._facts["queues"] = {"steering": record["steering"], "follow_up": record["followUp"]}
        elif kind == "extension_ui_request":
            method = record.get("method")
            if not isinstance(method, str):
                raise ValueError("invalid extension UI method")
            if method not in NOTICES:
                ident = record.get("id")
                if not isinstance(ident, str):
                    raise ValueError("dialog missing id")
                if len(self._facts["requests"]) >= self.limits.requests:
                    raise ValueError("dialog request limit exceeded")
                if any(r["id"] == ident for r in self._facts["requests"]):
                    raise ValueError("duplicate dialog id")
                self._facts["requests"].append({"id": ident, "kind": method,
                    "prompt": record.get("title", ""), "data": record})
                timeout = record.get("timeout", 0)
                if not isinstance(timeout, (int, float)) or not math.isfinite(timeout) or timeout < 0:
                    raise ValueError("invalid dialog timeout")
                if timeout:
                    self._dialog_timeouts[ident] = asyncio.get_running_loop().call_later(
                        timeout / 1000, self._expire_request, ident)
                if method not in self.supported_dialogs:
                    task = asyncio.create_task(self.answer({"id": ident, "cancelled": True}))
                    self._ui_tasks.add(task)
                    task.add_done_callback(self._ui_tasks.discard)
        # agent_end is deliberately not settled; deltas and future types stay opaque.
        self._status()
        self._publish(record)

    async def _read_stderr(self):
        while True:
            data = await self.process.stderr.read(4096)
            if not data:
                return
            self._stderr_total += len(data)
            self._stderr_tail.extend(data)
            del self._stderr_tail[:-self.limits.stderr_bytes]

    async def _watch(self):
        code = await self.process.wait()
        await asyncio.gather(self._stdout, self._stderr)
        self._facts["exit_code"] = code
        self._disconnect(f"child exited: {code}")
        self._publish({"type": "child_exit", "code": code})

    async def wait_closed(self):
        await asyncio.shield(self._watcher)

    async def close(self, grace=1):
        """Request EOF shutdown, then bound cleanup of only this owned child."""
        self._closing = True
        self.process.stdin.close()
        try:
            await asyncio.wait_for(asyncio.shield(self._watcher), grace)
        except TimeoutError:
            self._signal_child()
            try:
                await asyncio.wait_for(asyncio.shield(self._watcher), grace)
            except TimeoutError:
                self._signal_child(kill=True)
                await self._watcher
        if self._ui_tasks:
            await asyncio.gather(*list(self._ui_tasks), return_exceptions=True)

    async def __aenter__(self):
        return self

    async def __aexit__(self, *_):
        await self.close()
