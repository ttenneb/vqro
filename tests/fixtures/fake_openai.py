"""Deterministic fake OpenAI-compatible chat model for Pi plugin tests (no network, no paid calls).

Every request is appended to --log as {n, t, messages:[{role, text}]}: the model request log is the oracle for
*when a message was consumed*. Script: the newest user text decides the reply.
  "steps:K"       -> K turns, each a `bash` tool call `sleep S` (S from "sleep:S", default 1), then final text.
  anything else   -> one text reply "ack <n>".
Tool-result turns continue the step count found in the conversation.
"""
import argparse, json, re, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ap = argparse.ArgumentParser()
ap.add_argument("--port", type=int, required=True)
ap.add_argument("--log", required=True)
args = ap.parse_args()
count = 0


def text_of(content):
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return "".join(p.get("text", "") for p in content if isinstance(p, dict))
    return ""


class H(BaseHTTPRequestHandler):
    def log_message(self, *a):
        pass

    def do_GET(self):  # model listing, if probed
        body = json.dumps({"data": [{"id": "fake-1", "object": "model"}]}).encode()
        self.send_response(200); self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body))); self.end_headers(); self.wfile.write(body)

    def do_POST(self):
        global count
        req = json.loads(self.rfile.read(int(self.headers["content-length"])))
        count += 1
        n = count
        msgs = [{"role": m.get("role"), "text": text_of(m.get("content"))[:400],
                 "kinds": [p.get("type") for p in m["content"]] if isinstance(m.get("content"), list) else ["text"],
                 **({"tool_calls": len(m["tool_calls"])} if m.get("tool_calls") else {})} for m in req.get("messages", [])]
        with open(args.log, "a") as f:
            f.write(json.dumps({"n": n, "t": time.time(), "messages": msgs}) + "\n")
        ms = req["messages"]
        starts = [i for i, m in enumerate(ms) if m.get("role") == "user" and "steps:" in text_of(m.get("content"))]
        task = text_of(ms[starts[-1]]["content"]) if starts else ""
        steps = int((re.search(r"steps:(\d+)", task) or [0, 0])[1])
        sleep = (re.search(r"sleep:([\d.]+)", task) or [0, "1"])[1]
        done_tools = sum(1 for m in ms[starts[-1]:] if m.get("role") == "tool") if starts else 0
        self.send_response(200)
        self.send_header("content-type", "text/event-stream"); self.end_headers()

        def chunk(delta, finish=None):
            row = {"id": f"c{n}", "object": "chat.completion.chunk", "created": int(time.time()), "model": "fake-1",
                   "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}
            self.wfile.write(b"data: " + json.dumps(row).encode() + b"\n\n"); self.wfile.flush()

        if done_tools < steps:
            call = {"index": 0, "id": f"call_{n}", "type": "function",
                    "function": {"name": "bash", "arguments": json.dumps({"command": f"sleep {sleep}; echo step{done_tools + 1}"})}}
            chunk({"role": "assistant", "tool_calls": [call]})
            chunk({}, "tool_calls")
        else:
            chunk({"role": "assistant", "content": f"ack {n}"})
            chunk({}, "stop")
        usage = {"id": f"c{n}", "object": "chat.completion.chunk", "created": int(time.time()), "model": "fake-1",
                 "choices": [], "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}}
        self.wfile.write(b"data: " + json.dumps(usage).encode() + b"\n\ndata: [DONE]\n\n"); self.wfile.flush()


ThreadingHTTPServer(("127.0.0.1", args.port), H).serve_forever()
