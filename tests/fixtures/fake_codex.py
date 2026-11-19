#!/usr/bin/env python3
"""No-model Codex app-server fixture. Native calls are recorded, never terminal-injected.

AGENT_CONFORMANCE_LEDGER: private JSONL ledger path.
CODEX_FAKE_REPLY_DELAY_MS / CODEX_FAKE_HOLD: bounded admission-race fixtures.
The wire contains only native methods; there are no production test-effect operations.
"""
import json
import os
import sys
import threading
import time

lock = threading.Lock()
threads = {}
turns = {}
sequence = 0
ledger = os.environ.get("AGENT_CONFORMANCE_LEDGER")


def write(message):
    with lock:
        sys.stdout.write(json.dumps(message, ensure_ascii=False) + "\n")
        sys.stdout.flush()


def event(method, params):
    write({"method": method, "params": params})


def record(message):
    if ledger:
        with open(ledger, "a", encoding="utf8") as out:
            out.write(json.dumps(message, ensure_ascii=False) + "\n")


def reply(request, result):
    write({"id": request["id"], "result": result})


def error(request, message):
    write({"id": request["id"], "error": {"code": -32600, "message": message}})


def finish(thread_id, turn_id, text, delay):
    time.sleep(delay)
    item = {"id": "assistant-" + turn_id, "type": "agentMessage", "text": ""}
    event("item/started", {"threadId": thread_id, "turnId": turn_id, "item": item})
    event("item/agentMessage/delta", {"threadId": thread_id, "turnId": turn_id,
                                    "itemId": item["id"], "delta": text})
    item["text"] = text
    event("item/completed", {"threadId": thread_id, "turnId": turn_id, "item": item})
    if turns.get(thread_id) == turn_id:
        del turns[thread_id]
        threads[thread_id]["status"] = {"type": "idle"}
        event("turn/completed", {"threadId": thread_id,
                                 "turn": {"id": turn_id, "status": "completed", "error": None}})
        event("thread/status/changed", {"threadId": thread_id, "status": {"type": "idle"}})


def handle(request):
    global sequence
    method = request.get("method")
    params = request.get("params", {})
    if method is None:  # server-initiated request response; no invented acknowledgement
        record(request)
        return
    if method == "initialized":
        record(request)
        return
    record(request)
    if method == "initialize":
        reply(request, {"userAgent": "fake-codex/fixture-1", "platformFamily": "test", "platformOs": "test"})
    elif method == "thread/start":
        sequence += 1
        thread_id = "thread-" + str(sequence)
        thread = {"id": thread_id, "status": {"type": "idle"}, "cwd": params.get("cwd"), "name": None}
        threads[thread_id] = thread
        event("thread/started", {"thread": thread})
        reply(request, {"thread": thread, "model": params.get("model", "fake-model")})
    elif method == "thread/resume":
        thread_id = params["threadId"]
        if thread_id not in threads:
            error(request, "unknown thread")
        else:
            reply(request, {"thread": threads[thread_id], "model": "fake-model"})
    elif method == "thread/list":
        data = list(threads.values())[:min(params.get("limit", 64), 64)]
        reply(request, {"data": data, "nextCursor": None})
    elif method in ("turn/start", "turn/steer"):
        thread_id = params["threadId"]
        if thread_id not in threads:
            error(request, "unknown thread")
            return
        if method == "turn/steer" and params.get("expectedTurnId") != turns.get(thread_id):
            error(request, "expected active turn id does not match")
            return
        if thread_id not in turns:
            sequence += 1
            turns[thread_id] = "turn-" + str(sequence)
            threads[thread_id]["status"] = {"type": "active", "activeFlags": []}
            event("turn/started", {"threadId": thread_id, "turn": {"id": turns[thread_id], "status": "inProgress"}})
        turn_id = turns[thread_id]
        if os.environ.get("CODEX_FAKE_HOLD") == "1":
            return
        delay = min(float(os.environ.get("CODEX_FAKE_REPLY_DELAY_MS", "0")) / 1000, 2)
        time.sleep(delay)
        result = {"turnId": turn_id} if method == "turn/steer" else {"turn": {"id": turn_id, "status": "inProgress", "items": []}}
        reply(request, result)
        text = "".join(x["text"] for x in params["input"] if x.get("type") == "text")
        threading.Thread(target=finish, args=(thread_id, turn_id, text, 0.02), daemon=True).start()
    elif method == "turn/interrupt":
        thread_id = params["threadId"]
        if turns.get(thread_id) != params["turnId"]:
            error(request, "no active turn to interrupt")
        else:
            turn_id = turns.pop(thread_id)
            threads[thread_id]["status"] = {"type": "idle"}
            event("turn/completed", {"threadId": thread_id, "turn": {"id": turn_id, "status": "interrupted", "error": None}})
            reply(request, {})
    else:
        write({"id": request.get("id"), "error": {"code": -32601, "message": "unknown method " + str(method)}})


for raw in sys.stdin.buffer:
    handle(json.loads(raw))
