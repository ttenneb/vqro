#!/usr/bin/env python3
"""Synthetic Claude stream/control peer. No network, CLI, auth/profile or transcript reads."""
import json
import os
import sys

SESSION = "synthetic-session-1"
RESUME = sys.argv[sys.argv.index('--resume')+1] if '--resume' in sys.argv else None
INIT_SESSION = os.environ.get('FAKE_CLAUDE_INIT_SESSION')
if RESUME and INIT_SESSION == 'same':
    SESSION = RESUME
elif RESUME and INIT_SESSION == 'wrong':
    SESSION = 'synthetic-wrong-session'
COUNT = 0
ACTIVE = None
PENDING = {}
INITIALIZED = 0
LOG = os.environ.get("FAKE_CLAUDE_LOG")


def record(kind, value):
    if LOG:
        with open(LOG, "a", encoding="utf-8") as out:
            out.write(json.dumps({kind: value}, ensure_ascii=False) + "\n")


def emit(value):
    record("out", value)
    sys.stdout.write(json.dumps(value, ensure_ascii=False) + "\n")
    sys.stdout.flush()


def control(request_id, value):
    emit({"type": "control_response", "response": {"subtype": "success", "request_id": request_id, "response": value}})


def state(value):
    emit({"type": "system", "subtype": "session_state_changed", "state": value,
          "session_id": SESSION, "uuid": "state-" + str(COUNT) + "-" + value})


def assistant(uuid, blocks):
    emit({"type": "assistant", "uuid": uuid, "session_id": SESSION,
          "parent_tool_use_id": None, "message": {"id": "shared-api-message-" + str(COUNT), "role": "assistant", "content": blocks}})


def result(input_uuid, reason="completed", error=False):
    global ACTIVE
    emit({"type": "result", "subtype": "error_during_execution" if error else "success",
          "uuid": "result-" + str(COUNT), "session_id": SESSION,
          "user_message_uuid": input_uuid, "user_message_uuids": [input_uuid],
          "terminal_reason": reason, "is_error": error, "queued_turn_count": 0,
          "usage": {"input_tokens": 11, "output_tokens": 7}})
    ACTIVE = None
    state("idle")


def receive_user(frame):
    global COUNT, ACTIVE
    COUNT += 1
    assert frame.get("client_composed") is True, "literal strategy must be explicit"
    assert frame.get("priority") == "later", "no unqualified mid-turn insertion"
    assert frame["message"]["role"] == "user"
    assert all(block["type"] == "text" for block in frame["message"]["content"])
    uuid = frame["uuid"]
    ACTIVE = uuid
    emit({"type": "system", "subtype": "init", "uuid": "init-" + str(COUNT), "session_id": SESSION,
          "model": "synthetic-model", "tools": ["Bash"], "slash_commands": [], "skills": [],
          "mcp_servers": [], "plugins": [], "cwd": os.getcwd(),
          "capabilities": ["interrupt_receipt_v1", "interrupt_cancel_queued_v1"]})
    state("running")
    emit({"type": "user", "uuid": uuid, "isReplay": True, "session_id": SESSION,
          "parent_tool_use_id": None, "message": frame["message"]})
    text = "\n".join(block["text"] for block in frame["message"]["content"])
    if "[[hang]]" in text:
        return
    if "[[permission]]" in text or "[[unsupported_question]]" in text:
        tool_id = "tool-" + str(COUNT)
        assistant("assistant-tool-" + str(COUNT), [{"type": "tool_use", "id": tool_id, "name": "Bash", "input": {"command": "printf synthetic"}}])
        request_id = "permission-" + str(COUNT)
        PENDING[request_id] = (uuid, tool_id)
        request = {"subtype": "can_use_tool", "tool_name": "Bash", "tool_use_id": tool_id,
                   "input": {"command": "printf synthetic"}, "decision_reason": "synthetic permission"}
        if "[[unsupported_question]]" in text:
            request["requires_user_interaction"] = True
        emit({"type": "control_request", "request_id": request_id, "request": request})
        return
    if "[[limit]]" in text:
        emit({"type": "rate_limit_event", "uuid": "rate-" + str(COUNT), "session_id": SESSION,
              "rate_limit_info": {"status": "rejected", "rateLimitType": "five_hour", "utilization": 1.0}})
        result(uuid, "blocking_limit", True)
        return
    assistant("assistant-first-" + str(COUNT), [{"type": "text", "text": "first synthetic block"}])
    assistant("assistant-second-" + str(COUNT), [{"type": "text", "text": "second synthetic block"}])
    result(uuid)
    if "[[exit_tail]]" in text:
        # Final records precede pipe EOF; process exit must not truncate the item tail.
        sys.exit(0)


def receive_control(frame):
    global INITIALIZED
    request = frame["request"]
    request_id = frame["request_id"]
    if request["subtype"] == "initialize":
        INITIALIZED += 1
        if RESUME and INIT_SESSION in ('same', 'wrong') and INITIALIZED == 1:
            emit({'type':'system','subtype':'init','uuid':'synthetic-resume-init','session_id':SESSION,
                  'model':'synthetic-model','tools':[],'slash_commands':[],'skills':[],'mcp_servers':[],'plugins':[]})
        control(request_id, {"claude_code_version": os.environ.get("FAKE_CLAUDE_VERSION", "2.1.329"),
                             "commands": [], "agents": [], "models": [], "output_styles": [], "capabilities": []})
        if INITIALIZED > 1:
            emit({"type": "system", "subtype": "background_tasks_changed", "uuid": "background-snapshot",
                  "session_id": SESSION, "tasks": []})
    elif request["subtype"] == "interrupt":
        control(request_id, {"still_queued": []})
        if ACTIVE:
            result(ACTIVE, "aborted_streaming")
    else:
        emit({"type": "control_response", "response": {"subtype": "error", "request_id": request_id, "error": "synthetic unsupported control"}})


def receive_response(frame):
    response = frame["response"]
    pending = PENDING.pop(response["request_id"], None)
    if pending is None:
        return
    input_uuid, tool_id = pending
    denied = response["subtype"] != "success" or response.get("response", {}).get("behavior") != "allow"
    emit({"type": "user", "uuid": "tool-result-" + str(COUNT), "session_id": SESSION,
          "parent_tool_use_id": None, "message": {"role": "user", "content": [
              {"type": "tool_result", "tool_use_id": tool_id, "content": "denied" if denied else "synthetic output", "is_error": denied}]}})
    result(input_uuid)


def main():
    record("pid", os.getpid())
    # The bridge must not leak controller/bootstrap/mailbox environment to its native child.
    forbidden = sorted(key for key in os.environ if key.startswith("HERDR_") or key in {
        "VQRO_BRIDGE_TOKEN", "VQRO_EXTENSION_TOKEN", "VQRO_BOOTSTRAP_TOKEN", "VQRO_ACCOUNT",
        "CLAUDE_CODE_RESUME_INTERRUPTED_TURN", "CLAUDE_CODE_RESUME_REASON"})
    record("forbidden_env_keys", forbidden)
    assert not forbidden, "controller/bootstrap/rescue environment leaked"
    record("argv", sys.argv[1:])
    for line in sys.stdin:
        frame = json.loads(line)
        record("in", frame)
        if frame["type"] == "control_request":
            receive_control(frame)
        elif frame["type"] == "user":
            receive_user(frame)
        elif frame["type"] == "control_response":
            receive_response(frame)
        else:
            raise AssertionError("unexpected synthetic peer input")


if __name__ == "__main__":
    main()
