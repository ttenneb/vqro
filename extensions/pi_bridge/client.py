"""Bounded, stdlib-only asynchronous client for Pi 1.5's JSONL RPC (not JSON-RPC)."""
import asyncio
from collections import deque
from copy import deepcopy
from dataclasses import dataclass
import json
import math

from vqro_display import label


@dataclass(frozen=True)
class Limits:
    record_bytes: int = 262144
    pending: int = 32
    event_bytes: int = 524288
    event_rows: int = 512  # Wire bytes alone undercount Python dictionaries/strings for tiny events.
    stderr_bytes: int = 65536
    requests: int = 32
    deadline: float = 5.0
    # After the owned child exits, how long stdout/stderr may keep draining. A descendant that
    # inherited them can hold them open forever; past this, the pipes are abandoned.
    drain: float = 1.0
    # How long a started prompt may take to show agent_start before the start gate opens anyway
    # (journaled as start_unconfirmed). Pi0.87 loses one of two overlapping start preflights.
    start_confirm: float = 30.0

    def __post_init__(self):
        for name in ("record_bytes", "pending", "event_bytes", "event_rows", "stderr_bytes", "requests"):
            if not isinstance(getattr(self, name), int) or getattr(self, name) <= 0:
                raise ValueError(f"{name} must be positive")
        for name in ("deadline", "drain", "start_confirm"):
            value = getattr(self, name)
            if not isinstance(value, (int, float)) or not math.isfinite(value) or value <= 0:
                raise ValueError(f"{name} must be finite and positive")


DIALOGS = frozenset({"select", "confirm", "input", "editor"})
NOTICES = frozenset({"notify", "setStatus", "setWidget", "setTitle", "set_editor_text"})
EFFECTS = frozenset({"prompt", "steer", "follow_up", "abort", "clear_queue", "answer"})


class _ExitProtocol(asyncio.subprocess.SubprocessStreamProtocol):
    """Keep asyncio's bounded streams/flow control, but expose owned exit before pipe EOF.

    Process.wait() can wait forever on a descendant's inherited pipe. The subprocess protocol's
    cross-platform process_exited callback is independent of that pipe lifetime; no PID polling.
    """
    def __init__(self, limit, loop):
        super().__init__(limit, loop)
        self.owned_exit = asyncio.Event()

    def process_exited(self):
        self.owned_exit.set()
        super().process_exited()


class PiRpcClient:
    """One child, one event loop, no automatic retries or reconnects.

    Construct with ``await PiRpcClient.start(argv, cwd=..., env={...})`` inside
    the backend-owned bridge, never in vqro core. ``request`` is the metadata
    escape hatch; ``invoke`` limits the public effects to native Pi commands.
    """

    @classmethod
    async def start(cls, argv, *, cwd, env, limits=None, supported_dialogs=DIALOGS):
        self = cls(limits or Limits(), supported_dialogs)
        loop=asyncio.get_running_loop()
        transport,protocol=await loop.subprocess_exec(
            lambda: _ExitProtocol(self.limits.record_bytes+1,loop),
            *argv, cwd=cwd, env=dict(env), stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE,
        )
        self.process=asyncio.subprocess.Process(transport,protocol,loop)
        self._owned_exited=protocol.owned_exit
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
            "failure": None, "retry": None,
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
        self._abandon = asyncio.Event()  # close() gave up waiting for inherited pipes
        # Start gate (P4): Pi has no atomic start guard; two overlapping prompt preflights can both
        # ACK success while only one reaches the model, with a spurious settle. One prompt at a time
        # until Pi shows it started (or answered handled/queued/rejected); later ones wait in order.
        self._start_gate = asyncio.Lock()  # FIFO for waiters
        self._starts = 0           # agent_start events seen
        self._start_pending = False  # a prompt is in native preflight or not yet confirmed
        self._aborts = 0

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
                "event_bytes": self._event_bytes, "event_rows": len(self._events), "pending": len(self._waiting),
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

    async def wait_changed(self, after):
        """Wait for a journal revision without a timer; a stale/future cursor returns immediately."""
        while self._sequence == after:
            changed=self._changed
            await changed.wait()
        return self._sequence

    def _publish(self, event):
        self._sequence += 1
        row = {**event, "seq": self._sequence}
        size = len(json.dumps(row, ensure_ascii=True).encode())
        # Bound object count as well as encoded payload. Small JSON records otherwise retain
        # thousands of dictionaries/strings while still appearing far below the wire-byte cap.
        # Either limit causes the same explicit cursor gap; native effects are never replayed.
        while self._events and (self._event_bytes + size > self.limits.event_bytes or
                                len(self._events) >= self.limits.event_rows):
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
        """Native effects for a future provider layer. Does not connect to vqro."""
        if not isinstance(op, str) or op not in EFFECTS:
            return {"outcome": "rejected", "reason": "unsupported effect"}
        if op == "answer":
            return await self.answer(args or {}, timeout=timeout)
        if op == "prompt":
            return await self.prompt(args, timeout=timeout)
        if op == "abort":
            self._aborts += 1  # unsent queued prompts see it and are not sent
            unresolved = self._start_pending
            result = await self.request(op, args, timeout=timeout)
            if unresolved and result.get("outcome") == "accepted":
                # Measured on Pi0.87: abort does not cancel an unresolved earlier prompt preflight.
                result["data"] = {**(result.get("data") or {}), "start_unresolved": True}
            return result
        return await self.request(op, args, timeout=timeout)

    async def prompt(self, args, *, timeout=None):
        """`prompt` through the start gate: sent only after the previous prompt is confirmed.

        Waiting here sends nothing: if the deadline passes or an abort arrives first, the outcome is
        rejected (not sent), never indeterminate. Once sent, the result is Pi's own ACK.
        """
        try:
            deadline = self._timeout(timeout)
        except ValueError as error:
            return {"outcome": "rejected", "reason": str(error)}
        aborts = self._aborts
        loop = asyncio.get_running_loop()
        try:
            await asyncio.wait_for(self._start_gate.acquire(), max(0, deadline - loop.time()))
        except TimeoutError:
            return {"outcome": "rejected", "reason": "an earlier prompt has not started yet; not sent"}
        released = False
        try:
            if self._aborts != aborts:
                return {"outcome": "rejected", "reason": "aborted before dispatch; not sent"}
            if deadline - loop.time() <= 0:
                return {"outcome": "rejected", "reason": "deadline before dispatch; not sent"}
            starts = self._starts
            self._start_pending = True
            self._status()
            self._poke()
            result = await self.request("prompt", args, timeout=max(0.001, deadline - loop.time()))
            disposition = (result.get("data") or {}).get("disposition") if isinstance(result.get("data"), dict) else None
            if result.get("outcome") == "rejected" or disposition in ("handled", "queued") or self._starts > starts:
                return result  # nothing more to wait for
            # Started (or unknown): hold the gate, not the caller, until Pi shows agent_start.
            task = asyncio.create_task(self._confirm_start(starts))
            self._ui_tasks.add(task)
            task.add_done_callback(self._ui_tasks.discard)
            released = True
            return result
        finally:
            if not released:
                self._start_pending = False
                self._status()
                self._start_gate.release()
                self._poke()

    async def _confirm_start(self, starts):
        try:
            await asyncio.wait_for(self._wait_started(starts), self.limits.start_confirm)
        except TimeoutError:
            self._publish({"type": "start_unconfirmed", "reason": "no agent_start after a successful prompt ACK"})
        finally:
            self._start_pending = False
            self._status()
            self._start_gate.release()
            self._poke()

    def _poke(self):
        """Wake fact waiters (status changed without a journal row)."""
        self._changed.set()
        self._changed = asyncio.Event()

    async def _wait_started(self, starts):
        while self._starts == starts and self._facts["available"]:
            changed = self._changed
            await changed.wait()

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
        self._facts["start_pending"] = getattr(self, "_start_pending", False)
        if not self._facts["available"]:
            self._facts["status"] = "unknown"
        elif self._facts["start_pending"] and not self._facts["requests"]:
            # A prompt in native preflight: Pi may still report idle/settled, but it is not (P4).
            self._facts["status"] = "working"
        elif self._facts["requests"]:
            self._facts["status"] = "blocked"
        elif (self._facts.get("retry") or {}).get("active"):
            self._facts["status"] = "working"
        elif self._facts.get("failure"):
            self._facts["status"] = "error"
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
            self._starts += 1
            if not (self._facts.get("retry") or {}).get("active"):
                self._facts.update(failure=None, retry=None)
            self._facts["run"] = {"active": True, "settled": False, "aborted": None}
        elif kind == "message_end" and record.get("message", {}).get("role") == "assistant":
            message = record["message"]
            if message.get("stopReason") == "error":
                self._facts["failure"] = {"message": label(message.get("errorMessage") or "Runtime reported an assistant error (no details).", 4096)}
            elif message.get("stopReason") in ("stop", "length", "toolUse", "deferred"):
                self._facts["failure"] = None
        elif kind in ("auto_retry_start", "auto_retry_end"):
            retry = dict(self._facts.get("retry") or {}, active=kind == "auto_retry_start")
            for native, field in (("attempt", "attempt"), ("maxAttempts", "max_attempts"), ("delayMs", "delay_ms")):
                value = record.get(native)
                if type(value) is int and value >= (0 if native == "delayMs" else 1):
                    retry[field] = value
            if kind == "auto_retry_start" and record.get("errorMessage"):
                self._facts["failure"] = {"message": label(record["errorMessage"], 4096)}
            if kind == "auto_retry_end":
                retry["success"] = record.get("success") if type(record.get("success")) is bool else None
                if retry["success"] is True:
                    self._facts["failure"] = None
                elif retry["success"] is False:
                    self._facts["failure"] = {"message": label(record.get("finalError") or (self._facts.get("failure") or {}).get("message") or "Runtime retry failed (no details).", 4096)}
            self._facts["retry"] = retry
        elif kind == "agent_settled":
            self._facts["run"] = {"active": False, "settled": True, "aborted": record.get("aborted")}
            if self._facts.get("retry"):
                self._facts["retry"]["active"] = False
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

    async def _owned_exit(self):
        # The native subprocess exit callback is authoritative even if a descendant holds pipes.
        await self._owned_exited.wait()
        return self.process.returncode

    async def _watch(self):
        code = await self._owned_exit()
        # Exited: not available for new effects, whatever its pipes do.
        self._facts.update(available=False, stale=True, exit_code=code)
        self._status()
        # Records already written before the exit still count: drain, but only for a bounded
        # time (or until close() gives up), then abandon pipes a descendant keeps open.
        loop = asyncio.get_running_loop()
        deadline = loop.time() + self.limits.drain
        readers = {self._stdout, self._stderr}
        while not all(t.done() for t in readers) and not self._abandon.is_set():
            remaining = deadline - loop.time()
            if remaining <= 0:
                break
            abandon = asyncio.ensure_future(self._abandon.wait())
            try:
                await asyncio.wait({task for task in readers if not task.done()} | {abandon},
                                   timeout=remaining, return_when=asyncio.FIRST_COMPLETED)
            finally:
                abandon.cancel()
        abandoned = not all(t.done() for t in readers)
        if abandoned:
            for task in readers:
                task.cancel()
            await asyncio.gather(*readers, return_exceptions=True)
            # Stop reading the inherited pipes; never signal processes we don't own.
            transport = getattr(self.process, "_transport", None)
            if transport is not None:
                transport.close()
        self._disconnect(f"child exited: {code}")
        if abandoned:
            self._publish({"type": "pipes_abandoned", "reason": "a descendant kept stdout/stderr open after exit"})
        self._publish({"type": "child_exit", "code": code})

    async def wait_closed(self):
        await asyncio.shield(self._watcher)

    async def close(self, grace=1):
        """Request EOF shutdown, then bound cleanup of only this owned child."""
        self._closing = True
        self.process.stdin.close()
        for step in ("wait", "terminate", "kill"):
            if step == "terminate":
                self._signal_child()
            elif step == "kill":
                self._signal_child(kill=True)
            if self.process.returncode is not None and step != "wait":
                # Exited, but pipes still held by a descendant: stop waiting for them.
                self._abandon.set()
            try:
                await asyncio.wait_for(asyncio.shield(self._watcher), grace)
                break
            except TimeoutError:
                continue
        else:
            # Killed: the owned exit follows promptly; pipes are abandoned right after it.
            self._abandon.set()
            await self._watcher
        if self._ui_tasks:
            await asyncio.gather(*list(self._ui_tasks), return_exceptions=True)

    async def __aenter__(self):
        return self

    async def __aexit__(self, *_):
        await self.close()
