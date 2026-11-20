"""Shared harness for the tier-1 plugin tests: a private vqro core plus a coordination inbox:
- `fake`: `tests/fixtures/fake_inbox.py`;
- `k2`: K2's code via `tests/fixtures/coord_k2.py` with stand-ins for C1/C2 (development only);
- `k2core`: K2 as the core's supervised `coord` extension on a core with C1 (operational domains) and C2
  (principals): main has both. No stand-ins: the qualification path.

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
import signal
import socket
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))


def txt(s):
    return [{"type": "text", "text": s}]


def pull_declaration(runtime):
    """Launcher declarations for these pinned native plugin paths, not inferred precision."""
    points = {point: {"precision": "exact"} for point in ("turn", "settle", "idle")}
    if runtime == "pi":
        framing = {point: {"role": "user", "frame": "sender_header"} for point in points}
        return {"points": points, "insert": "user", "evidence": "entry", "framing": framing}
    if runtime != "claude":
        raise ValueError("unknown native pull fixture")
    points["idle"]["precision"] = "approximate"
    framing = {point: {"role": "user", "frame": "system-reminder", "authority": "above_user"}
               for point in ("turn", "settle")}
    framing["idle"] = {"role": "user", "frame": "SYSTEM NOTIFICATION - NOT USER INPUT", "authority": "above_user"}
    return {"points": points, "insert": "context", "evidence": "none", "framing": framing}


def finish_framing(harness, results, failures):
    """Keep actual model logs and public declaration readbacks; callers grade them independently."""
    declarations = []
    for agent in harness.declarations:
        row = harness.c.call("workspace.get", {"key": "doc/agents/terminals/" + agent})
        declarations.append({"agent": agent, "entry": row})
    results["public_declarations"] = declarations
    with open(os.path.join(harness.root, "framing-result.json"), "w") as output:
        json.dump(results, output, indent=2)
    harness.stop()
    raise SystemExit(1 if failures else 0)


class Harness:
    PLUGIN = "p:plugin"  # the plugin's principal (k2core: bound by VQRO_PRINCIPAL at the plugin's hello)

    def __init__(self, root, vqro, *, inbox="fake", coord_ext=None, startup_s=60):
        self.root, self.kind, self.vqro = root, inbox, vqro
        self.procs = []
        try:
            self.start(coord_ext, startup_s)
        except BaseException:
            # A failed start must not leave the core (or the extensions it supervises) running: nobody would stop
            # them, because the caller never gets a Harness to call stop() on.
            self.stop()
            raise

    def start(self, coord_ext, startup_s):
        root, inbox, vqro = self.root, self.kind, self.vqro
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
        end = time.time() + startup_s
        self.declarations = {}
        self.spawn([vqro, "serve", "--fake"], env=self.env, stdout=open(os.path.join(root, "core.log"), "w"), stderr=subprocess.STDOUT)
        while not os.path.exists(self.sock):
            if time.time() > end or self.procs[0].poll() is not None:
                raise RuntimeError(f"core did not start (exit {self.procs[0].poll()}); see {root}/core.log")
            time.sleep(0.05)
        if inbox == "k2":
            self.spawn([sys.executable, os.path.join(HERE, "fixtures", "coord_k2.py")], env=dict(self.env, COORD_EXT=coord_ext),
                       stderr=open(os.path.join(root, "inbox.err"), "w"))
        elif inbox == "fake":
            self.spawn([sys.executable, os.path.join(HERE, "fixtures", "fake_inbox.py"), self.inbox_log],
                       env=dict(self.env, VQRO_EXT_PATH=os.path.join(HERE, "..", "extensions")), stderr=open(os.path.join(root, "inbox.err"), "w"))
        if inbox == "k2core":
            # Product cores do not bootstrap an owner for a test. Use a real raw
            # recovery transaction before requesting the configured owner identity.
            bootstrap = Client(self.sock)
            bootstrap.call("workspace.apply", {"ops": [{"op": "set", "key": "doc/principals/owner",
                                                       "value": "p:owner"}], "note": "private native pull fixture owner"})
            bootstrap.f.close(); bootstrap.s.close()
        self.c = Client(self.sock, owner=inbox == "k2core")
        self.wait_commands(end)
        if inbox == "k2core":
            while self.c.cmd("coord.status").get("state") != "ready" and time.time() < end:
                time.sleep(0.2)
            self.status = self.c.cmd("coord.status")
        self.n = 0
        self.declared = {}
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
        self.procs[0] = subprocess.Popen([self.vqro, "serve", "--fake"], env=self.env, stdout=open(os.path.join(self.root, "core2.log"), "w"),
                                         stderr=subprocess.STDOUT, start_new_session=True)
        while not os.path.exists(self.sock):
            time.sleep(0.05)
        self.procs[1] = subprocess.Popen([sys.executable, os.path.join(HERE, "fixtures", "fake_inbox.py"), self.inbox_log],
                                         env=dict(self.env, VQRO_EXT_PATH=os.path.join(HERE, "..", "extensions")), stderr=open(os.path.join(self.root, "inbox2.err"), "w"),
                                         start_new_session=True)
        self.c = Client(self.sock)
        self.wait_commands(time.time() + 60)
        for rid, pull in self.declared.items():  # the launcher's declarations are workspace state, not inbox state
            self.c.cmd("fixture.declare", agent=rid, pull=pull)

    def wait_commands(self, end):
        """Every command the tests use: an inbox registers them one by one, so one name is not readiness."""
        needed = {"coord.inbox." + n for n in ("window", "take", "report", "reconcile", "op", "send", "get", "resolve")}
        needed |= {"coord.scope", "coord.status"} if self.kind == "k2core" else {"fixture.declare"} if self.kind == "fake" else set()
        while True:
            missing = needed - {x.get("name") for x in self.c.call("command.list", {})["commands"]}
            if not missing:
                return
            if time.time() > end:
                raise RuntimeError(f"inbox commands never registered: {sorted(missing)}; see core.log / inbox.err")
            time.sleep(0.1)

    def k2_extensions(self, coord_ext):
        """A private extension directory with only `coord` (K2) and `agents` (K3) declared, copied from the given tree."""
        import json, shutil
        dest = os.path.join(self.root, "extensions")
        os.makedirs(os.path.join(dest, "manifests"), mode=0o700, exist_ok=True)
        for name in os.listdir(coord_ext):
            if name.endswith(".py"):
                shutil.copy(os.path.join(coord_ext, name), dest)
        for name in ("coord", "agents"):
            shutil.copy(os.path.join(coord_ext, "manifests", name + ".json"), os.path.join(dest, "manifests"))
        defaults = json.load(open(os.path.join(coord_ext, "defaults.json")))
        # coord (K2) and agents (K3: agents.status, the tier-1 status table); nothing else
        defaults["extensions"] = [e for e in defaults["extensions"] if e["name"] in ("coord", "agents")]
        json.dump(defaults, open(os.path.join(dest, "defaults.json"), "w"))
        return dest

    def spawn(self, argv, **kw):
        """Each process leads its own group, so stop() also reaches what it started (the core's extensions)."""
        p = subprocess.Popen(argv, start_new_session=True, **kw)
        self.procs.append(p)
        return p

    def stop(self):
        for p in self.procs:
            try:
                os.killpg(p.pid, signal.SIGKILL)  # the whole group, even if its leader already exited
            except (ProcessLookupError, PermissionError):
                if p.poll() is None:  # not a group leader (a process a test added): just the process
                    p.kill()
            p.wait()

    def op(self):
        self.n += 1
        epoch = self.c.cmd("coord.inbox.window")["epoch"]
        return f"{epoch}:test:{self.n}"

    def agent(self, pull):
        """A terminal endpoint (fake backend) declared for pull; returns its rN."""
        # As a launcher does: tag the endpoint with its real plugin principal.
        rid = self.c.call("resource.spawn", {"argv": ["sh"], "env": {"VQRO_PRINCIPAL": self.PLUGIN}})["resource"]["id"]
        self.declarations[rid] = pull
        assert isinstance(pull.get("points"), dict), "pull.points is a map {point: {precision}} (coordination-v0 §7)"
        if self.kind == "fake":
            self.declared[rid] = pull
            self.c.cmd("fixture.declare", agent=rid, pull=pull)
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

    def withdraw(self, delivery):
        args = {"op": self.op(), "delivery": delivery}
        if self.kind in ("k2", "k2core"):
            args["expect"] = self.get(delivery).get("revision")
        return self.c.cmd("coord.inbox.withdraw", **args)

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
