"""Shared harness for the tier-1 plugin tests: a private vqro core plus a coordination inbox, either the fake
(`tests/fixtures/fake_inbox.py`) or K2 itself (`tests/fixtures/coord_k2.py`, needs the coord-ext checkout).

    h = Harness(root, vqro, inbox="k2", coord_ext=path)   # or inbox="fake"
    rid = h.agent(pull={"points": [...], "insert": "user", "evidence": "entry"})
    d = h.send(rid, "boundary", txt("hi"), render=["context"])
    h.get(d)["state"]

With K2 the harness plays the launcher and the owner: it binds its own connection to the owner principal, writes
the endpoint's pull declaration (`doc/agents/terminals/<rN>`) and the plugin principal's `receive` grant, and
resumes the endpoint's gate (a new coordination owner starts with every gate closed).
"""
import json
import os
import socket
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))


def txt(s):
    return [{"type": "text", "text": s}]


class Harness:
    def __init__(self, root, vqro, *, inbox="fake", coord_ext=None):
        self.root, self.kind = root, inbox
        os.makedirs(root, exist_ok=True)
        self.sock = os.path.join(root, "v.sock")
        self.inbox_log = os.path.join(root, "inbox.jsonl")
        self.env = {"HOME": os.path.join(root, "home-core"), "PATH": "/usr/bin:/bin", "XDG_RUNTIME_DIR": os.path.join(root, "xrun"),
                    "VQRO_STATE_DIR": os.path.join(root, "st"), "VQRO_SOCKET": self.sock,
                    "VQRO_EXTENSIONS_DIR": os.path.join(root, "noext"), "VQRO_HERDR": '["/bin/false"]',
                    "HC_SOCKET": os.path.join(root, "h.sock"), "HC_SESSION": "tier1"}
        for d in ("home-core", "xrun", "st", "noext"):
            os.makedirs(os.path.join(root, d), exist_ok=True)
        self.procs = []
        self.spawn([vqro, "serve", "--fake"], env=self.env, stdout=open(os.path.join(root, "core.log"), "w"), stderr=subprocess.STDOUT)
        while not os.path.exists(self.sock):
            time.sleep(0.05)
        if inbox == "k2":
            self.spawn([sys.executable, os.path.join(HERE, "fixtures", "coord_k2.py")], env=dict(self.env, COORD_EXT=coord_ext),
                       stderr=open(os.path.join(root, "inbox.err"), "w"))
        else:
            self.spawn([sys.executable, os.path.join(HERE, "fixtures", "fake_inbox.py"), self.inbox_log],
                       env=dict(self.env, VQRO_EXT_PATH=os.path.join(HERE, "..", "extensions")), stderr=open(os.path.join(root, "inbox.err"), "w"))
        self.c = Client(self.sock)
        while not any(x.get("name") == "coord.inbox.take" for x in self.c.call("command.list", {})["commands"]):
            time.sleep(0.1)
        self.n = 0
        if inbox == "k2":
            self.c.cmd("fixture.principal", author=self.c.me, principal="p:owner")
            self.c.call("workspace.apply", {"ops": [{"op": "set", "key": "doc/principals/owner", "value": "p:owner"}]})

    def spawn(self, argv, **kw):
        p = subprocess.Popen(argv, **kw)
        self.procs.append(p)
        return p

    def stop(self):
        for p in self.procs:
            if p.poll() is None:
                p.kill()
                p.wait()

    def op(self):
        self.n += 1
        epoch = self.c.cmd("coord.inbox.window")["epoch"]
        return f"{epoch}:test:{self.n}"

    def agent(self, pull):
        """A terminal endpoint (fake backend) declared for pull; returns its rN."""
        rid = self.c.call("resource.spawn", {"argv": ["sh"]})["resource"]["id"]
        if self.kind == "k2":
            self.c.call("workspace.apply", {"ops": [
                {"op": "set", "key": f"doc/agents/grants/plugin-{rid}", "value": {"who": "p:plugin", "on": "agent:" + rid, "can": ["receive"]}},
                {"op": "set", "key": f"doc/agents/terminals/{rid}", "value": {"pull": pull}}]})
            time.sleep(0.5)
            self.resume(rid)
        return rid

    def resume(self, rid):
        inspect = self.c.cmd("coord.scope", action="inspect", scope="agent:" + rid)
        return self.c.cmd("coord.scope", op=self.op(), action="resume", scope="agent:" + rid, expect=inspect["expect"])

    def send(self, to, mode, content, render=None, **extra):
        op = self.op()
        args = {"op": op, "to": to, "mode": mode, "content": content, "send_new": True, **extra}
        if self.kind == "k2":
            args["submission"] = op + ":sub"
        if render is not None:
            args["render"] = render
        return self.c.cmd("coord.inbox.send", **args)["delivery"]

    def edit(self, delivery, content):
        rec = self.get(delivery)
        args = {"op": self.op(), "delivery": delivery, "content": content}
        if self.kind == "k2":
            args["expect"] = rec.get("revision")
        return self.c.cmd("coord.inbox.edit", **args)

    def get(self, delivery):
        return self.c.cmd("coord.inbox.get", delivery=delivery)

    def statuses(self):
        if not os.path.exists(self.inbox_log):
            return []
        return [json.loads(l) for l in open(self.inbox_log) if '"event": "status"' in l]


class Client:
    def __init__(self, sock):
        self.s = socket.socket(socket.AF_UNIX); self.s.connect(sock); self.f = self.s.makefile("rw"); self.n = 0
        self.me = self.call("hello", {"client": "tier1-test"})["you"]

    def call(self, method, params):
        self.n += 1
        self.f.write(json.dumps({"jsonrpc": "2.0", "id": self.n, "method": method, "params": params}) + "\n"); self.f.flush()
        while True:
            row = json.loads(self.f.readline())
            if row.get("id") == self.n:
                if "error" in row:
                    raise RuntimeError(row["error"])
                return row["result"]

    def cmd(self, name, **args):
        return self.call("command.invoke", {"name": name, "args": args})
