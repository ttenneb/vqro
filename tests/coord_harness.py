"""Shared harness for the tier-1 plugin tests: a private vqro core plus a coordination inbox:
- `fake`: `tests/fixtures/fake_inbox.py`;
- `k2`: K2's code via `tests/fixtures/coord_k2.py` with stand-ins for C1/C2 (development only);
- `k2core`: K2 as the core's supervised `coord` extension on a core with C1 (operational domains) and C2
  (principals), e.g. the test-only agent-core-int binary. No stand-ins: the qualification path.

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
    PLUGIN = "p:plugin"  # the plugin's principal (k2core: bound by VQRO_PRINCIPAL at the plugin's hello)

    def __init__(self, root, vqro, *, inbox="fake", coord_ext=None):
        self.root, self.kind, self.vqro = root, inbox, vqro
        os.makedirs(root, exist_ok=True)
        self.sock = os.path.join(root, "v.sock")
        self.inbox_log = os.path.join(root, "inbox.jsonl")
        self.env = {"HOME": os.path.join(root, "home-core"), "PATH": "/usr/bin:/bin", "XDG_RUNTIME_DIR": os.path.join(root, "xrun"),
                    "VQRO_STATE_DIR": os.path.join(root, "st"), "VQRO_SOCKET": self.sock,
                    "VQRO_EXTENSIONS_DIR": os.path.join(root, "noext"), "VQRO_HERDR": '["/bin/false"]',
                    "HC_SOCKET": os.path.join(root, "h.sock"), "HC_SESSION": "tier1"}
        for d in ("home-core", "xrun", "st", "noext"):
            os.makedirs(os.path.join(root, d), mode=0o700, exist_ok=True)
        if inbox == "k2core":
            self.env["VQRO_EXTENSIONS_DIR"] = self.k2_extensions(coord_ext)
        self.procs = []
        self.spawn([vqro, "serve", "--fake"], env=self.env, stdout=open(os.path.join(root, "core.log"), "w"), stderr=subprocess.STDOUT)
        while not os.path.exists(self.sock):
            time.sleep(0.05)
        if inbox == "k2":
            self.spawn([sys.executable, os.path.join(HERE, "fixtures", "coord_k2.py")], env=dict(self.env, COORD_EXT=coord_ext),
                       stderr=open(os.path.join(root, "inbox.err"), "w"))
        elif inbox == "fake":
            self.spawn([sys.executable, os.path.join(HERE, "fixtures", "fake_inbox.py"), self.inbox_log],
                       env=dict(self.env, VQRO_EXT_PATH=os.path.join(HERE, "..", "extensions")), stderr=open(os.path.join(root, "inbox.err"), "w"))
        self.c = Client(self.sock, owner=inbox == "k2core")
        end = time.time() + 60
        while not any(x.get("name") == "coord.inbox.take" for x in self.c.call("command.list", {})["commands"]):
            if time.time() > end:
                raise RuntimeError("coord.inbox.take never registered; see core.log / inbox.err")
            time.sleep(0.1)
        if inbox == "k2core":
            while self.c.cmd("coord.status").get("state") != "ready" and time.time() < end:
                time.sleep(0.2)
            self.status = self.c.cmd("coord.status")
        self.n = 0
        if inbox == "k2":
            self.c.cmd("fixture.principal", author=self.c.me, principal="p:owner")
            self.c.call("workspace.apply", {"ops": [{"op": "set", "key": "doc/principals/owner", "value": "p:owner"}]})

    def restart_core(self):
        """Kill and restart the core and the fake inbox on the same endpoint (state dir kept). Fake inbox only: its
        records are in memory, so mail sent before the restart is gone, like an inbox owner replacement."""
        assert self.kind == "fake"
        for p in self.procs[:2]:
            p.kill()
            p.wait()
        if os.path.exists(self.sock):
            os.unlink(self.sock)
        self.procs[0] = subprocess.Popen([self.vqro, "serve", "--fake"], env=self.env, stdout=open(os.path.join(self.root, "core2.log"), "w"), stderr=subprocess.STDOUT)
        while not os.path.exists(self.sock):
            time.sleep(0.05)
        self.procs[1] = subprocess.Popen([sys.executable, os.path.join(HERE, "fixtures", "fake_inbox.py"), self.inbox_log],
                                         env=dict(self.env, VQRO_EXT_PATH=os.path.join(HERE, "..", "extensions")), stderr=open(os.path.join(self.root, "inbox2.err"), "w"))
        self.c = Client(self.sock)
        while not any(x.get("name") == "coord.inbox.take" for x in self.c.call("command.list", {})["commands"]):
            time.sleep(0.1)

    def k2_extensions(self, coord_ext):
        """A private extension directory with only `coord` (K2) declared, copied from the given coord-ext tree."""
        import json, shutil
        dest = os.path.join(self.root, "extensions")
        os.makedirs(os.path.join(dest, "manifests"), mode=0o700, exist_ok=True)
        for name in os.listdir(coord_ext):
            if name.endswith(".py"):
                shutil.copy(os.path.join(coord_ext, name), dest)
        shutil.copy(os.path.join(coord_ext, "manifests", "coord.json"), os.path.join(dest, "manifests"))
        defaults = json.load(open(os.path.join(coord_ext, "defaults.json")))
        defaults["extensions"] = [e for e in defaults["extensions"] if e["name"] == "coord"]
        json.dump(defaults, open(os.path.join(dest, "defaults.json"), "w"))
        return dest

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
        if self.kind in ("k2", "k2core"):
            self.c.call("workspace.apply", {"ops": [
                {"op": "set", "key": f"doc/agents/grants/plugin-{rid}", "value": {"who": self.PLUGIN, "on": "agent:" + rid, "can": ["receive"]}},
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
        if self.kind in ("k2", "k2core"):
            args["submission"] = op + ":sub"
        if render is not None:
            args["render"] = render
        return self.c.cmd("coord.inbox.send", **args)["delivery"]

    def edit(self, delivery, content):
        rec = self.get(delivery)
        args = {"op": self.op(), "delivery": delivery, "content": content}
        if self.kind in ("k2", "k2core"):
            args["expect"] = rec.get("revision")
        return self.c.cmd("coord.inbox.edit", **args)

    def get(self, delivery):
        return self.c.cmd("coord.inbox.get", delivery=delivery)

    def statuses(self):
        if not os.path.exists(self.inbox_log):
            return []
        return [json.loads(l) for l in open(self.inbox_log) if '"event": "status"' in l]


class Client:
    def __init__(self, sock, owner=False):
        self.s = socket.socket(socket.AF_UNIX); self.s.connect(sock); self.f = self.s.makefile("rw"); self.n = 0
        hello = {"client": "tier1-test", **({"identity": {"kind": "owner"}} if owner else {})}
        self.me = self.call("hello", hello)["you"]

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
