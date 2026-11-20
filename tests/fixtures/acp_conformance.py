#!/usr/bin/env python3
"""Native ACP fixture, not an adapter or a second runtime implementation.

Reuses K1's private control/ledger engine and P6 a726ede's actual Model/Codec.
Only the explicit subset in acp-conformance.md is supported. No MCP or network.
"""
import argparse
import asyncio
import json
import os
from pathlib import Path
import stat
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from agent_conformance.runtime import Runtime
from acp_conformance_model import Codec, Model, Rejected, Unsupported, SCHEMA_VOCABULARY

OPERATIONS = {"initialize": "query", "session/new": "session.new", "session/load": "session.resume",
              "session/resume": "session.resume", "session/prompt": "send", "session/cancel": "interrupt",
              "session/set_config_option": "configure"}
MAX_OPERATIONS = 256
MAX_SESSIONS = 32


class AcpRuntime(Runtime):
    def __init__(self, args):
        self.model_state = Model(max_pending=64, max_bytes=32768)
        self.codec = Codec(SCHEMA_VOCABULARY)
        self.version = None
        self.current = None
        self.pending_rpc = {}
        self.answer_ids = {}
        self.operation_serial = 0
        self.configs = {}
        self.cwds = {}
        super().__init__(args)
        allowed = {"turn", "model_call", "request", "retain", "pause_before", "pause_after", "drop_reply", "exit", "host_exit", "crash", "hang"}
        for rule in self.script.get("script", []):
            if rule.get("on") != "input" or any(len(allowed.intersection(action)) != 1 for action in rule.get("do", [])):
                raise ValueError("only input-triggered timelines with implemented ACP controls are supported")

    def selected(self, row):
        session = row.get("session") or self.current
        if session not in self.model_state.sessions:
            raise ValueError("unknown or missing session")
        return session

    def messages(self, session):
        state = self.model_state.sessions[session]
        return [{"role": "user", "content": json.loads(entity["content"])}
                for (kind, _), entity in sorted(state.entities.items(), key=lambda pair: pair[1]["position"])
                if kind == "user_message"]

    async def reply(self, command, data=None, success=True):
        if "id" not in command:  # Native notifications never receive invented replies.
            return
        method = command.get("method", "respond")
        operation = OPERATIONS.get(method, "respond")
        if method in self.drop or operation in self.drop:
            self.record("reply_dropped", command=method, operation=operation, request=command["id"])
            return
        self.record("native_reply", command=method, operation=operation, request=command["id"], success=success)
        command["_fixture_replied"] = True
        await self.emit({"jsonrpc": "2.0", "id": command["id"], "result" if success else "error": data or {}})

    async def update(self, session, value):
        params = {"sessionId": session, "update": value}
        neutral = self.codec.decode(self.version, "session/update", params)
        await self.emit(self.codec.encode(self.version, "session/update", neutral))

    async def state_update(self, session, reason=None):
        state = self.model_state.sessions[session]
        self.active = any(s.foreground != "idle" for s in self.model_state.sessions.values())
        if self.version == 2:
            value = {"sessionUpdate": "state_update", "state": state.foreground}
            if reason is not None:
                value["stopReason"] = reason
            await self.update(session, value)

    async def message_update(self, session, ident, content):
        if self.version == 2:
            await self.update(session, {"sessionUpdate": "user_message", "messageId": ident, "content": content})
        else:
            for block in content:
                await self.update(session, {"sessionUpdate": "user_message_chunk", "content": block})

    async def flush_replies(self):
        for operation, command in list(self.pending_rpc.items()):
            result = self.model_state.facade_replies.get(operation)
            if result is not None:
                del self.pending_rpc[operation]
                if "error" in result:
                    await self.reply(command, {"code": result["error"], "message": "cancelled before insertion"}, False)
                else:
                    await self.reply(command, result)

    async def native(self, command):
        if not isinstance(command, dict) or command.get("jsonrpc") != "2.0":
            raise ValueError("native fixture requires JSON-RPC 2.0 objects")
        method, params = command.get("method"), command.get("params", {})
        self.record("command", command=method or "respond", method=method, params=params,
                    request=command.get("id"), operation=OPERATIONS.get(method, "respond" if method is None else "unknown"),
                    effectful=method not in ("initialize",), value=command)
        try:
            if method is None:
                await self.permission_answer(command)
                return
            if not isinstance(params, dict):
                raise ValueError("params must be an object")
            if method != "session/cancel" and "id" not in command:
                raise ValueError("this native method requires a JSON-RPC request id")
            if method == "initialize":
                if self.version is not None or params.get("protocolVersion") not in (1, 2):
                    raise ValueError("fixture supports one initialize, protocolVersion 1 or 2")
                self.version = Model.negotiate(params["protocolVersion"], [params["protocolVersion"]])
                self.codec.decode(self.version, method, params)
                info = {"name": "acp-conformance-fixture", "version": "1"}
                data = {"protocolVersion": self.version, "authMethods": []}
                if self.version == 2:
                    data.update(info=info, capabilities={"session": {"prompt": {}}})
                else:
                    data.update(agentInfo=info, agentCapabilities={"loadSession": True})
                await self.reply(command, data)
                return
            if self.version is None:
                raise ValueError("initialize first")
            if method not in OPERATIONS:
                raise Unsupported("unimplemented native ACP method")
            self.codec.decode(self.version, method, params)
            if method == "session/new":
                if len(self.model_state.sessions) >= MAX_SESSIONS:
                    raise ValueError("fixture session bound")
                if not isinstance(params.get("cwd"), str) or not Path(params["cwd"]).is_absolute():
                    raise ValueError("absolute cwd required")
                if set(params) - {"cwd", "mcpServers", "_meta"}:
                    raise Unsupported("additional session setup is outside this fixture subset")
                session = f"session-{len(self.model_state.sessions) + 1}"
                self.model_state.new(session)
                self.current = session
                self.cwds[session] = params["cwd"]
                self.configs[session] = [{"configId": "fixture.auto", "name": "Automatic fixture",
                                          "type": "boolean", "currentValue": False}]
                self.record("session", action="new", session=session)
                await self.reply(command, {"sessionId": session, **({"configOptions": self.configs[session]} if self.version == 2 else {})})
                return
            session = params.get("sessionId")
            self.selected({"session": session})
            if self.model_state.sessions[session].replaying and method in ("session/prompt", "session/set_config_option"):
                raise Unsupported("live mutations during replay are outside this fixture subset")
            if method == "session/prompt":
                await self.prompt(command, session, params)
            elif method == "session/cancel":
                await self.finish_turn(session, True)
                await self.reply(command, {})
            elif method in ("session/load", "session/resume"):
                if (self.version, method) not in ((1, "session/load"), (2, "session/resume")):
                    raise Unsupported("wrong replay method for negotiated dialect")
                await self.replay(command, session, params)
            elif method == "session/set_config_option":
                if self.version != 2 or params.get("configId") != "fixture.auto" or params.get("type") != "boolean" or type(params.get("value")) is not bool:
                    raise ValueError("unknown config or invalid value")
                self.configs[session][0]["currentValue"] = params["value"]
                self.model_state.put(session, "config", "fixture.auto", {"value": params["value"]})
                await self.update(session, {"sessionUpdate": "config_option_update", "configOptions": self.configs[session]})
                await self.reply(command, {"configOptions": self.configs[session]})
        except (ValueError, KeyError, TypeError) as error:
            self.record("native_error", request=command.get("id"), message=str(error))
            if command.get("_fixture_replied"):
                raise  # A post-reply fixture failure is not a second native reply.
            await self.reply(command, {"code": -32601 if isinstance(error, Unsupported) else -32602,
                                       "message": str(error)}, False)

    async def prompt(self, command, session, params):
        content = params.get("prompt")
        if not isinstance(content, list) or not content or len(content) > 64 or any(
                not isinstance(b, dict) or set(b) != {"type", "text"} or b["type"] != "text" or not isinstance(b["text"], str) for b in content):
            raise Unsupported("fixture accepts ordered text blocks only")
        if set(params) - {"sessionId", "prompt", "idempotencyKey", "_meta"}:
            raise Unsupported("unsupported prompt field")
        if self.operation_serial >= MAX_OPERATIONS:
            raise ValueError("fixture lifetime operation bound")
        await self.barrier("preflight")
        await self.barrier("preflight", "after")
        await self.barrier("acceptance")
        if self.model_state.sessions[session].replaying:
            raise Unsupported("live prompt during replay is outside this fixture subset")
        content_bytes = len(json.dumps(content, separators=(",", ":")).encode())
        if sum(len(item["content"].encode()) for item in self.model_state.operations.values() if item["session"] == session) + content_bytes > 256 * 1024:
            raise ValueError("fixture per-session lifetime content bound")
        if params.get("idempotencyKey") is not None and (not isinstance(params["idempotencyKey"], str) or len(params["idempotencyKey"]) > 128):
            raise ValueError("fixture insertion key bound")
        self.operation_serial += 1
        operation = f"prompt-{self.operation_serial}"
        previous = len(self.model_state.native_writes)
        self.model_state.prompt(self.version, session, operation, json.dumps(content, separators=(",", ":")),
                                key=params.get("idempotencyKey"))
        self.pending_rpc[operation] = command
        item = self.model_state.operations[operation]
        if len(self.model_state.native_writes) != previous:
            self.record("input_received", operation="send", session=session, request=command.get("id"), content=content)
            self.model_state.sessions[session].foreground = "running"
            self.record("turn", state="start", session=session)
            await self.state_update(session)
        await self.barrier("acceptance", "after")
        await self.barrier("insertion")
        if item["stage"] == "dispatched":
            self.model_state.insert(operation)
            self.record("inserted", session=session, request=command.get("id"), entry=item["message"], content=content)
            await self.message_update(session, item["message"], content)
        await self.flush_replies()
        await self.barrier("insertion", "after")
        await self.rules("input", {"text": content[0]["text"]})

    async def finish_turn(self, session, aborted=False):
        state = self.model_state.sessions[session]
        was_active = state.foreground != "idle"
        if aborted:
            self.model_state.interrupt(session)
        else:
            # P6 keeps v1 facade calls pending until observed foreground completion.
            operations = [op for op, item in self.model_state.operations.items()
                          if item["session"] == session and item["stage"] == "inserted" and item["version"] == 1 and op in self.pending_rpc]
            if operations:
                for op in operations:
                    self.model_state.complete(session, "end_turn", op)
            elif was_active:
                self.model_state.complete(session)
        for op, item in self.model_state.operations.items():
            if aborted and item["session"] == session and item["version"] == 1 and item["stage"] == "inserted" and op in self.pending_rpc:
                self.model_state.facade_replies[op] = {"stopReason": "cancelled"}
        if was_active:
            self.record("turn", state="settled", session=session, aborted=aborted)
            await self.state_update(session, "cancelled" if aborted else "end_turn")
        await self.flush_replies()

    async def replay(self, command, session, params):
        if set(params) - {"sessionId", "cwd", "mcpServers", "replayFrom", "_meta"}:
            raise Unsupported("unsupported replay field")
        if params.get("cwd") != self.cwds[session]:
            raise ValueError("fixture replay requires the session's original absolute cwd")
        if self.version == 2 and params.get("replayFrom") not in (None, {"type": "start"}):
            raise ValueError("only replayFrom start is supported")
        if self.model_state.sessions[session].foreground != "idle":
            raise Unsupported("this fixture resumes only idle sessions")
        self.current = session
        if self.version == 1 or params.get("replayFrom") is not None:
            state = self.model_state.sessions[session]
            cut = [(key, dict(value)) for key, value in state.entities.items()]
            self.model_state.start_replay(session)
            self.record("replay", phase="begin", session=session)
            try:
                for (kind, ident), entity in cut:
                    self.model_state.put(session, kind, ident, entity)
                    if kind == "user_message":
                        await self.message_update(session, ident, json.loads(entity["content"]))
                if self.version == 2:
                    await self.update(session, {"sessionUpdate": "replay_complete"})
                self.model_state.replay_end(session)
                self.record("replay", phase="end", session=session)
            except BaseException:
                self.model_state.replay_abort(session)
                raise
        await self.reply(command, {"configOptions": self.configs[session]} if self.version == 2 else {})

    async def permission_answer(self, command):
        key = json.dumps(command.get("id"), sort_keys=True)
        if key not in self.answer_ids:
            self.record("answer", request=command.get("id"), pending=False, approved=False)
            return
        session, request = self.answer_ids.pop(key)
        result = command.get("result")
        outcome = result.get("outcome", {}) if isinstance(result, dict) else {}
        if not isinstance(outcome, dict):
            outcome = {}
        state = self.model_state.sessions[session]
        value = state.requests[request]
        valid = True
        try:
            if "error" in command or outcome.get("outcome") == "cancelled":
                response = {"action": "cancel"}
            elif outcome.get("outcome") == "selected":
                response = {"choice": outcome.get("optionId")}
            else:
                raise Rejected("unknown permission outcome")
            self.model_state.answer(session, request, state.epoch, value["revision"], response)
        except (Rejected, TypeError, AttributeError):
            valid = False
            if value["status"] == "pending":
                self.model_state.answer(session, request, state.epoch, value["revision"], {"action": "cancel"})
            response = {"action": "cancel"}
        approved = valid and response.get("choice") == "allow"
        self.record("answer", request=command.get("id"), session=session, valid=valid, approved=approved, value=command.get("result"))
        if state.foreground == "requires_action":
            state.foreground = "running"
            await self.state_update(session)

    async def control(self, row):
        verb = row.get("verb")
        if verb in ("pause_before", "pause_after", "hang", "release", "drop_reply", "exit", "crash"):
            return await super().control(row)
        if verb == "host_exit":
            return await super().control({**row, "verb": "exit"})
        if verb == "state":
            session = row.get("session") or self.current
            state = self.model_state.sessions.get(session)
            after, limit = row.get("after", 0), row.get("limit", 16)
            if type(after) is not int or after < 0 or type(limit) is not int or not 1 <= limit <= 64:
                raise ValueError("invalid state message page")
            messages, page, used = self.messages(session) if state else [], [], 0
            for message in messages[after:after + limit]:
                charge = len(json.dumps(message).encode())
                if used + charge > 48 * 1024:
                    break
                page.append(message)
                used += charge
            next_page = after + len(page) if after + len(page) < len(messages) else None
            return {"pid": os.getpid(), "wire": "acp", "seq": self.serial, "session": session,
                    "sessions": list(self.model_state.sessions), "active": bool(state and state.foreground != "idle"),
                    "messages": page, "next": next_page, "limits": {"sessions": MAX_SESSIONS,
                    "operations": MAX_OPERATIONS, "content_bytes": 32768, "session_content_bytes": 256 * 1024,
                    "record_bytes": 1 << 20, "ledger_bytes": 16 << 20}}
        if verb == "reconnect":
            result = self.model_state.reconnect()
            self.record("reconnect", sessions=result)
            return result  # Observation only: never invokes prompt again.
        session = self.selected(row)
        state = self.model_state.sessions[session]
        if verb == "turn":
            if row.get("state") == "start":
                state.foreground = "running"
                self.record("turn", state="start", session=session)
                await self.state_update(session)
            elif row.get("state") in ("end", "settled"):
                await self.finish_turn(session, bool(row.get("aborted", False)))
            else:
                raise ValueError("unknown turn state")
        elif verb == "model_call":
            await self.barrier("model")
            self.record("model_call", model=row.get("id_ref", "model-1"), session=session, messages=self.messages(session))
            await self.barrier("model", "after")
        elif verb == "request":
            if row.get("method") != "permission":
                raise ValueError("only permission requests implemented")
            if state.foreground == "idle" or row.get("scope", "run") != "run":
                raise ValueError("fixture permission requests require foreground work; background scopes unsupported")
            if len(state.requests) >= 64:
                raise ValueError("request bound")
            request = row["request"]
            if not isinstance(request, str) or len(request) > 128 or json.dumps(request) in self.answer_ids:
                raise ValueError("request identity invalid or already used")
            self.model_state.request(session, request, "permission", {"choices": ["allow", "deny"]})
            self.answer_ids[json.dumps(request)] = (session, request)
            options = [{"optionId": "allow", "name": "Allow once", "kind": "allow_once"},
                       {"optionId": "deny", "name": "Reject once", "kind": "reject_once"}]
            params = {"sessionId": session, "options": options}
            if self.version == 2:
                params["title"] = row.get("title", "Fixture permission")
            else:
                params["toolCall"] = {"toolCallId": f"tool-{request}", "title": row.get("title", "Fixture permission"), "status": "pending"}
            self.record("request", request=request, method="permission", session=session)
            native = self.codec.encode(self.version, "session/request_permission",
                                       self.codec.decode(self.version, "session/request_permission", params))
            await self.emit({**native, "id": request})
            state.foreground = "requires_action"
            await self.state_update(session)
        elif verb == "retain":
            if state.replaying:
                raise ValueError("retention mutation during replay is outside this fixture subset")
            if row.get("action") != "prune" or not isinstance(row.get("key"), str):
                raise ValueError("retain supports only action prune with an insertion key")
            self.model_state.prune_retained_message(session, row["key"])
            self.record("retention", session=session, key=row["key"])
        else:
            raise ValueError(f"unimplemented ACP fixture control: {verb}")
        return {"seq": self.serial, "session": session}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--wire", choices=("acp",), default="acp")
    parser.add_argument("--control", type=Path, required=True)
    parser.add_argument("--ledger", type=Path, required=True)
    parser.add_argument("--script", type=Path)
    args = parser.parse_args()
    if not args.control.is_absolute() or not args.ledger.is_absolute() or args.control.exists():
        parser.error("fresh absolute private control path and absolute ledger path required")
    if not stat.S_ISFIFO(os.fstat(1).st_mode):
        parser.error("piped native stdout required")
    runtime = AcpRuntime(args)
    asyncio.run(runtime.run())
    if runtime.failure:
        print(runtime.failure, file=sys.stderr)
    raise SystemExit(runtime.exit_code)


if __name__ == "__main__":
    main()
