#!/usr/bin/env python3
"""Scripted native fixture. Never an agent implementation or model client.

Production-native records use stdin/stdout. Runtime-neutral timeline controls
and the effect oracle use a different private socket/file. No test RPC reaches
an adapter's production effect handlers.
"""
import argparse
import asyncio
import json
import os
from pathlib import Path
import sys
import time
import stat

MAX_RECORD = 1 << 20
MAX_LEDGER = 16 << 20
MODEL = {"id": "fixture-small", "name": "Fixture Small", "provider": "fixture", "api": "openai-completions",
         "reasoning": True, "input": ["text", "image"], "contextWindow": 8192, "maxTokens": 1024,
         "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0}}
READS = {"get_state", "get_commands", "get_available_models", "get_available_thinking_levels", "get_messages",
         "get_session_stats", "get_entries", "get_tree", "get_fork_messages", "get_last_assistant_text"}
OPERATIONS = {"prompt": "send", "steer": "send", "follow_up": "send", "abort": "interrupt",
              "clear_queue": "queue.clear", "extension_ui_response": "respond", "new_session": "session.new",
              "switch_session": "session.resume", "fork": "session.fork", "clone": "session.clone"}


class Runtime:
    def __init__(self, args):
        self.args = args
        self.active, self.session, self.serial = False, 1, 0
        self.native_turn = False
        self.steering, self.follow_up, self.entries = [], [], []
        self.steering_images, self.follow_up_images = [], []
        self.requests, self.barriers, self.drop = {}, {}, set()
        self.queue_modes = {"steering": "one-at-a-time", "follow_up": "one-at-a-time"}
        self.model, self.thinking = dict(MODEL), "off"
        self.compaction = True
        self.output_lock = asyncio.Lock()
        self.tasks = set()
        self.stop = asyncio.Event()
        self.exit_code, self.failure = 0, None
        self.ledger_bytes = args.ledger.stat().st_size if args.ledger.exists() else 0
        self.script = json.loads(args.script.read_text()) if args.script else {"script": []}
        self.script_actions = 0
        if not isinstance(self.script, dict) or len(json.dumps(self.script).encode()) > MAX_RECORD or len(self.script.get("script", [])) > 128:
            raise ValueError("script exceeds fixture bounds")
        if "rules" in self.script:
            raise ValueError("timeline uses conformance's script field, not a second rules format")
        for rule in self.script.get("script", []):
            if ("on" in rule) == ("after" in rule) or ("on" in rule and rule["on"] != "input"):
                raise ValueError("unimplemented fixture timeline trigger")
        self.record("ready", wire=args.wire, bootstrap_in_child="VQRO_BRIDGE_TOKEN" in os.environ,
                    herdr_in_child=any(k.startswith("HERDR_") for k in os.environ))

    def record(self, kind, **details):
        self.serial += 1
        row = {"seq": self.serial, "kind": kind, "ns": time.monotonic_ns(), "pid": os.getpid(), **details}
        data = json.dumps(row, ensure_ascii=False, separators=(",", ":")).encode() + b"\n"
        if self.ledger_bytes + len(data) > MAX_LEDGER:
            raise ValueError("native oracle ledger bound exceeded")
        fd = os.open(self.args.ledger, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600)
        try:
            os.write(fd, data)
            if kind in ("input_received", "inserted", "answer"):
                os.fsync(fd)
        finally:
            os.close(fd)
        self.ledger_bytes += len(data)
        return row

    async def emit(self, row):
        data = json.dumps(row, ensure_ascii=False, separators=(",", ":")).encode() + b"\n"
        if len(data) > MAX_RECORD:
            raise ValueError("native fixture output record exceeds bound")
        async with self.output_lock:
            # A stopped adapter reader must not freeze fixture control/cleanup.
            loop = asyncio.get_running_loop()
            while data:
                try:
                    count = os.write(1, data)
                    data = data[count:]
                except BlockingIOError:
                    ready = loop.create_future()
                    def writable():
                        if not ready.done():
                            ready.set_result(None)
                    loop.add_writer(1, writable)
                    try:
                        await ready
                    finally:
                        loop.remove_writer(1)

    async def reply(self, command, data=None, success=True):
        operation = OPERATIONS.get(command["type"], "query" if command["type"] in READS else "configure" if command["type"].startswith("set_") else "unknown")
        if command["type"] in self.drop or operation in self.drop:
            self.record("reply_dropped", command=command["type"], operation=operation, request=command.get("id"))
            return
        row = {"type": "response", "id": command.get("id"), "command": command["type"], "success": success}
        if data is not None:
            row["data"] = data
        self.record("native_reply", command=command["type"], request=command.get("id"), success=success)
        await self.emit(row)

    async def input_ack(self, command, disposition):
        # Weak success-only ACK is the P4 baseline. Newer Pi versions may add a
        # disposition; the scenario opts in rather than granting fake precision.
        await self.reply(command, {"disposition": disposition} if self.script.get("rich_disposition") else None)

    async def barrier(self, phase, when="before"):
        event = self.barriers.get((when, phase))
        if event is not None:
            self.record("paused", phase=phase, when=when)
            await event.wait()

    async def queue_update(self):
        await self.emit({"type": "queue_update", "steering": self.steering[:], "followUp": self.follow_up[:]})

    async def insert(self, text, source, images=None):
        await self.barrier("insertion")
        entry = {"id": f"entry-{len(self.entries) + 1}", "parentId": self.entries[-1]["id"] if self.entries else None,
                 "type": "message", "message": {"role": "user", "content": [{"type": "text", "text": text}] + (images or []),
                                                   "timestamp": int(time.time() * 1000)}}
        self.entries.append(entry)
        self.record("inserted", text=text, source=source, session=f"fixture-{self.session}", entry=entry["id"])
        await self.emit({"type": "message_start", "message": entry["message"]})
        await self.emit({"type": "message_end", "message": entry["message"]})
        await self.barrier("insertion", "after")

    async def rules(self, on, value):
        for rule in self.script.get("script", []):
            if (on == "after" and rule.get("after") != value.get("id")) or (on != "after" and rule.get("on") != on):
                continue
            match = rule.get("match", {})
            if set(match) - {"text", "contains"}:
                raise ValueError("unimplemented fixture match predicate")
            if "text" in match and value.get("text") != match["text"]:
                continue
            if "contains" in match and match["contains"] not in value.get("text", ""):
                continue
            for action in rule.get("do", []):
                await self.action(action)

    async def action(self, action):
        # One timeline syntax: the on/match/do + after notation in conformance.
        names = {"turn", "tool_call", "model_call", "request", "queue", "crash", "hang", "drop_reply",
                 "pause_before", "pause_after", "retain", "session_switch", "exit", "host_exit"}
        verbs = names.intersection(action)
        if len(verbs) != 1:
            raise ValueError("timeline action needs one runtime-neutral verb")
        self.script_actions += 1
        if self.script_actions > 1024:
            raise ValueError("fixture script work bound exceeded")
        verb = next(iter(verbs))
        arg = action[verb]
        params = dict(arg) if isinstance(arg, dict) else {}
        if verb == "turn":
            params["state"] = "settled" if arg == "end" and action.get("stop") else arg
        elif verb in ("tool_call", "model_call"):
            params["id_ref"] = arg
            if verb == "tool_call":
                params["state"] = "start"
        elif verb in ("pause_before", "pause_after") and isinstance(arg, str):
            params["phase"] = arg
        await self.control({"verb": verb, **params})
        if verb == "tool_call":
            milliseconds = action.get("hold_ms", 0)
            if not isinstance(milliseconds, (int, float)) or not 0 <= milliseconds <= 2000:
                raise ValueError("fixture tool hold bound")
            await asyncio.sleep(milliseconds / 1000)
            await self.control({"verb": "tool_call", "id_ref": arg, "state": "end"})
            await self.rules("after", {"id": arg})

    async def native(self, command):
        kind = command.get("type")
        if not isinstance(kind, str):
            raise ValueError("Pi command needs type")
        operation = "query" if kind in READS else "configure" if kind.startswith("set_") else OPERATIONS.get(kind, "unknown")
        self.record("command", command=kind, operation=operation, phase="preflight" if kind == "get_state" else None,
                    effectful=kind not in READS, value=command)
        if kind == "get_state":
            await self.barrier("preflight")
            snapshot = {"sessionId": f"fixture-{self.session}", "sessionFile": str(self.args.ledger.parent / "fixture-session.jsonl"),
                "isStreaming": self.active, "isCompacting": False, "model": self.model, "thinkingLevel": self.thinking,
                "steeringMode": self.queue_modes["steering"], "followUpMode": self.queue_modes["follow_up"],
                "autoCompactionEnabled": self.compaction, "messageCount": len(self.entries),
                "pendingMessageCount": len(self.steering) + len(self.follow_up)}
            self.record("preflight", active=self.active)
            await self.barrier("preflight", "after")
            await self.reply(command, snapshot)
        elif kind == "get_commands":
            commands = [] if self.args.no_prompt_templates else [
                {"name": "fixture-template", "description": "Fixture template", "source": "prompt"}]
            await self.reply(command, {"commands": commands})
        elif kind == "get_available_models":
            await self.reply(command, {"models": [self.model, dict(MODEL, id="fixture-large", name="Fixture Large")]})
        elif kind == "get_available_thinking_levels":
            await self.reply(command, {"levels": ["off", "low", "high"]})
        elif kind == "get_messages":
            await self.reply(command, {"messages": [e["message"] for e in self.entries]})
        elif kind == "get_entries":
            entries = self.entries
            if command.get("since"):
                positions = [i for i, entry in enumerate(entries) if entry["id"] == command["since"]]
                if not positions:
                    await self.reply(command, {"reason": "unknown cursor"}, False)
                    return
                entries = entries[positions[0] + 1:]
            await self.reply(command, {"entries": entries, "leafId": self.entries[-1]["id"] if self.entries else None})
        elif kind == "get_tree":
            await self.reply(command, {"tree": [{"entry": e, "children": []} for e in self.entries],
                                       "leafId": self.entries[-1]["id"] if self.entries else None})
        elif kind == "get_session_stats":
            await self.reply(command, {"userMessages": len(self.entries), "assistantMessages": 0, "toolCalls": 0,
                                       "tokens": {"input": 0, "output": 0, "total": 0}, "cost": 0})
        elif kind in ("prompt", "steer", "follow_up"):
            text = command.get("message")
            if not isinstance(text, str):
                await self.reply(command, {"reason": "message must be text"}, False)
                return
            await self.barrier("acceptance")
            if kind == "prompt" and self.active and command.get("streamingBehavior") not in ("steer", "followUp"):
                await self.reply(command, {"reason": "busy"}, False)
                return
            images = command.get("images", [])
            content = ([{"type": "text", "text": text}] if text or not images else []) + [
                {"type": "image", "mime": image.get("mimeType"), "data": image.get("data")} for image in images]
            self.record("input_received", text=text, content=content, command=kind, active=self.active,
                        session=f"fixture-{self.session}")
            await self.barrier("acceptance", "after")
            if text.startswith("/fixture-template") and not self.args.no_prompt_templates:
                self.record("command_executed", name="fixture-template", text=text)
                await self.input_ack(command, "handled")
                return
            if self.script.get("consume_input"):
                self.record("hook_consumed", text=text)
                await self.input_ack(command, "handled")
                return
            if kind in ("steer", "follow_up") or self.active:
                queue = self.steering if kind == "steer" or command.get("streamingBehavior") == "steer" else self.follow_up
                queue.append(text)
                images = self.steering_images if queue is self.steering else self.follow_up_images
                images.append(command.get("images", []))
                await self.queue_update()
                await self.input_ack(command, "queued")
            else:
                self.active = True
                await self.input_ack(command, "started")
                self.native_turn = True
                self.record("turn", state="start", source="native")
                await self.emit({"type": "agent_start"})
                await self.emit({"type": "turn_start"})
                await self.insert(text, "prompt", command.get("images", []))
            await self.rules("input", {"text": text, "command": kind})
        elif kind == "abort":
            was_active, self.active = self.active, False
            self.native_turn = False
            if self.script.get("abort_inserts_steering"):
                while self.steering:
                    await self.insert(self.steering.pop(0), "abort", self.steering_images.pop(0))
            await self.queue_update()
            if was_active:
                await self.emit({"type": "agent_end", "messages": [], "willRetry": False})
            await self.emit({"type": "agent_settled", "aborted": True})
            await self.reply(command)
        elif kind == "clear_queue":
            removed = {"steering": self.steering[:], "followUp": self.follow_up[:]}
            self.steering.clear()
            self.follow_up.clear()
            self.steering_images.clear()
            self.follow_up_images.clear()
            await self.queue_update()
            await self.reply(command, removed)
        elif kind == "extension_ui_response":
            request = self.requests.pop(command.get("id"), None)
            method = (request or {}).get("method")
            if command.get("cancelled") is True:
                response = {"action": "cancel"}
            elif method == "confirm" and isinstance(command.get("confirmed"), bool):
                response = {"value": command["confirmed"]}
            elif method == "choice" and isinstance(command.get("value"), str):
                response = {"choice": command["value"]}
            elif method == "text" and isinstance(command.get("value"), str):
                response = {"value": command["value"]}
            else:
                response = {"action": "_invalid"}
            self.record("answer", request=command.get("id"), response=command, normalized_response=response,
                        pending=request is not None)
            # Pi extension UI responses have no acknowledgement.
        elif kind in ("set_steering_mode", "set_follow_up_mode"):
            name = "steering" if kind == "set_steering_mode" else "follow_up"
            if command.get("mode") not in ("one-at-a-time", "all"):
                await self.reply(command, None, False)
                return
            self.queue_modes[name] = command["mode"]
            await self.reply(command)
        elif kind == "set_model":
            if command.get("modelId") not in ("fixture-small", "fixture-large"):
                await self.reply(command, None, False)
                return
            self.model = dict(MODEL, id=command["modelId"], name=command["modelId"])
            await self.reply(command, self.model)
        elif kind == "set_thinking_level":
            self.thinking = command["level"]
            await self.reply(command)
        elif kind == "set_auto_compaction":
            self.compaction = bool(command["enabled"])
            await self.reply(command)
        elif kind in ("new_session", "switch_session", "fork", "clone"):
            self.session += 1
            self.entries.clear()
            self.steering.clear()
            self.follow_up.clear()
            self.steering_images.clear()
            self.follow_up_images.clear()
            self.active = False
            self.native_turn = False
            await self.reply(command, {"cancelled": False})
        else:
            await self.reply(command, {"reason": "unknown native command"}, False)

    async def control(self, row):
        verb = row.get("verb")
        if verb == "state":
            return {"pid": os.getpid(), "wire": self.args.wire, "seq": self.serial, "active": self.active,
                    "session": f"fixture-{self.session}", "steering": self.steering[:], "follow_up": self.follow_up[:],
                    "limits": {"record_bytes": MAX_RECORD, "ledger_bytes": MAX_LEDGER, "commands": 64, "script_actions": 1024}}
        if verb in ("pause_before", "pause_after", "hang"):
            phase = row.get("phase", "acceptance")
            if phase not in ("preflight", "acceptance", "insertion", "tool", "model"):
                raise ValueError("unknown semantic barrier")
            when = "after" if verb == "pause_after" else "before"
            self.barriers[(when, phase)] = asyncio.Event()
            return {"paused": phase, "when": when}
        if verb == "release":
            keys = [key for key in self.barriers if row.get("phase") in ("all", key[1])]
            for key in keys:
                self.barriers.pop(key).set()
            return {"released": [list(key) for key in keys]}
        if verb == "drop_reply":
            operation = row.get("operation", row.get("command"))
            if not isinstance(operation, str):
                raise ValueError("drop_reply needs a semantic operation")
            self.drop.add(operation)
            return {"drop_reply": operation}
        if verb == "turn":
            state = row["state"]
            if state == "start" and self.native_turn:
                return {"seq": self.serial}  # a script sets state; it does not double the native start
            self.record("turn", state=state, had_turn=self.native_turn)
            if state == "start":
                self.active = True
                self.native_turn = True
                await self.emit({"type": "agent_start"})
                await self.emit({"type": "turn_start"})
            elif state in ("end", "settled"):
                self.native_turn = False
                await self.emit({"type": "agent_end", "messages": [], "willRetry": False})
                if state == "settled":
                    self.active = False
                    await self.emit({"type": "agent_settled", "aborted": bool(row.get("aborted", False))})
            else:
                raise ValueError("unknown turn state")
        elif verb == "tool_call":
            await self.barrier("tool")
            ident, state = row.get("id_ref", "tool-1"), row["state"]
            self.record("tool_call", tool=ident, state=state)
            await self.emit({"type": "tool_execution_start" if state == "start" else "tool_execution_end",
                             "toolCallId": ident, "toolName": "fixture", "args": {}, "result": {}, "isError": False})
            await self.barrier("tool", "after")
        elif verb == "model_call":
            await self.barrier("model")
            count = len(self.steering) if self.queue_modes["steering"] == "all" else min(1, len(self.steering))
            for _ in range(count):
                await self.insert(self.steering.pop(0), "boundary", self.steering_images.pop(0))
            await self.queue_update()
            self.record("model_call", model=row.get("id_ref", "model-1"), entries=[e["id"] for e in self.entries],
                        messages=[e["message"] for e in self.entries])
            await self.barrier("model", "after")
        elif verb == "settings":
            modes = row.get("queue_modes", {})
            if not modes or any(key not in ("boundary", "settle") or value not in ("one", "all") for key, value in modes.items()):
                raise ValueError("settings needs declared native one/all queue modes")
            for key, value in modes.items():
                name = "steering" if key == "boundary" else "follow_up"
                self.queue_modes[name] = "one-at-a-time" if value == "one" else "all"
            self.record("settings", queue_modes=dict(self.queue_modes), source="native-controller")
        elif verb == "queue":
            queue = self.steering if row["mode"] == "boundary" else self.follow_up
            images = self.steering_images if queue is self.steering else self.follow_up_images
            if row.get("action") == "consume":
                name = "steering" if queue is self.steering else "follow_up"
                native_count = len(queue) if self.queue_modes[name] == "all" else min(1, len(queue))
                count = min(row.get("count", native_count), len(queue))
                for _ in range(count):
                    await self.insert(queue.pop(0), "queue", images.pop(0))
            else:
                content = row.get("content", [])
                queue.extend(content)
                images.extend([[] for _ in content])
            await self.queue_update()
        elif verb == "request":
            ident, method = row["request"], row["method"]
            if ident in self.requests:
                raise ValueError("native pending request identity reused")
            self.requests[ident] = dict(row)
            self.record("request", request=ident, method=method)
            native_method = {"choice": "select", "text": "editor" if row.get("multiline") else "input"}.get(method, method)
            event = {"type": "extension_ui_request", "id": ident, "method": native_method,
                     "title": row.get("title", "Fixture question"), "message": row.get("message", ""),
                     "options": row.get("options", ["one", "two"])}
            if "timeout" in row:
                event["timeout"] = row["timeout"]
            await self.emit(event)
        elif verb == "session_switch":
            self.session += 1
            self.entries.clear()
            self.record("session_switch", session=f"fixture-{self.session}")
        elif verb == "retain":
            count = row.get("count", 1)
            if not isinstance(count, int) or not 0 <= count <= 1024:
                raise ValueError("fixture retain count bound")
            text, chunks = row.get("text", "fixture retained text"), row.get("chunks")
            if chunks is not None and (not isinstance(chunks, list) or len(chunks) > 256 or any(not isinstance(part, str) for part in chunks)):
                raise ValueError("fixture delta work bound")
            for index in range(count):
                current = f"{index}:" if chunks is not None else ""
                await self.emit({"type": "message_start", "message": {"role": "assistant", "content": [
                    {"type": "text", "text": current}] if chunks is not None else []}})
                for part in chunks or []:
                    current += part
                    await self.emit({"type": "message_update", "message": {"role": "assistant", "content": [
                        {"type": "text", "text": current}]}, "assistantMessageEvent": {
                        "type": "text_delta", "contentIndex": 0, "delta": part}})
                await self.emit({"type": "message_end", "message": {"role": "assistant", "content": [
                    {"type": "text", "text": current if chunks is not None else f"{index}:{text}"}], "stopReason": "stop"}})
        elif verb == "failure":
            await self.emit({"type": "message_end", "message": {"role": "assistant", "content": [],
                             "stopReason": "error", "errorMessage": row.get("message", "fixture failure")}})
            self.active = False
            self.native_turn = False
            await self.emit({"type": "agent_settled", "aborted": False})
        elif verb in ("crash", "exit"):
            self.exit_code = row.get("code", 23 if verb == "crash" else 0)
            if not isinstance(self.exit_code, int) or not 0 <= self.exit_code <= 255:
                raise ValueError("invalid fixture exit code")
            self.record("exit", reason=verb, code=self.exit_code)
            self.stop.set()
            return {"exiting": True}
        else:
            raise ValueError(f"unimplemented fixture verb: {verb}")
        return {"seq": self.serial}

    async def controller(self, reader, writer):
        try:
            while line := await reader.readline():
                if len(line) > MAX_RECORD:
                    raise ValueError("control record bound")
                row = json.loads(line)
                try:
                    result = await self.control(row)
                    reply = {"id": row.get("id"), "result": result}
                except (ValueError, KeyError, TypeError) as error:
                    reply = {"id": row.get("id"), "error": {"message": str(error)}}
                writer.write(json.dumps(reply).encode() + b"\n")
                await writer.drain()
        except (OSError, ValueError):
            pass
        finally:
            writer.close()
            await writer.wait_closed()

    def finished(self, task):
        self.tasks.discard(task)
        if not task.cancelled() and task.exception() is not None:
            self.failure = str(task.exception())[:1024]
            self.exit_code = 70
            self.stop.set()

    async def run(self):
        os.set_blocking(1, False)
        server = await asyncio.start_unix_server(self.controller, str(self.args.control), limit=MAX_RECORD + 1)
        loop = asyncio.get_running_loop()
        reader = asyncio.StreamReader(limit=MAX_RECORD + 1)
        protocol = asyncio.StreamReaderProtocol(reader)
        transport, _ = await loop.connect_read_pipe(lambda: protocol, sys.stdin.buffer)
        try:
            async def inputs():
                while line := await reader.readline():
                    if len(line) > MAX_RECORD:
                        raise ValueError("native input record bound")
                    if len(self.tasks) >= 64:
                        raise ValueError("native fixture concurrent-command bound")
                    command = json.loads(line)
                    task = asyncio.create_task(self.native(command))
                    self.tasks.add(task)
                    task.add_done_callback(self.finished)
                self.stop.set()
            task = asyncio.create_task(inputs())
            task.add_done_callback(self.finished)
            await self.stop.wait()
            # Output EOF/process exit is native lifecycle evidence. No synthetic
            # settled event is invented on a crash.
            if not task.done():
                task.cancel()
            for pending in self.tasks:
                pending.cancel()
            await asyncio.gather(task, *self.tasks, return_exceptions=True)
        finally:
            transport.close()
            server.close()
            await server.wait_closed()
            self.args.control.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--wire", choices=("pi", "codex", "acp"), default="pi")
    parser.add_argument("--mode", choices=("rpc",), help="real Pi native launch flag")
    parser.add_argument("--control", type=Path)
    parser.add_argument("--ledger", type=Path)
    parser.add_argument("--script", type=Path)
    parser.add_argument("--no-extensions", action="store_true")
    parser.add_argument("--no-skills", action="store_true")
    parser.add_argument("--no-prompt-templates", action="store_true")
    args = parser.parse_args()
    args.control = args.control or Path(os.environ.get("AGENT_CONFORMANCE_CONTROL", os.environ.get("PI_FIXTURE_CONTROL", "")))
    args.ledger = args.ledger or Path(os.environ.get("AGENT_CONFORMANCE_LEDGER", os.environ.get("PI_FIXTURE_LEDGER", "")))
    if not args.control.is_absolute() or not args.ledger.is_absolute():
        parser.error("absolute private --control/--ledger paths required")
    if args.wire != "pi":
        parser.error(f"fixture wire {args.wire} is not implemented yet; never a capability skip")
    if not stat.S_ISFIFO(os.fstat(1).st_mode):
        parser.error("native fixture requires piped stdout")
    runtime = Runtime(args)
    asyncio.run(runtime.run())
    if runtime.failure:
        print(runtime.failure, file=sys.stderr)
    raise SystemExit(runtime.exit_code)


if __name__ == "__main__":
    main()
