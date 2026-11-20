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
        # Each log file is the child's: the parent's copy is closed as soon as the child has it.
        with open(os.path.join(root, "core.log"), "w") as log:
            self.spawn([vqro, "serve", "--fake"], env=self.env, stdout=log, stderr=subprocess.STDOUT)
        while not os.path.exists(self.sock):
            if time.time() > end or self.procs[0].poll() is not None:
                raise RuntimeError(f"core did not start (exit {self.procs[0].poll()}); see {root}/core.log")
            time.sleep(0.05)
        if inbox == "k2":
            with open(os.path.join(root, "inbox.err"), "w") as log:
                self.spawn([sys.executable, os.path.join(HERE, "fixtures", "coord_k2.py")], env=dict(self.env, COORD_EXT=coord_ext),
                           stderr=log)
        elif inbox == "fake":
            with open(os.path.join(root, "inbox.err"), "w") as log:
                self.spawn([sys.executable, os.path.join(HERE, "fixtures", "fake_inbox.py"), self.inbox_log],
                           env=dict(self.env, VQRO_EXT_PATH=os.path.join(HERE, "..", "extensions")), stderr=log)
        if inbox == "k2core":
            # Resolve the core's actual bootstrapped owner, then configure this
            # fixture's owner through an authorized public transaction. A label
            # or an arbitrary ordinary principal confers no such standing.
            bootstrap = Client(self.sock, owner=True, connect_timeout=max(1.0, end - time.time()))
            try:
                bootstrap.call("workspace.apply", {"ops": [{"op": "set", "key": "doc/principals/owner",
                                                           "value": "p:owner"}], "note": "private native pull fixture owner"})
            finally:
                bootstrap.close()
        self.c = Client(self.sock, owner=inbox == "k2core", connect_timeout=max(1.0, end - time.time()))
        self.wait_commands(end)
        if inbox == "k2core":
            # Command registration is incremental; inbox.take can precede status.
            # These are bounded read-only startup observations, never effect retries.
            while time.time() < end:
                try:
                    self.status = self.c.cmd("coord.status")
                    if self.status.get("state") == "ready":
                        break
                except RpcFailure as error:
                    if error.kind not in ("not_found", "stale_generation"):
                        raise
                time.sleep(0.1)
            else:
                raise RuntimeError("managed coordination owner never became ready")
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
        self.c.close()  # its core is gone
        with open(os.path.join(self.root, "core2.log"), "w") as log:
            self.procs[0] = subprocess.Popen([self.vqro, "serve", "--fake"], env=self.env, stdout=log,
                                             stderr=subprocess.STDOUT, start_new_session=True)
        end = time.time() + 60
        while not os.path.exists(self.sock):
            if time.time() > end or self.procs[0].poll() is not None:
                raise RuntimeError(f"restarted core did not start (exit {self.procs[0].poll()}); see {self.root}/core2.log")
            time.sleep(0.05)
        with open(os.path.join(self.root, "inbox2.err"), "w") as log:
            self.procs[1] = subprocess.Popen([sys.executable, os.path.join(HERE, "fixtures", "fake_inbox.py"), self.inbox_log],
                                             env=dict(self.env, VQRO_EXT_PATH=os.path.join(HERE, "..", "extensions")), stderr=log,
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
        with open(os.path.join(coord_ext, "defaults.json")) as file:
            defaults = json.load(file)
        # coord (K2) and agents (K3: agents.status, the tier-1 status table); nothing else
        defaults["extensions"] = [e for e in defaults["extensions"] if e["name"] in ("coord", "agents")]
        with open(os.path.join(dest, "defaults.json"), "w") as file:
            json.dump(defaults, file)
        return dest

    def spawn(self, argv, **kw):
        """Each process leads its own group, so stop() also reaches what it started (the core's extensions)."""
        p = subprocess.Popen(argv, start_new_session=True, **kw)
        self.procs.append(p)
        return p

    def stop(self):
        if getattr(self, "c", None) is not None:
            self.c.close()
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
        with open(self.inbox_log) as file:
            return [json.loads(l) for l in file if '"event": "status"' in l]


class RpcFailure(RuntimeError):
    def __init__(self, error):
        self.kind = (error.get("data") or {}).get("kind", "unknown")
        super().__init__(error)


CALL_TIMEOUT = 60.0  # seconds a harness call waits for its reply: generous, but a silent peer cannot hang a test


class Client:
    """A JSON-RPC client for tests. Every call has a deadline (`timeout`, default CALL_TIMEOUT): a peer that never
    answers fails the call with TimeoutError and closes the connection; nothing is retried (the outcome is unknown)."""

    def __init__(self, sock, owner=False, principal=None, timeout=CALL_TIMEOUT, connect_timeout=None):
        self.s = socket.socket(socket.AF_UNIX); self.f = None; self.n = 0; self.timeout = timeout
        first = timeout if connect_timeout is None else connect_timeout  # connect and hello (a startup bound)
        try:
            self.s.settimeout(first)
            self.s.connect(sock); self.f = self.s.makefile("rw")
            identity = ({"kind": "principal", "principal": principal} if principal is not None else
                        {"kind": "owner"} if owner else None)
            hello = {"client": "tier1-test", **({"identity": identity} if identity is not None else {})}
            self.me = self.call("hello", hello, timeout=first)["you"]
        except BaseException:
            self.close()  # a failed connect or hello leaves nothing open
            raise

    def close(self):
        for handle in (self.f, self.s):
            try:
                if handle is not None:
                    handle.close()
            except OSError:
                pass

    def call(self, method, params, timeout=None):
        self.n += 1
        limit = self.timeout if timeout is None else timeout
        deadline = time.monotonic() + limit
        try:
            self.s.settimeout(limit)
            self.f.write(json.dumps({"jsonrpc": "2.0", "id": self.n, "method": method, "params": params}) + "\n"); self.f.flush()
            while True:
                self.s.settimeout(max(0.001, deadline - time.monotonic()))
                line = self.f.readline()
                if not line:
                    raise ConnectionError("core closed the connection")
                row = json.loads(line)
                if row.get("id") == self.n:
                    if "error" in row:
                        raise RpcFailure(row["error"])
                    return row["result"]
        except (socket.timeout, TimeoutError):
            self.close()  # a partly read reply leaves the stream unusable
            raise TimeoutError(f"{method}: no reply within {limit:g} s (connection closed; not retried, outcome unknown)") from None

    def cmd(self, name, **args):
        return self.call("command.invoke", {"name": name, "args": args})
