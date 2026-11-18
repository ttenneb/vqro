"""Deterministic Pi JSONL fixture, never a provider or model client.

Normal Pi-shaped commands exercise native effects. test_* commands are fixture
controls (barriers, not sleeps) and are not a proposed production protocol.
"""
import json
import os
import sys
import time

active = False
steering = []
follow = []
held = []
accepted = []
trace_path = sys.argv[sys.argv.index("--trace-file") + 1] if "--trace-file" in sys.argv else None
timing_path = sys.argv[sys.argv.index("--timing-file") + 1] if "--timing-file" in sys.argv else None


def timing(kind, record):
    if timing_path:
        with open(timing_path, "a") as output:
            output.write(json.dumps({"event": kind, "ns": time.monotonic_ns(), "id": record.get("id"),
                                     "command": record.get("command", record.get("type"))}) + "\n")


def trace(record):
    if trace_path:
        fd = os.open(trace_path, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600)
        try:
            os.write(fd, json.dumps(record).encode() + b"\n")
        finally:
            os.close(fd)


trace({"pid": os.getpid(), "bootstrap_in_child": "PRIM_BRIDGE_TOKEN" in os.environ,
       "herdr_in_child": any(key.startswith("HERDR_") for key in os.environ)})


def emit(record):
    timing("emit", record)
    os.write(1, json.dumps(record, ensure_ascii=False).encode() + b"\n")


if "--no-read" in sys.argv:
    import threading
    emit({"type": "test_ready"})
    threading.Event().wait()


def reply(command, data=None, success=True):
    record = {"type": "response", "id": command.get("id"),
              "command": command["type"], "success": success}
    if data is not None:
        record["data"] = data
    emit(record)


def queues():
    emit({"type": "queue_update", "steering": steering, "followUp": follow})


def settle(aborted=False):
    global active
    active = False
    emit({"type": "agent_settled", "aborted": aborted})


def error_turn(message, retries=0, recovered=False):
    for attempt in range(retries+1):
        if attempt:
            emit({"type": "auto_retry_start", "attempt": attempt, "maxAttempts": retries,
                  "delayMs": 0, "errorMessage": message})
        emit({"type": "message_end", "message": {"role": "assistant", "content": [],
              "stopReason": "error", "errorMessage": message}})
    if recovered:
        emit({"type": "message_end", "message": {"role": "assistant", "content": "Recovered reply", "stopReason": "stop"}})
    if retries:
        emit({"type": "auto_retry_end", "attempt": retries, "success": recovered,
              **({} if recovered else {"finalError": message})})
    emit({"type": "agent_end", "messages": [], "willRetry": False})
    settle()


for line in sys.stdin.buffer:
    command = json.loads(line)
    timing("receive", command)
    trace(command)
    kind = command["type"]
    if kind == "get_state":
        reply(command, {"isStreaming": active, "isCompacting": False,
                        "messageCount": len(accepted), "pendingMessageCount": len(steering) + len(follow)})
    elif kind in {"get_messages", "get_commands", "get_session_stats"}:
        reply(command, {"messages": [], "commands": [], "tokens": {"total": 0}, "cost": 0})
    elif kind == "prompt":
        message = command.get("message")
        if message == "crash":
            os._exit(23)
        if message == "linger":
            # A descendant inherits stdout/stderr and outlives us; its pid goes to
            # $HOME/linger.pid so the test can clean up exactly that process.
            import subprocess
            child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"],
                                     stdin=subprocess.DEVNULL, close_fds=False)
            with open(os.path.join(os.environ["HOME"], "linger.pid"), "w") as f:
                f.write(str(child.pid))
            os._exit(0)
        if message == "hang":
            accepted.append(message)
            held.append(command)
            continue
        if message == "reject" or (active and "streamingBehavior" not in command):
            reply(command, {"reason": "rejected before acceptance"}, False)
            continue
        accepted.append(message)
        if message == "handled":
            reply(command, {"disposition": "handled"})
        elif active:
            (steering if command["streamingBehavior"] == "steer" else follow).append(message)
            queues()
            reply(command, {"disposition": "queued"})
        else:
            reply(command, {"disposition": "started"})
            active = True
            emit({"type": "agent_start"})
            emit({"type": "turn_start"})
            if message in ("fixture-model-error", "fixture-retry-success"):
                emit({"type": "message_end", "message": {"role": "user", "content": message}})
                error_turn("ResourceNotFoundException: Model use case details have not been submitted", 4,
                           recovered=message == "fixture-retry-success")
                continue
            emit({"type": "tool_execution_start", "toolCallId": "tool-1", "toolName": "fixture", "args": {}})
            if "--bridge-e2e" in sys.argv and message == "fixture-dialog":
                emit({"type": "extension_ui_request", "id": "fixture-confirm", "method": "confirm",
                      "title": "Fixture confirmation", "message": "Continue without a model call?"})
    elif kind in {"steer", "follow_up"}:
        (steering if kind == "steer" else follow).append(command["message"])
        queues()
        reply(command, {"disposition": "queued"})
    elif kind == "clear_queue":
        removed = {"steering": steering[:], "followUp": follow[:]}
        steering.clear()
        follow.clear()
        queues()
        reply(command, removed)
    elif kind == "abort":
        if active:
            emit({"type": "agent_end", "messages": [], "willRetry": False})
        # Pi abort preserves queued work: model only its native continuation
        # and idle-before-response semantics, not a fictitious clear-on-abort.
        while steering or follow:
            value = (steering if steering else follow).pop(0)
            queues()
            emit({"type": "message_end", "message": {"role": "user", "content": value}})
        settle(True)
        reply(command)
    elif kind == "extension_ui_response":
        emit({"type": "test_answer_observed", "answer": command})
        if "--bridge-e2e" in sys.argv and command.get("id") == "fixture-confirm":
            emit({"type": "message_end", "message": {"role": "assistant", "content": [
                {"type": "text", "text": "FIXTURE FINAL transcript tail"}]}})
            settle()
    elif kind == "test_retry_start":
        emit({"type": "auto_retry_start", "attempt": 2, "maxAttempts": 4, "delayMs": 2000, "errorMessage": "Fixture overloaded"})
        reply(command)
    elif kind == "test_error_turn":
        error_turn(command.get("message", "Fixture model failed"), command.get("retries", 0), command.get("recovered", False))
        reply(command)
    elif kind == "test_end":
        emit({"type": "tool_execution_end", "toolCallId": "tool-1", "toolName": "fixture", "result": {}, "isError": False})
        emit({"type": "turn_end", "message": {}, "toolResults": []})
        emit({"type": "agent_end", "messages": [], "willRetry": False})
        reply(command)
    elif kind == "test_settle":
        settle()
        reply(command)
    elif kind == "test_boundary":
        if steering:
            value = steering.pop(0)
            queues()
            emit({"type": "message_end", "message": {"role": "user", "content": value}})
        reply(command)
    elif kind == "test_dialog":
        emit({"type": "extension_ui_request", "id": command["dialog_id"],
              "method": command["method"], "title": command.get("title", "Fixture question"),
              "message": command.get("message", ""), "options": command.get("options", ["one", "two"]),
              "timeout": command.get("timeout", 0)})
        reply(command)
    elif kind == "test_release":
        for old in reversed(held):
            reply(old, {"disposition": "started", "echo": old.get("message")})
        held.clear()
        reply(command)
    elif kind == "test_hold":
        held.append(command)
    elif kind == "test_transcript":
        emit({"type": "message_start", "message": {"role": "assistant", "content": []}})
        for _ in range(command.get("count", 1)):
            emit({"type": "message_update", "assistantMessageEvent": {
                "type": "text_delta", "contentIndex": 0, "delta": command.get("text", "fixture text")}})
        if "final" in command:
            emit({"type": "message_end", "message": {"role": "assistant", "content": [
                {"type": "text", "text": command["final"]}]}})
        reply(command)
    elif kind == "test_accepted":
        reply(command, {"accepted": accepted})
    elif kind == "test_unicode":
        wire = json.dumps({"type": "fixture_text", "text": "before\u2028middle\u2029after"}, ensure_ascii=False).encode() + b"\n"
        split = wire.index(b"\xe2") + 1
        os.write(1, wire[:split])
        os.write(1, wire[split:])
        reply(command)
    elif kind == "test_flood":
        os.write(2, b"diagnostic" * command.get("count", 100))
        for i in range(command.get("count", 100)):
            emit({"type": "message_update", "index": i, "text": "x" * 128})
        reply(command)
    elif kind == "test_oversize":
        os.write(1, b"x" * 1048576)
    elif kind == "test_truncated":
        os.write(1, b'{"type":"response"')
        sys.exit(0)
    elif kind == "test_bad_response":
        emit({"type": "response", "id": command["id"], "command": "different", "success": True})
    else:
        reply(command, {"unknown": kind}, False)
