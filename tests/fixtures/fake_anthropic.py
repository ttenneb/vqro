"""Deterministic fake Anthropic Messages API for Claude Code tests (no network, no spend). Every request is logged to
--log as {n, t, stream, model, messages: [{role, blocks: [{type, text}]}]}; that log is the oracle for when content
reached the model. Script, from the newest user text block containing "steps:N" (optional "sleep:S"):
N turns of one Bash tool call `sleep S; echo stepK`, then a text reply "ack <n>". Without a task: "ack <n>".
"""
import argparse, json, re, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ap = argparse.ArgumentParser()
ap.add_argument("--port", type=int, required=True)
ap.add_argument("--log", required=True)
args = ap.parse_args()
count = [0]


def blocks(content):
    if isinstance(content, str):
        return [{"type": "text", "text": content}]
    out = []
    for b in content or []:
        if b.get("type") == "text":
            out.append({"type": "text", "text": b.get("text", "")})
        elif b.get("type") == "tool_result":
            inner = b.get("content")
            text = inner if isinstance(inner, str) else "".join(x.get("text", "") for x in inner or [] if isinstance(x, dict))
            out.append({"type": "tool_result", "text": text})
        else:
            out.append({"type": b.get("type", "?"), "text": json.dumps(b.get("input", ""))[:200]})
    return out


class H(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *a):
        pass

    def reply_json(self, obj, code=200):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("content-type", "application/json"); self.send_header("content-length", str(len(body)))
        self.end_headers(); self.wfile.write(body)

    def do_GET(self):
        self.reply_json({"data": [], "has_more": False})

    def do_POST(self):
        req = json.loads(self.rfile.read(int(self.headers.get("content-length", 0))) or b"{}")
        if "count_tokens" in self.path:
            return self.reply_json({"input_tokens": 10})
        if not self.path.startswith("/v1/messages"):
            return self.reply_json({"type": "error", "error": {"type": "not_found_error", "message": self.path}}, 404)
        count[0] += 1
        n = count[0]
        msgs = [{"role": m["role"], "blocks": blocks(m.get("content"))} for m in req.get("messages", [])]
        with open(args.log, "a") as f:
            f.write(json.dumps({"n": n, "t": time.time(), "stream": bool(req.get("stream")), "model": req.get("model"),
                                "tools": len(req.get("tools") or []), "messages": msgs}) + "\n")
        # The task: newest user text with steps:N; count tool results after it.
        task_i, task = None, ""
        for i, m in enumerate(msgs):
            if m["role"] == "user":
                for b in m["blocks"]:
                    if b["type"] == "text" and "steps:" in b["text"]:
                        task_i, task = i, b["text"]
        steps = int((re.search(r"steps:(\d+)", task) or [0, 0])[1])
        sleep = (re.search(r"sleep:([\d.]+)", task) or [0, "1"])[1]
        done = sum(1 for m in msgs[task_i:] for b in m["blocks"] if b["type"] == "tool_result") if task_i is not None else 0
        has_tools = bool(req.get("tools"))
        if has_tools and done < steps:
            content = [{"type": "tool_use", "id": f"toolu_{n}", "name": "Bash",
                        "input": {"command": f"sleep {sleep}; echo step{done + 1}", "description": f"step {done + 1}"}}]
            stop = "tool_use"
        else:
            content = [{"type": "text", "text": f"ack {n}"}]
            stop = "end_turn"
        message = {"id": f"msg_{n}", "type": "message", "role": "assistant", "model": req.get("model", "fake"),
                   "content": [], "stop_reason": None, "stop_sequence": None,
                   "usage": {"input_tokens": 10, "output_tokens": 0, "cache_creation_input_tokens": 0, "cache_read_input_tokens": 0}}
        if not req.get("stream"):
            message.update(content=content, stop_reason=stop, usage=dict(message["usage"], output_tokens=5))
            return self.reply_json(message)
        self.send_response(200)
        self.send_header("content-type", "text/event-stream"); self.send_header("cache-control", "no-cache")
        self.send_header("connection", "close"); self.end_headers()

        def ev(kind, data):
            self.wfile.write(f"event: {kind}\ndata: {json.dumps(dict(data, type=kind))}\n\n".encode()); self.wfile.flush()

        ev("message_start", {"message": message})
        for i, block in enumerate(content):
            if block["type"] == "text":
                ev("content_block_start", {"index": i, "content_block": {"type": "text", "text": ""}})
                ev("content_block_delta", {"index": i, "delta": {"type": "text_delta", "text": block["text"]}})
            else:
                ev("content_block_start", {"index": i, "content_block": {"type": "tool_use", "id": block["id"], "name": block["name"], "input": {}}})
                ev("content_block_delta", {"index": i, "delta": {"type": "input_json_delta", "partial_json": json.dumps(block["input"])}})
            ev("content_block_stop", {"index": i})
        ev("message_delta", {"delta": {"stop_reason": stop, "stop_sequence": None}, "usage": {"output_tokens": 5}})
        ev("message_stop", {})
        self.close_connection = True


ThreadingHTTPServer(("127.0.0.1", args.port), H).serve_forever()
