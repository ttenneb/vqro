#!/usr/bin/env python3
"""No-model Codex app-server fixture. Native calls are recorded, never terminal-injected.

AGENT_CONFORMANCE_LEDGER: private JSONL ledger path.
CODEX_FAKE_REPLY_DELAY_MS / CODEX_FAKE_HOLD: bounded admission-race fixtures.
The wire contains only native methods; there are no production test-effect operations.
"""
import argparse
import json
import os
from pathlib import Path
import sys
import threading
import time

lock = threading.Lock()
threads = {}
thread_models = {}
MODELS = ("fake-model", "fake-alternate")
turns = {}
sequence = 0
parser = argparse.ArgumentParser()
parser.add_argument("--ledger", default=os.environ.get("AGENT_CONFORMANCE_LEDGER"))
parser.add_argument("--reply-delay-ms", type=int, default=int(os.environ.get("CODEX_FAKE_REPLY_DELAY_MS", "0")))
parser.add_argument("--hold", action="store_true", default=os.environ.get("CODEX_FAKE_HOLD") == "1")
parser.add_argument("--model-read-delay-ms", type=int, default=int(os.environ.get("CODEX_FAKE_MODEL_READ_DELAY_MS", "0")))
parser.add_argument("--reject-model-read", action="store_true", help="fixture-only native read error witness")
parser.add_argument("--ignore-model-override", action="store_true", default=os.environ.get("CODEX_FAKE_IGNORE_MODEL")=="1")
parser.add_argument("--omit-model-readback", action="store_true")
parser.add_argument("--exit-after-init", action="store_true")
parser.add_argument("--control", type=Path, help="K1 private control socket; enables controlled mode")
parser.add_argument("--script", type=Path)
parser.add_argument("--harness-root", type=Path, help="fixture-only path to K1 tests while branches differ")
args = parser.parse_args()
if args.control:
    from codex_control import run_controlled
    raise SystemExit(run_controlled(args))
ledger = args.ledger


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
        if args.exit_after_init:
            sys.exit(0)
    elif method == "model/list":
        reply(request, {"data": [{"id":"catalog-"+model,"model":model,"displayName":model,"description":"Native fixture model",
            "hidden":False,"isDefault":model==MODELS[0],"defaultReasoningEffort":"low","supportedReasoningEfforts":[]} for model in MODELS], "nextCursor":None})
    elif method == "thread/start":
        model = params.get("model", MODELS[0])
        if model not in MODELS:
            error(request, "unknown native model"); return
        sequence += 1
        thread_id = "thread-" + str(sequence)
        thread = {"id": thread_id, "status": {"type": "idle"}, "cwd": params.get("cwd"), "name": None}
        threads[thread_id] = thread
        thread_models[thread_id] = model
        event("thread/started", {"thread": thread})
        reply(request, {"thread": thread, "model": model})
    elif method == "thread/resume":
        thread_id = params["threadId"]
        if thread_id not in threads:
            error(request, "unknown thread")
        else:
            model = params.get("model", thread_models[thread_id])
            if model not in MODELS:
                error(request, "unknown native model"); return
            if "model" in params:
                error(request,"no rollout found for thread id "+thread_id); return
            reply(request, {"thread": threads[thread_id], "model": thread_models[thread_id]})
    elif method == "thread/read":
        thread_id=params["threadId"]
        if args.reject_model_read:
            error(request,"fixture native model read rejected"); return
        if thread_id not in threads:
            error(request,"unknown thread");return
        time.sleep(args.model_read_delay_ms / 1000)
        model=None if args.omit_model_readback else thread_models[thread_id]
        reply(request,{"thread":{**threads[thread_id],"model":model,"reasoningEffort":None},"readState":None})
    elif method == "thread/list":
        data = list(threads.values())[:min(params.get("limit", 64), 64)]
        reply(request, {"data": data, "nextCursor": None})
    elif method in ("turn/start", "turn/steer"):
        thread_id = params["threadId"]
        if thread_id not in threads:
            error(request, "unknown thread")
            return
        if method == "turn/start" and "model" in params:
            if params["model"] not in MODELS:
                error(request, "unknown native model"); return
            if not args.ignore_model_override:
                thread_models[thread_id] = params["model"]
        if method == "turn/steer" and params.get("expectedTurnId") != turns.get(thread_id):
            error(request, "expected active turn id does not match")
            return
        if thread_id not in turns:
            sequence += 1
            turns[thread_id] = "turn-" + str(sequence)
            threads[thread_id]["status"] = {"type": "active", "activeFlags": []}
            event("turn/started", {"threadId": thread_id, "turn": {"id": turns[thread_id], "status": "inProgress"}})
        turn_id = turns[thread_id]
        if args.hold:
            return
        delay = min(max(args.reply_delay_ms, 0) / 1000, 2)
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
