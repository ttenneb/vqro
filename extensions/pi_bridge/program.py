"""Pi RPC child + runtime provider, intended as a backend terminal's program."""
import asyncio
import json
import math
import os
from pathlib import Path
import re
import random
import uuid

from vqro_provider import Limits as ProviderLimits, Provider, ProviderError
from . import control
from .client import DIALOGS, EFFECTS, PiRpcClient
from .display import Fallback, Transcript, agent_data, plain, tail

CAPABILITIES = {
    "agent.prompt": {"while_running": "needs_mode"},
    "agent.steer": {"applies": "after_current_tool_calls"},
    "agent.follow_up": {"applies": "when_idle"},
    "agent.abort": {"clears_queue": False, "waits_for_idle": True},
    "agent.clear_queue": {"returns": "texts"},
    "agent.answer": {"kinds": sorted(DIALOGS)},
    "agent.correlation": {"per_input": False},
    # Agent standard v0 section 6.3: what each resource supports is in data.agent.commands / .config.
    "agent.command": {"v": 0, "declared_in": "data.agent.commands", "args": "string"},
    "agent.configure": {"v": 0, "declared_in": "data.agent.config", "observed": "get_state"},
}
OPS = sorted(EFFECTS | {"command", "configure"})


class Bridge:
    def __init__(self, argv, *, cwd, env, token, endpoint=None, sock=None,
                 provider=None, fallback=None, interval=.2, transcript_bytes=16384,
                 pi_limits=None, connector=None, retry_initial=.25, retry_max=30):
        if not argv:
            raise ValueError("Pi argv is required; no installed runtime is selected implicitly")
        if not isinstance(token, str) or not token:
            raise ValueError("VQRO_BRIDGE_TOKEN is required")
        if not isinstance(interval, (int, float)) or not math.isfinite(interval) or interval < .1:
            raise ValueError("publication interval must be finite and at least 0.1 seconds")
        self.argv, self.cwd = list(argv), cwd
        # Pi/tools receive only the terminal endpoint, not bridge/supervisor credentials or
        # core configuration. Keep the marker so an agent's scratch VQRO_STATE_DIR wins.
        self.env = {k: v for k, v in env.items() if not k.startswith("HERDR_") and
                    (not k.startswith("VQRO_") or k == "VQRO_SOCKET")}
        if "VQRO_SOCKET" in self.env:
            self.env["VQRO_SOCKET_SOURCE"] = "terminal"
        # Legacy/custom launchers may still use PYTHONPATH for the bridge. Remove only this
        # installation's import root, preserving the user's unrelated path entries verbatim.
        if "PYTHONPATH" in self.env:
            installed=Path(__file__).resolve().parent.parent
            paths=[]
            for entry in self.env["PYTHONPATH"].split(os.pathsep):
                try:
                    is_bridge=(Path(cwd or ".")/entry).resolve()==installed
                except (OSError,RuntimeError):
                    is_bridge=False  # preserve unrelated/unresolvable user entries
                if not is_bridge:paths.append(entry)
            if paths:self.env["PYTHONPATH"]=os.pathsep.join(paths)
            else:self.env.pop("PYTHONPATH")
        instance = str(uuid.uuid4())
        self.provider = provider or Provider("pi-" + instance, instance, CAPABILITIES, role="runtime", token=token,
                                            software={"name": "pi-rpc", "version": "unknown"},
                                            limits=ProviderLimits(identities=2048))
        if not all(isinstance(v, (int, float)) and math.isfinite(v) and v > 0 for v in (retry_initial, retry_max)) or retry_max < retry_initial:
            raise ValueError("reconnect backoff must be positive and bounded")
        self.endpoint, self.sock = endpoint, sock
        self.connector = connector
        self.retry_initial, self.retry_max = retry_initial, retry_max
        self._credential = token
        self._template = {"name": self.provider.name, "instance": self.provider.instance,
                          "capabilities": self.provider.capabilities, "role": "runtime",
                          "software": self.provider.software, "provider_version": self.provider.provider_version,
                          "limits": self.provider.limits}
        self.connections = 0
        self.view_problem = None
        self._publish_lock = asyncio.Lock()
        self._publish_now = asyncio.Event()
        self._control_task = None
        self._publishing = None
        # Connection setup (register, sync, views) and shutdown retirement never interleave: shutdown
        # waits for setup to reach a checkpoint, and setup publishes nothing once closing.
        self._setup = asyncio.Lock()
        self.retirement_problem = None  # why resource.gone could not be delivered, if it could not
        self.fallback = fallback or Fallback()
        self.interval, self.pi_limits = interval, pi_limits
        self.transcript = Transcript(transcript_bytes)
        self.ready, self.started = asyncio.Event(), asyncio.Event()
        self.pi = None
        self.rid = None
        self.view = self.status_view = None
        self.cursor = 0
        self.connected = False
        self.closing = False
        self._facts = self._content = self._status_content = self._fallback_frame = None
        self._tasks = set()
        self.failure = None
        self._cancel_attempts = set()
        self.declared = control.Declarations()  # agent v0 6.3 config/commands; unknown until Pi answers
        for effect in OPS:
            self.provider.effect("agent." + effect)(self._effect)

    def _task(self, coroutine):
        task = asyncio.create_task(coroutine)
        self._tasks.add(task)
        return task

    def facts(self):
        snapshot = self.pi.snapshot()
        return {"ref": "agent", "incarnation": "", "kind": "agent", "state": "live",
                "name": "Pi", "title": "Pi RPC", "cwd": str(self.cwd),
                "status": {"state": snapshot["status"]}, "data": {**agent_data(snapshot),
                    # Agent standard v0 section 6.3 lists (the rest of data.agent is not adopted yet).
                    "agent": self.declared.facts(),
                    "views": [view for view in (self.status_view, self.view) if view is not None],
                    **({"view_error": self.view_problem} if self.view_problem else {})}}

    async def _effect(self, effect, args):
        if self.closing or self.pi is None:
            raise ProviderError("unavailable", "bridge is closing")
        if effect.ref != "agent" or effect.incarnation != "":
            raise ProviderError("not_found", "no matching Pi agent")
        if effect.remaining <= 0:
            raise ProviderError("backend", "deadline expired before runtime dispatch")
        op = effect.op.removeprefix("agent.")
        if op in ("command", "configure"):
            try:
                return await self._settings_effect(op, args, effect.remaining)
            finally:
                self._publish_now.set()
        if op in ("prompt", "steer", "follow_up") and isinstance(args, dict) and self.declared.expands(args.get("message")):
            # Pi expands /templates, /skill:… and extension commands inside sends; a send stays literal (6.3).
            return {"outcome": "rejected", "error": {"kind": "unsupported",
                    "message": "Pi would run this text as a command; nothing sent. Use agent.command for advertised commands."}}
        try:
            result = await self.pi.invoke(op, args, timeout=effect.remaining)
        finally:
            # User controls should not wait for the transcript sampling timer.
            # This only wakes publication; effects are still sent exactly once.
            self._publish_now.set()
        outcome = result["outcome"]
        if outcome == "indeterminate":
            raise ProviderError("backend", result.get("reason", "runtime outcome unknown"))
        if outcome == "rejected":
            error = result.get("error", result.get("reason", "Pi rejected input"))
            return {"outcome": "rejected", "error": {"kind": "backend", "message": str(error)}}
        reply = {"outcome": outcome}  # includes dispatched: no acknowledgment for UI answers
        if "data" in result:
            reply["data"] = result["data"]
        return reply

    async def _settings_effect(self, op, args, remaining):
        """agent.command / agent.configure: only declared names and values, sent once; then
        re-read Pi's state so facts show what Pi reports (observed), never what was asked."""
        args = args if isinstance(args, dict) else {}
        native, native_args = self.declared.configure(args) if op == "configure" else self.declared.command(args)
        if native in ("unsupported", "stale"):
            return {"outcome": "rejected", "error": {"kind": native, "message": native_args}}
        result = await self.pi.request(native, native_args, timeout=remaining)
        if result["outcome"] == "indeterminate":
            raise ProviderError("backend", result.get("reason", "runtime outcome unknown"))
        if result["outcome"] == "rejected":
            return {"outcome": "rejected", "error": {"kind": "backend", "message": str(result.get("error", result.get("reason", "Pi rejected the command")))}}
        data = result.get("data") if isinstance(result.get("data"), dict) else {}
        if native == "new_session" and data.get("cancelled") is True:
            return {"outcome": "rejected", "error": {"kind": "backend", "message": "a Pi extension cancelled the new session"}}
        options, commands = await control.read(self.pi)
        self.declared.update(options, commands)
        if op == "configure" and options is not None and self.declared.current(args["id"]) == args["value"]:
            return {"outcome": "observed", "data": {"id": args["id"], "value": args["value"]}}
        if native == "compact":
            data = {k: data[k] for k in ("tokensBefore", "estimatedTokensAfter") if k in data}
        elif native in ("set_model", "cycle_model"):
            data = {"model": self.declared.current("model")}
        elif native == "prompt":
            data = {k: data[k] for k in ("disposition",) if k in data}
        return {"outcome": "accepted", **({"data": data} if data else {})}

    async def _read_config(self):
        self.declared.update(*await control.read(self.pi))
        self._publish_now.set()

    def _remember(self, provider):
        registration = provider.registration
        if registration:
            token = registration.get("resume_token")
            self._credential = token if isinstance(token, str) and token else None

    async def _connect(self, provider):
        async with self._setup:
            if self.closing:
                return  # shutdown began before registering: nothing to publish or retire here
            await self._setup_connection(provider)

    async def _setup_connection(self, provider):
        if self.connector is not None:
            await self.connector(provider)
        else:
            sock, self.sock = self.sock, None
            await provider.connect(self.endpoint, sock=sock)
        self._remember(provider)
        if self._credential is None:
            raise ProviderError("backend", "core omitted required resume_token; cannot resume")
        if self.closing:
            # Registered but not yet synced: never publish a live agent now. Control keeps this
            # connection open (wait_closed) so shutdown can retire the agent on it.
            return
        self.view_problem = None
        initial = self.facts()
        await provider.full_sync([initial])
        if self.closing:
            return  # synced; shutdown retires it as soon as setup releases
        scoped = await provider.call("resource.list", {"provider": provider.name, "backend_ref": "agent"})
        resources = [r for r in scoped.get("resources", [])
                     if r.get("provider") == provider.name and r.get("backend_ref") == "agent" and r.get("kind") == "agent"]
        if len(resources) != 1 or not isinstance(resources[0].get("id"), str) or not re.fullmatch(r"r[0-9]+", resources[0]["id"]):
            raise ProviderError("backend", "acknowledged sync did not publish exactly one agent resource")
        self.rid = resources[0]["id"]
        self.view = await self._declare_view(provider, "agent-" + self.rid)
        self.status_view = await self._declare_view(provider, "agent-status-" + self.rid)
        async with self._publish_lock:
            self._facts, self._content, self._status_content = initial, None, None
            self.connected = True
        self.connections += 1
        self.failure = None
        await self.publish()
        if not self.connected:
            raise ProviderError("unavailable", "control disconnected during initial publication")
        self.ready.set()

    async def _declare_view(self, provider, view):
        marker = {"instance": provider.instance, "ref": "agent"}
        accepted = None
        try:
            # A provider has a 1 MiB wire cap. Never fetch the whole workspace
            # just to locate one resource and inspect its one owned view.
            entry = await provider.call("workspace.get", {"key": "view/" + view})
            if entry is None:
                try:
                    await provider.call("workspace.apply", {"note": "Declare Pi agent fallback view", "ops": [{
                        "op": "set", "key": "view/" + view, "expect": 0,
                        "value": {"kind": "panel", "title": "Pi " + self.rid, "bridge": marker,
                                  "items": [{"type": "text", "text": "Pi bridge not connected; use its backend terminal."}]}}]})
                    accepted = view
                except ProviderError as error:
                    if error.kind != "conflict":
                        raise
                    entry = await provider.call("workspace.get", {"key": "view/" + view})
            if entry is not None:
                value = entry.get("value") if isinstance(entry, dict) else None
                if isinstance(value, dict) and value.get("kind") == "panel" and value.get("bridge") == marker:
                    accepted = view
                else:
                    self.view_problem = "Agent view belongs to someone else; not overwritten or published."
            elif accepted is None:
                self.view_problem = "Agent view changed during declaration; not overwritten or published."
        except ProviderError as error:
            if error.kind not in ("too_large", "forbidden", "unknown_method", "unsupported"):
                raise
            # An oversized/foreign view is not a reason to reconnect and sync
            # repeatedly. Resource controls and the backend terminal still work.
            self.view_problem = "Agent view unavailable (" + str(error.kind) + "): " + tail(plain(error), 512)
        return accepted

    def _retry_delay(self, backoff):
        # Equal jitter retains a nonzero floor and spreads even capped retries.
        return random.uniform(backoff / 2, backoff)

    async def _control(self):
        provider = self.provider
        backoff = self.retry_initial
        while not self.closing:
            self.provider = provider
            self.connected = False
            self.ready.clear()
            connected_at = None
            try:
                await self._connect(provider)
                connected_at = asyncio.get_running_loop().time()
                await provider.wait_closed()
            except asyncio.CancelledError:
                raise
            except Exception as error:
                self.failure = tail(plain(error), 1024)
                safe_mode = isinstance(error, ProviderError) and error.remote and error.kind == "forbidden" and error.data.get("reason") == "safe_mode"
                if isinstance(error, ProviderError) and provider.generation is None and not safe_mode and (error.remote or error.kind not in ("unavailable", "timeout")):
                    # A definitive refusal cannot become an unreachable, immortal
                    # Pi process. Exit even for overload: the user may launch again
                    # after recovery; we never replay input or guess token reuse.
                    self.fallback.show("Pi bridge registration refused: " + self.failure + "; stopping runtime.")
                    raise
            finally:
                self._remember(provider)  # an observed rotated token survives later setup failure
                self.connected = False
                self.ready.clear()
                self._publish_now.set()  # connection diagnostics changed even if Pi is idle
                await provider.close()
            if self._credential is None:
                raise ProviderError("forbidden", "registration returned no resume credential; stopping runtime rather than keeping an unreachable Pi")
            # A briefly accepted, flapping connection must not reset the backoff.
            if connected_at is not None and asyncio.get_running_loop().time() - connected_at >= 30:
                backoff = self.retry_initial
            # Transport gaps and explicit temporary safe-mode admission retry
            # the last observed credential. No input/effect is replayed; a lost
            # registration reply uses the core's credential grace/rotation.
            await asyncio.sleep(self._retry_delay(backoff))
            backoff = min(self.retry_max, backoff * 2)
            provider = Provider(**self._template, token=self._credential)
            for effect in OPS:
                provider.effect("agent." + effect)(self._effect)

    def _events(self):
        batch = self.pi.events(self.cursor)
        self.cursor = batch["cursor"]
        if batch["gap"]:
            self.transcript.gap()
        for event in batch["events"]:
            self.transcript.feed(event)

    def content(self, part=None):
        snapshot = self.pi.snapshot()
        rows, clipped, lost = self.transcript.window(styled=True)
        run = snapshot["run"]
        status = plain(snapshot["status"])
        style = {"working": "accent", "blocked": "warn", "done": "ok", "error": "error"}.get(status, "dim")
        description = ("Failed" if status == "error" else "Retrying" if (snapshot.get("retry") or {}).get("active") else
                       "Stopped (aborted)" if run["aborted"] else "Finished" if run["settled"] else
                       "Running" if run["active"] else "Ready for a prompt" if run["active"] is False else "Waiting for runtime status")
        items = [{"text": "Pi", "badges": [{"text": status, "style": style}], "description": description}]
        queues = snapshot["queues"]
        counts = [("Queued steering", len(queues["steering"])), ("follow-up", len(queues["follow_up"])),
                  ("pending requests", len(snapshot["requests"]))]
        if any(count for _, count in counts):
            items.append({"type": "text", "text": " | ".join(f"{name}: {count}" for name, count in counts if count)})
        for kind, values in queues.items():
            for value in values[:3]:
                items.append({"type": "text", "style": "dim", "text": tail(plain(kind+": "+value), 512)})
            if len(values) > 3:
                items.append({"type": "text", "style": "dim", "text": f"{kind}: more queued messages omitted"})
        if clipped or lost:
            items.append({"type": "text", "style": "warn", "text": "Bounded transcript tail; some earlier output is unavailable."})
        request_bytes = 0
        for request in snapshot["requests"]:
            detail = request.get("data", {})
            label = f"Request {request['id']} ({request['kind']}): {request['prompt']}"
            if detail.get("message"):
                label += " - " + str(detail["message"])
            if detail.get("options"):
                label += " | choices: " + str(detail["options"])
            label = tail(plain(label), 1024)
            request_bytes += len(label.encode())
            if request_bytes > 8192:
                items.append({"type": "text", "style": "warn", "text": "Additional request details omitted; inspect the pending request facts."})
                break
            items.append({"type": "text", "style": "warn", "text": label})
        transcript_start = len(items)
        items.extend({"type": "text", **row, "text": tail(row["text"], 2048)} for row in rows)
        if part == "status":
            items = items[:transcript_start]
            transcript_start = len(items)
        elif part == "output":
            items = items[transcript_start:]
            transcript_start = 0
        result = {"title": "Pi " + (self.rid or "starting"), "requires": ["badges", "description"], "items": items,
                  "truncated": clipped, "output_gap": lost}
        if len(json.dumps(result, ensure_ascii=False).encode()) > 32768:
            items.insert(transcript_start, {"type": "text", "style": "warn", "text": "Bounded view: older transcript rows omitted."})
            transcript_start += 1
            while len(json.dumps(result, ensure_ascii=False).encode()) > 32768 and len(items) > transcript_start:
                del items[transcript_start]
                result["truncated"] = True
        return result

    def _show(self):
        snapshot = self.pi.snapshot()
        rows, _, _ = self.transcript.window()
        frame = f"Pi {self.rid or 'starting'} | {snapshot['status']} | core {'connected' if self.connected else 'unavailable'}"
        frame += "\n" + "\n".join(rows[-2:])
        if snapshot["requests"]:
            request = snapshot["requests"][0]
            frame += "\nRequest " + tail(plain(f"{request['id']} {request['kind']}: {request['prompt']}"), 1024)
        if self.view_problem or self.failure:
            frame += "\n" + (self.view_problem or self.failure)
        if frame != self._fallback_frame:
            self._fallback_frame = frame
            self.fallback.show(frame)

    async def publish(self, final=False):
        self._events()
        # Unrepresentable dialogs must not leave the runtime invisibly blocked.
        observed = self.pi.snapshot()
        data = agent_data(observed)
        self._cancel_attempts.intersection_update(r["id"] for r in observed["requests"])
        if data.get("requests") is None and observed["available"]:
            for request in observed["requests"]:
                if request["id"] not in self._cancel_attempts:
                    self._cancel_attempts.add(request["id"])
                    await self.pi.invoke("answer", {"id": request["id"], "cancelled": True})
                    self.transcript.append("Oversized dialog cancellation dispatched/attempted; no automatic resend.")
        provider = self.provider
        async with self._publish_lock:
            if provider is self.provider and provider.failure is not None:
                self.connected = False
            if provider is self.provider and self.connected:
                try:
                    current = self.facts()
                    # Once closing, facts are never upserted again: that could re-create the agent
                    # after its resource.gone.
                    if not final and not self.closing and current != self._facts:
                        await provider.upsert(current)
                        if provider is self.provider:
                            self._facts = current
                    for view, part, cache in ((self.view, "output", "_content"), (self.status_view, "status", "_status_content")):
                        content = self.content(part)
                        if view is not None and content != getattr(self, cache):
                            await provider.call("view.publish", {"view": view, "content": content}, timeout=1 if final else None)
                            if provider is self.provider:
                                setattr(self, cache, content)
                except ProviderError as error:
                    if provider is self.provider:
                        self.connected = False
                        self.failure = tail(plain(error), 1024)
                    await provider.close()
        self._show()

    async def _publish_loop(self):
        loop=asyncio.get_running_loop()
        next_sample=0
        while True:
            changed=asyncio.create_task(self.pi.wait_changed(self.cursor))
            immediate=asyncio.create_task(self._publish_now.wait())
            try:
                await asyncio.wait({changed,immediate},return_when=asyncio.FIRST_COMPLETED)
                # Only dirty runtime output needs sampling/coalescing. User controls and
                # connection diagnostics remain immediate, as before; idle has no timer.
                if not immediate.done():
                    remaining=next_sample-loop.time()
                    if remaining>0:
                        await asyncio.wait({immediate},timeout=remaining)
                self._publish_now.clear()
                await self.publish()
                next_sample=loop.time()+self.interval
            finally:
                for task in (changed,immediate):
                    task.cancel()
                await asyncio.gather(changed,immediate,return_exceptions=True)

    async def _retire(self, why):
        """Send resource.gone for the agent; retry once, then report rather than swallow a loss.

        Retirement is idempotent (it names an identity, it is not runtime input), so one retry
        after a timeout is safe. A loss leaves the agent cached as unavailable in vqro, so it is
        made visible on the fallback screen and returned to the caller."""
        try:
            # Deliberately shorter than one setup call's deadline (30 s by default): shutdown stays bounded on a
            # stalled core. Setup stops at its next checkpoint once closing; if it is still inside one call after
            # 5 s, retirement is reported as not delivered and the agent stays cached as unavailable (honest).
            await asyncio.wait_for(self._setup.acquire(), 5)
        except TimeoutError:
            problem, unknown = "connection setup still in progress", False
        else:
            try:
                problem, unknown = await self._send_gone(why)
            finally:
                self._setup.release()
        if problem is None:
            return None
        verdict = "delivery unconfirmed" if unknown else "not delivered"
        self.retirement_problem = tail(plain(f"agent retirement (resource.gone) {verdict}: " + problem), 1024)
        self.failure = self.retirement_problem if not self.failure else self.failure + "; " + self.retirement_problem
        return self.retirement_problem

    async def _send_gone(self, why):
        """(problem, unknown): problem is None once a gone was written. A registered provider
        retires even before its UI-ready `connected` (setup stopped at a closing checkpoint)."""
        provider = self.provider
        if provider.generation is None:
            if self.rid is None and not self.connections:
                return None, False  # this bridge never published an agent
            return "not registered with the core", False
        problem, unknown = None, False
        for _ in range(2):
            if provider.failure is not None:
                return "provider disconnected: " + str(provider.failure), unknown
            try:
                await asyncio.wait_for(provider.gone("agent", why=why), 1)
                return None, False  # written; the core's processing is not acknowledged
            except (TimeoutError, ProviderError) as error:
                # A timed-out write may already have reached the core: unknown, never "not sent".
                problem, unknown = str(error) or type(error).__name__, True
        return problem, unknown

    async def run(self, stop=None):
        stop = stop or asyncio.Event()
        why = {"reason": "closed"}
        try:
            self.pi = await PiRpcClient.start(self.argv, cwd=self.cwd, env=self.env, limits=self.pi_limits)
            self.started.set()
            ended = self._task(self.pi.wait_closed())
            stopping = self._task(stop.wait())
            metadata = self._task(self.pi.request("get_state"))
            done, _ = await asyncio.wait({metadata, ended, stopping}, return_when=asyncio.FIRST_COMPLETED)
            if metadata in done:
                state = await metadata
                if state["outcome"] != "accepted":
                    raise ProviderError("backend", "Pi did not accept startup metadata request")
                self._task(self._read_config())  # registration never waits for model lists
                self._control_task = self._task(self._control())
                publishing = self._publishing = self._task(self._publish_loop())
                done, _ = await asyncio.wait({publishing, self._control_task, ended, stopping}, return_when=asyncio.FIRST_COMPLETED)
                if publishing in done:
                    await publishing
                if self._control_task in done:
                    await self._control_task
            if ended in done:
                code = self.pi.process.returncode
                why = {"reason": "exited", "exit_code": code}
            else:
                why = {"reason": "closed", "message": "bridge stdin/lifetime EOF or explicit stop"}
        except asyncio.CancelledError:
            why = {"reason": "closed", "message": "bridge cancelled"}
            raise
        except Exception as error:
            self.failure = f"bridge failure: {error}"
            why = {"reason": "closed", "message": tail(plain(self.failure), 1024)}
            raise
        finally:
            self.closing = True
            # The publisher may be mid-write; it keeps running until the agent is retired, so
            # the retirement is never queued behind (or lost to) a cancelled publication.
            finishing = [task for task in self._tasks if task not in (self._control_task, self._publishing)]
            for task in finishing:
                task.cancel()
            await asyncio.gather(*finishing, return_exceptions=True)
            if self.pi is not None:
                await self.pi.close(grace=.5)
            await self._retire(why)
            if self._publishing is not None:
                self._publishing.cancel()
                await asyncio.gather(self._publishing, return_exceptions=True)
            if self.pi is not None and self.view is not None and self.connected:
                try:
                    await asyncio.wait_for(self.publish(final=True), 1.5)
                except (TimeoutError, ProviderError):
                    pass
            if self._control_task is not None:
                self._control_task.cancel()
                await asyncio.gather(self._control_task, return_exceptions=True)
            await self.provider.close()
            self.connected = False
            self.fallback.show(f"Pi bridge stopped | {why['reason']} | {self.failure or ''}")
            self.fallback.close()
        return why
