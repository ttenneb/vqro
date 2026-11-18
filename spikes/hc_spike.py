"""Spike: drive herdr-core over its JSON-RPC socket.

Run with an isolated session:
  HC_SESSION=primspike XDG_STATE_HOME=... XDG_RUNTIME_DIR=... python3 hc_spike.py
"""
import json, os, socket, subprocess, sys, threading, time

HC = os.path.expanduser("~/Projects/bin/hc")
AGENT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fake-agent.sh")


def sock_path():
    rt = os.environ["XDG_RUNTIME_DIR"]
    return os.path.join(rt, "herdr-core", os.environ.get("HC_SESSION", "default") + ".sock")


class Conn:
    def __init__(self):
        self.s = socket.socket(socket.AF_UNIX)
        self.s.connect(sock_path())
        self.f = self.s.makefile("rw")
        self.n = 0

    def call(self, method, params=None):
        self.n += 1
        self.f.write(json.dumps({"jsonrpc": "2.0", "id": self.n, "method": method, "params": params or {}}) + "\n")
        self.f.flush()
        while True:
            msg = json.loads(self.f.readline())
            if msg.get("id") == self.n:
                if "error" in msg:
                    raise RuntimeError(msg["error"])
                return msg["result"]


def log(*a):
    print(f"[{time.strftime('%H:%M:%S')}]", *a, flush=True)


def main():
    subprocess.run([HC, "ls"], check=True, capture_output=True)  # starts the core
    c = Conn()
    hello = c.call("hello", {})
    log("hello:", {k: hello[k] for k in hello if k != "methods"})

    events = []

    def listen():
        e = Conn()
        e.call("events.subscribe", {})
        for line in e.f:
            m = json.loads(line)
            if m.get("method") == "event":
                p = m["params"]
                events.append(p)
                if p["type"] not in ("foreground", "size"):
                    log("EVENT", p["type"], p.get("terminal"), json.dumps(p.get("data"))[:160])

    threading.Thread(target=listen, daemon=True).start()
    time.sleep(0.3)

    # 1. start a shell
    r = c.call("terminal.run", {"argv": ["bash", "--norc"], "name": "sh1"})
    log("run:", json.dumps(r)[:200])
    t = r.get("terminal", {}).get("id") or r.get("id") or "sh1"
    time.sleep(0.5)
    # 2. send + read
    c.call("terminal.send", {"terminal": t, "text": "echo hi-from-vqro"})
    time.sleep(1)
    out = c.call("terminal.read", {"terminal": t, "lines": 20})
    log("read contains hi:", "hi-from-vqro" in json.dumps(out))
    # 3. labels + docs
    c.call("terminal.label", {"terminal": t, "labels": {"vqro.role": "worker"}})
    v = c.call("doc.set", {"name": "vqro-spike", "value": {"layout": "x"}, "version": 0})
    log("doc.set ->", v, "doc.get ->", c.call("doc.get", {"name": "vqro-spike"}))
    # 4. fake agent with OSC 7501
    r2 = c.call("terminal.run", {"argv": [AGENT], "name": "agent1"})
    a = r2.get("terminal", {}).get("id") or "agent1"
    w = c.call("terminal.wait", {"terminal": a})
    log("wait returned:", json.dumps(w)[:200])
    time.sleep(4)
    # 5. kill shell
    log("kill:", json.dumps(c.call("terminal.kill", {"terminal": t}))[:200])
    time.sleep(0.5)
    log("status events:", [e["data"].get("status") for e in events if e["type"] == "status"])


if __name__ == "__main__":
    main()
