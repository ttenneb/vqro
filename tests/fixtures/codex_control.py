"""Controlled native Codex wire for K1; reuses its generic socket/ledger/timeline engine.

Only fixture argv selects this module. The production adapter has no test effect or environment
exception. --harness-root is temporary dependency wiring while the two branches are separate.
"""
import asyncio
import contextvars
import importlib
import json
import os
from pathlib import Path
import stat
import sys
import time

OPERATIONS = {"initialize": "query", "initialized": "query", "thread/start": "session.new",
              "thread/resume": "session.resume", "thread/list": "query", "thread/read": "query",
              "turn/start": "send", "turn/steer": "send", "turn/interrupt": "interrupt"}


class CodexMixin:
    def __init__(self, args):
        self.threads = {}
        self.thread_serial = self.turn_serial = self.item_serial = 0
        self.selected = contextvars.ContextVar("codex-session", default=None)
        super().__init__(args)

    def thread(self, row=None, required=True):
        row = row or {}
        ident = row.get("session") or self.selected.get() or next(iter(self.threads), None)
        if ident is None and not required:
            return None
        if ident not in self.threads:
            raise ValueError("unknown or missing native session")
        return self.threads[ident]

    def create_thread(self, cwd=None):
        if len(self.threads) >= 200:
            raise ValueError("native fixture thread bound")
        self.thread_serial += 1
        ident = f"thread-{self.thread_serial}"
        value = {"meta": {"id": ident, "name": None, "cwd": cwd or os.getcwd(), "status": {"type": "idle"}},
                 "run": None, "entries": []}
        self.threads[ident] = value
        return value

    async def event(self, method, params):
        await self.emit({"method": method, "params": params})

    async def reply(self, command, data=None, success=True):
        method = command.get("method", "respond")
        operation = OPERATIONS.get(method, "respond" if method == "respond" else "unknown")
        if method in self.drop or operation in self.drop:
            self.record("reply_dropped", command=method, operation=operation, request=command.get("id"))
            return
        row = {"id": command.get("id")}
        if success:
            row["result"] = {} if data is None else data
        else:
            row["error"] = data
        self.record("native_reply", command=method, operation=operation, request=command.get("id"), success=success)
        await self.emit(row)

    async def error(self, command, message, code=-32600):
        await self.reply(command, {"code": code, "message": message}, False)

    def content(self, native):
        if not isinstance(native, list) or not native or len(native) > 128:
            raise ValueError("native input must be a bounded block array")
        blocks = []
        for block in native:
            if block.get("type") == "text" and isinstance(block.get("text"), str):
                # Keep actual block boundaries and native spans. Do not strip sender framing,
                # flatten blocks, or reconstruct content from the standard caller's arguments.
                out = {"type": "text", "text": block["text"]}
                if "text_elements" in block:
                    out["text_elements"] = block["text_elements"]
                blocks.append(out)
            elif block.get("type") in ("image", "audio") and isinstance(block.get("url"), str):
                url = block["url"]
                if not url.startswith("data:") or ";base64," not in url:
                    raise ValueError("fixture never fetches remote media")
                mime, data = url[5:].split(";base64,", 1)
                blocks.append({"type": block["type"], "mime": mime, "data": data})
            else:
                raise ValueError("unsupported native input block (not a capability skip)")
        return blocks

    async def start_turn(self, thread):
        if thread["run"] is not None:
            return thread["run"]
        self.turn_serial += 1
        run = f"turn-{self.turn_serial}"
        thread["run"] = run
        thread["meta"]["status"] = {"type": "active", "activeFlags": []}
        self.active = True
        self.record("turn", state="start", session=thread["meta"]["id"], run=run)
        await self.event("turn/started", {"threadId": thread["meta"]["id"], "turn": {"id": run, "status": "inProgress", "items": []}})
        await self.event("thread/status/changed", {"threadId": thread["meta"]["id"], "status": thread["meta"]["status"]})
        return run

    async def end_turn(self, thread, aborted=False, failure=None):
        run = thread["run"]
        if run is None:
            return
        session = thread["meta"]["id"]
        for key, request in list(self.requests.items()):
            if request["session"] == session and request["run"] == run and request["blocking"]:
                del self.requests[key]
                self.record("request_cancelled", request=request["id"], session=session, run=run)
                await self.event("serverRequest/resolved", {"threadId": session, "requestId": request["id"]})
        thread["run"] = None
        thread["meta"]["status"] = {"type": "idle"}
        self.active = any(t["run"] is not None for t in self.threads.values())
        self.record("turn", state="settled", session=session, run=run, aborted=aborted, error=failure)
        await self.event("turn/completed", {"threadId": session, "turn": {"id": run,
            "status": "failed" if failure else "interrupted" if aborted else "completed",
            "error": {"message": failure} if failure else None}})
        await self.event("thread/status/changed", {"threadId": session, "status": {"type": "idle"}})

    async def insert_native(self, thread, command, content, run):
        await self.barrier("insertion")
        if thread["run"] != run:
            self.record("input_cancelled_before_insertion", session=thread["meta"]["id"], run=run, request=command.get("id"))
            return
        self.item_serial += 1
        ident = f"user-{self.item_serial}"
        session = thread["meta"]["id"]
        entry = {"id": ident, "message": {"role": "user", "content": content}}
        thread["entries"].append(entry)
        self.record("inserted", session=session, entry=ident, request=command.get("id"), content=content,
                    texts=[b["text"] for b in content if b["type"] == "text"])
        item = {"id": ident, "type": "userMessage", "clientId": None, "content": command["params"]["input"]}
        params = {"threadId": session, "turnId": thread["run"], "item": item}
        await self.event("item/started", params)
        await self.event("item/completed", params)
        await self.barrier("insertion", "after")
        # Pattern hints do not replace the ledger's ordered content. Exact matches see actual
        # native text (including framing); use contains when the adapter declares sender framing.
        text = next((b["text"] for b in content if b["type"] == "text"), "")
        await self.rules("input", {"text": text, "session": session, "content": content})

    async def rules(self, on, value):
        if on != "input":
            return await super().rules(on, value)
        texts = [b["text"] for b in value.get("content", []) if b["type"] == "text"]
        for rule in self.script.get("script", []):
            if rule.get("on") != "input":
                continue
            match = rule.get("match", {})
            if set(match) - {"text", "contains"}:
                raise ValueError("unimplemented fixture match predicate")
            if match and not any(("text" not in match or text == match["text"]) and
                                 ("contains" not in match or match["contains"] in text) for text in texts):
                continue
            for action in rule.get("do", []):
                await self.action(action)

    @staticmethod
    def normalized_answer(command, request):
        error = command.get("error")
        if isinstance(error, dict) and error.get("code") in (-32800, -32601):
            return {"action": "cancel"}
        result = command.get("result")
        method = request["method"] if request else None
        if method == "permission" and isinstance(result, dict):
            decision = result.get("decision")
            if decision == "cancel":
                return {"action": "cancel"}
            if decision in ("accept", "acceptForSession", "decline"):
                return {"choice": decision}
        if method in ("form", "choice", "confirm", "text") and isinstance(result, dict):
            answers = result.get("answers")
            if isinstance(answers, dict) and all(isinstance(v, dict) and isinstance(v.get("answers"), list)
                    and all(isinstance(s, str) for s in v["answers"]) for v in answers.values()):
                return {"action": "accept", "content": {k: v["answers"] for k, v in answers.items()}}
        return {"action": "_invalid"}

    async def native(self, command):
        method = command.get("method")
        if method is None:
            self.record("command", command="respond", operation="respond", effectful=True, value=command)
            key = json.dumps(command.get("id"), sort_keys=True)
            request = self.requests.pop(key, None)
            self.record("answer", request=command.get("id"), session=request["session"] if request else None,
                        run=request["run"] if request else None, pending=request is not None, value=command.get("result"), error=command.get("error"),
                        normalized_response=self.normalized_answer(command, request))
            if request:
                await self.event("serverRequest/resolved", {"threadId": request["session"], "requestId": command.get("id")})
                thread = self.threads[request["session"]]
                if thread["run"] is not None and not any(r["session"] == request["session"] and r["blocking"] for r in self.requests.values()):
                    thread["meta"]["status"] = {"type": "active", "activeFlags": []}
                    await self.event("thread/status/changed", {"threadId": request["session"], "status": thread["meta"]["status"]})
            return  # A JSON-RPC response never gets a fabricated acknowledgement.
        if not isinstance(method, str):
            raise ValueError("native JSON-RPC method must be a string")
        operation = OPERATIONS.get(method, "unknown")
        self.record("command", command=method, operation=operation, effectful=operation != "query", value=command)
        params = command.get("params") or {}
        if method == "initialize":
            await self.reply(command, {"userAgent": "fake-codex/controlled-1", "platformFamily": "test", "platformOs": "test"})
            return
        if method == "initialized":
            return
        if method == "thread/start":
            thread = self.create_thread(params.get("cwd"))
            self.record("session", action="new", session=thread["meta"]["id"])
            await self.event("thread/started", {"thread": thread["meta"]})
            await self.reply(command, {"thread": thread["meta"], "model": "fixture-small"})
            return
        if method == "thread/list":
            start = int(params.get("cursor") or "0")
            limit = min(int(params.get("limit", 64)), 64)
            data = [t["meta"] for t in self.threads.values()]
            await self.reply(command, {"data": data[start:start + limit], "nextCursor": str(start + limit) if start + limit < len(data) else None})
            return
        session = params.get("threadId")
        if session not in self.threads:
            await self.error(command, "unknown native thread")
            return
        token = self.selected.set(session)
        try:
            thread = self.threads[session]
            if method in ("thread/resume", "thread/read"):
                self.record("session", action="resume" if method.endswith("resume") else "read", session=session)
                await self.reply(command, {"thread": thread["meta"], "model": "fixture-small"})
            elif method in ("turn/start", "turn/steer"):
                await self.barrier("preflight")
                try:
                    content = self.content(params.get("input"))
                except ValueError as error:
                    await self.error(command, str(error), -32602)
                    return
                await self.barrier("preflight", "after")
                await self.barrier("acceptance")
                # Guard checking and acceptance have no intervening await. A control caller can
                # race a paused preflight/acceptance, but cannot split the native atomic guard.
                if method == "turn/steer" and (thread["run"] is None or params.get("expectedTurnId") != thread["run"]):
                    await self.error(command, "expected active turn id does not match")
                    return
                text = next((b["text"] for b in content if b["type"] == "text"), "")
                self.record("input_received", session=session, request=command.get("id"), text=text,
                            texts=[b["text"] for b in content if b["type"] == "text"], content=content)
                run = await self.start_turn(thread)
                await self.barrier("acceptance", "after")
                await self.reply(command, {"turnId": run} if method == "turn/steer" else {"turn": {"id": run, "status": "inProgress", "items": []}})
                await self.insert_native(thread, command, content, run)
            elif method == "turn/interrupt":
                if params.get("turnId") != thread["run"] or thread["run"] is None:
                    await self.error(command, "no active turn to interrupt")
                    return
                await self.end_turn(thread, True)
                await self.reply(command, {})
            else:
                await self.error(command, "unknown native method", -32601)
        finally:
            self.selected.reset(token)

    async def control(self, row):
        verb = row.get("verb")
        if verb in ("pause_before", "pause_after", "hang", "release", "drop_reply", "exit", "crash"):
            return await super().control(row)
        if verb == "host_exit":
            return await super().control({**row, "verb": "exit"})
        if verb == "state":
            thread = self.thread(row, required=False)
            return {"pid": os.getpid(), "wire": "codex", "seq": self.serial,
                    "active": bool(thread and thread["run"]), "session": thread["meta"]["id"] if thread else None,
                    "cwd": thread["meta"]["cwd"] if thread else None, "model": "fixture-small", "effort": None,
                    "sessions": list(self.threads), "limits": {"record_bytes": 1 << 20, "ledger_bytes": 16 << 20,
                    "commands": 64, "threads": 200, "script_actions": 1024}}
        if verb == "session_switch":
            previous = self.thread(row, required=False)
            thread = self.create_thread(row.get("cwd"))
            self.record("session_switch", session=thread["meta"]["id"], previous=previous["meta"]["id"] if previous else None)
            # App-server threads do not spontaneously turn into another thread. The harness must
            # use public session.resume/new to move a logical endpoint; never invent a native RPC.
            return {"session": thread["meta"]["id"], "requires_resume": True}
        thread = self.thread(row)
        session = thread["meta"]["id"]
        token = self.selected.set(session)
        try:
            if verb == "turn":
                state = row["state"]
                if state == "start":
                    await self.start_turn(thread)
                elif state == "end":
                    # Canonical timeline's inner model-turn boundary is not Codex's outer turn.
                    # App-server exposes no separate inner-end notification; never invent one.
                    self.record("turn", state="end", session=session, run=thread["run"], native_turn_complete=False)
                elif state == "settled":
                    await self.end_turn(thread, bool(row.get("aborted", False)))
                else:
                    raise ValueError("unknown turn state")
            elif verb == "failure":
                message = row.get("message", "Fixture native turn failure")
                if not isinstance(message, str) or len(message.encode()) > 4096:
                    raise ValueError("failure message bound")
                self.record("failure", session=session, run=thread["run"], message=message)
                await self.end_turn(thread, failure=message)
            elif verb == "model_call":
                await self.barrier("model")
                self.record("model_call", model=row.get("id_ref", "model-1"), session=session,
                            entries=[e["id"] for e in thread["entries"]], messages=[e["message"] for e in thread["entries"]])
                await self.barrier("model", "after")
            elif verb == "request":
                ident, method = row["request"], row["method"]
                key = json.dumps(ident, sort_keys=True)
                if key in self.requests:
                    raise ValueError("native request identity already pending")
                if len(self.requests) >= 256:
                    raise ValueError("native request fixture bound")
                scope = row.get("scope", "run")
                scope_kind = scope.get("kind") if isinstance(scope, dict) else scope
                blocking = row.get("is_blocking", scope_kind != "session")
                if not isinstance(blocking, bool):
                    raise ValueError("is_blocking must be boolean")
                run = row.get("run") or thread["run"] or f"background-{len(self.requests) + 1}"
                item = row.get("item", f"request-tool-{len(self.requests) + 1}")
                if method == "permission":
                    native = "item/fileChange/requestApproval" if row.get("tool_kind") == "file" else "item/commandExecution/requestApproval"
                    params = {"threadId": session, "turnId": run, "itemId": item,
                              "startedAtMs": int(time.time() * 1000), "reason": row.get("title", "Fixture approval")}
                    if row.get("tool_kind") == "file":
                        params["grantRoot"] = row.get("grant_root")
                    else:
                        params["command"] = row.get("command", "printf fixture")
                        params["cwd"] = thread["meta"]["cwd"]
                        if "available_decisions" in row:
                            params["availableDecisions"] = row["available_decisions"]
                elif method in ("form", "choice", "confirm", "text"):
                    native = "item/tool/requestUserInput"
                    options = row.get("options", ["one", "two"])
                    question = {"id": "answer", "header": row.get("title", "Fixture question"),
                                "question": row.get("message", "Choose"), "isOther": method == "text", "isSecret": False,
                                "options": [{"label": str(v), "description": str(v)} for v in options] if method != "text" else None}
                    params = {"threadId": session, "turnId": run, "itemId": item,
                              "questions": row.get("questions", [question]), "isBlocking": blocking}
                else:
                    native = row.get("native_method", "_fixture/unknownRequest")
                    params = {"threadId": session, "turnId": run, "itemId": item, "method": method}
                self.requests[key] = {"id": ident, "session": session, "run": run, "blocking": blocking, "method": method}
                self.record("request", request=ident, method=method, session=session, run=run, blocking=blocking)
                await self.emit({"id": ident, "method": native, "params": params})
                if blocking and thread["run"] is not None:
                    flags = ["waitingOnApproval"] if method == "permission" else ["waitingOnUserInput"]
                    thread["meta"]["status"] = {"type": "active", "activeFlags": flags}
                    await self.event("thread/status/changed", {"threadId": session, "status": thread["meta"]["status"]})
            elif verb == "tool_call":
                await self.barrier("tool")
                ident, state = row.get("id_ref", "tool-1"), row["state"]
                self.record("tool_call", tool=ident, state=state, session=session, run=thread["run"])
                item = {"id": ident, "type": "commandExecution", "command": "fixture", "cwd": thread["meta"]["cwd"],
                        "processId": None, "commandActions": [], "exitCode": None if state == "start" else 0,
                        "durationMs": None if state == "start" else 1,
                        "status": "inProgress" if state == "start" else "completed", "aggregatedOutput": row.get("text", "")}
                await self.event("item/started" if state == "start" else "item/completed", {"threadId": session, "turnId": thread["run"], "item": item})
                await self.barrier("tool", "after")
            elif verb == "usage":
                # C11 (test-only): one native thread/tokenUsage/updated notification, exactly as reported.
                report = row.get("report")
                if not isinstance(report, dict) or len(json.dumps(report).encode()) > 4096:
                    raise ValueError("fixture usage report must be a bounded native tokenUsage object")
                self.record("usage", session=session, report=report)
                await self.event("thread/tokenUsage/updated", {"threadId": session, "turnId": thread["run"], "tokenUsage": report})
            elif verb == "retain":
                count, text = row.get("count", 1), row.get("text", "retained fixture text")
                if not isinstance(count, int) or not 0 <= count <= 1024 or not isinstance(text, str) or len(text.encode()) > 256 * 1024:
                    raise ValueError("retain count/text bound")
                for index in range(count):
                    self.item_serial += 1
                    ident = f"assistant-{self.item_serial}"
                    item = {"id": ident, "type": "agentMessage", "text": ""}
                    await self.event("item/started", {"threadId": session, "turnId": thread["run"], "item": item})
                    output = f"{index}:{text}"
                    await self.event("item/agentMessage/delta", {"threadId": session, "turnId": thread["run"], "itemId": ident, "delta": output})
                    await self.event("item/completed", {"threadId": session, "turnId": thread["run"], "item": {**item, "text": output}})
                self.record("retain", session=session, count=count, text_bytes=len(text.encode()))
            else:
                raise ValueError(f"unimplemented native Codex fixture verb: {verb}")
            return {"seq": self.serial, "session": session}
        finally:
            self.selected.reset(token)


def run_controlled(args):
    root = args.harness_root or Path(__file__).resolve().parents[1]
    if not (root / "agent_conformance" / "runtime.py").is_file():
        raise SystemExit("controlled Codex fixture needs K1 tests via --harness-root until it merges")
    if not args.control.is_absolute() or not args.ledger or not Path(args.ledger).is_absolute():
        raise SystemExit("absolute private --control/--ledger paths required")
    if args.control.exists():
        raise SystemExit("refusing to replace an existing fixture control path")
    if not stat.S_ISFIFO(os.fstat(1).st_mode):
        raise SystemExit("controlled fixture requires piped stdout")
    args.ledger = Path(args.ledger)
    args.wire = "codex"
    sys.path.insert(0, str(root))
    engine = importlib.import_module("agent_conformance.runtime")
    cls = type("CodexRuntime", (CodexMixin, engine.Runtime), {})
    runtime = cls(args)
    asyncio.run(runtime.run())
    if runtime.failure:
        print(runtime.failure, file=sys.stderr)
    return runtime.exit_code
