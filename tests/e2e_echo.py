"""A test-only command for e2e suites: `e2e.echo` records every invocation's args (JSONL) and replies {}."""
import json, socket, threading


def start(sock_path, out_path, name="e2e.echo"):
    s = socket.socket(socket.AF_UNIX)
    s.connect(sock_path)
    f = s.makefile("rwb", buffering=0)

    def send(msg):
        f.write((json.dumps(msg) + "\n").encode())

    send({"jsonrpc": "2.0", "id": 1, "method": "hello", "params": {"client": "e2e-echo"}})
    f.readline()
    send({"jsonrpc": "2.0", "id": 2, "method": "command.register", "params": {"name": name, "title": "e2e: record args"}})
    f.readline()

    def serve():
        for line in f:
            msg = json.loads(line)
            if msg.get("method") == "command.invoke" and "id" in msg:
                with open(out_path, "a") as out:
                    out.write(json.dumps(msg["params"].get("args") or {}) + "\n")
                send({"jsonrpc": "2.0", "id": msg["id"], "result": {}})
    threading.Thread(target=serve, daemon=True).start()
    return s


def calls(out_path):
    try:
        with open(out_path) as f:
            return [json.loads(l) for l in f if l.strip()]
    except OSError:
        return []
