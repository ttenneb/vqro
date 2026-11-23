#!/usr/bin/python3
"""End to end: private vqro core + vqro-rex with its vqro-owned Rex server, both under one private /tmp root.

Usage: tests/e2e_rex.py TARGET_DIR   (debug binaries: TARGET_DIR/debug/{vqro,vqro-rex}; Rex from $REX or /usr/bin/rex)
Rex is proprietary and optional: without it this prints SKIP and exits 77 (check.sh's SKIP). Never touches the user's Rex server: the
core, provider and Rex run with a private HOME, XDG_RUNTIME_DIR and state directory, and the owned Rex server's
sockets live under that root. Stops only what it started. Prints PASS/FAIL lines and exits non-zero on failure.
"""
import base64, json, os, queue, shutil, socket, subprocess, sys, tempfile, threading, time

TARGET = sys.argv[1]
VQRO, PREX = (os.path.join(TARGET, "debug", b) for b in ("vqro", "vqro-rex"))
REX = os.environ.get("REX", "/usr/bin/rex")
if not os.access(REX, os.X_OK):
    print(f"SKIP rex provider e2e: no Rex at {REX} (optional, proprietary)")
    sys.exit(77)  # scripts/check.sh: an intentional optional-dependency SKIP, never a PASS
R = tempfile.mkdtemp(prefix="prex", dir="/tmp")
for d in ("home", "st", "xst", "run", "cfg", "ext"):
    os.makedirs(os.path.join(R, d))
os.chmod(os.path.join(R, "run"), 0o700)
ENV = {"HOME": f"{R}/home", "PATH": "/usr/bin:/bin", "TERM": "xterm-256color", "SHELL": "/bin/sh",
       "VQRO_STATE_DIR": f"{R}/st", "VQRO_SOCKET": f"{R}/p.sock", "VQRO_EXTENSIONS_DIR": f"{R}/ext",
       "XDG_STATE_HOME": f"{R}/xst", "XDG_RUNTIME_DIR": f"{R}/run", "XDG_CONFIG_HOME": f"{R}/cfg",
       "VQRO_HERDR": '["/bin/false"]'}
REXDIR = f"{R}/st/rex"
results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(("PASS " if ok else "FAIL ") + name + (f"  [{detail}]" if detail and not ok else ""), flush=True)
    return ok


class Client:
    def __init__(self):
        self.s = socket.socket(socket.AF_UNIX)
        self.s.connect(ENV["VQRO_SOCKET"])
        self.f = self.s.makefile("wb", buffering=0)
        self.r = self.s.makefile("rb")
        self.n, self.replies, self.notes, self.lock = 0, {}, queue.Queue(), threading.Lock()
        self.cv = threading.Condition()
        threading.Thread(target=self.read, daemon=True).start()
        self.call("hello", {"client": "e2e-rex"})

    def read(self):
        for line in self.r:
            msg = json.loads(line)
            if "method" in msg:
                if "id" not in msg:
                    self.notes.put(msg)
            else:
                with self.cv:
                    self.replies[msg["id"]] = msg
                    self.cv.notify_all()

    def call(self, method, params=None, timeout=20):
        with self.lock:
            self.n += 1
            ident = self.n
            self.f.write((json.dumps({"jsonrpc": "2.0", "id": ident, "method": method, "params": params or {}}) + "\n").encode())
        with self.cv:
            if not self.cv.wait_for(lambda: ident in self.replies, timeout):
                raise TimeoutError(method)
            msg = self.replies.pop(ident)
        if "error" in msg:
            raise RuntimeError(f"{method}: {msg['error']}")
        return msg["result"]

    def notify(self, method, params):
        with self.lock:
            self.f.write((json.dumps({"jsonrpc": "2.0", "method": method, "params": params}) + "\n").encode())

    def stream_until(self, sid, needle, timeout=10):
        buf, end = b"", time.time() + timeout
        while time.time() < end:
            try:
                m = self.notes.get(timeout=0.2)
            except queue.Empty:
                continue
            p = m.get("params", {})
            if m["method"] == "stream.data" and p.get("stream") == sid:
                buf = (buf + base64.b64decode(p["data"]))[-262144:]
                if needle.encode() in buf:
                    return True, buf
        return False, buf


def wait(pred, timeout=20, step=0.2):
    end = time.time() + timeout
    while time.time() < end:
        try:
            v = pred()
            if v:
                return v
        except Exception:
            pass
        time.sleep(step)
    return None


def providers(c):
    return c.call("workspace.snapshot")["providers"]


def resources(c):
    return [r for r in c.call("resource.list")["resources"] if r.get("provider") == "rex"]


def by_id(c, rid):
    return next((r for r in c.call("resource.list")["resources"] if r["id"] == rid), None)


def rex(*args):
    """The private owned Rex server only (REX_RUNTIME_DIR under this root; never autostart elsewhere)."""
    env = dict(ENV, REX_RUNTIME_DIR=REXDIR)
    return subprocess.run([REX, "--autostart=false", *args], env=env, capture_output=True, text=True)


def provider_pid(core):
    found = []
    for thread in os.listdir(f"/proc/{core.pid}/task"):
        try:
            children = open(f"/proc/{core.pid}/task/{thread}/children").read().split()
        except OSError:
            continue
        for child in children:
            try:
                if os.path.realpath(f"/proc/{child}/exe") == os.path.realpath(PREX):
                    found.append(int(child))
            except OSError:
                pass
    return found


USER_REX = f"/run/user/{os.getuid()}/rex"
user_rex_before = os.path.exists(USER_REX)
procs = []
try:
    core = subprocess.Popen([VQRO, "serve", "--fake"], env=ENV, stdout=open(f"{R}/serve.log", "w"), stderr=subprocess.STDOUT)
    procs.append(core)
    assert wait(lambda: os.path.exists(ENV["VQRO_SOCKET"]) and Client(), 10), "core did not start"
    c = Client()
    c.call("events.subscribe", {"partial_resources": False})
    # The declaration the optional extension's README gives (manifest by path, relative to the extensions dir).
    os.makedirs(f"{R}/ext/optional/rex")
    shutil.copy(os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "extensions", "optional", "rex", "rex.json"), f"{R}/ext/optional/rex/")
    c.call("workspace.apply", {"ops": [{"op": "set", "key": "extension/rex",
        "value": {"argv": [PREX, "--rex", REX, "--name", "rex"], "kind": "provider", "manifest": "optional/rex/rex.json"}}], "note": "e2e rex"})
    pv = wait(lambda: (lambda p: p if p.get("rex", {}).get("state") == "connected" and p["rex"].get("synced") else None)(providers(c)), 40)
    if not check("rex provider starts its owned server, registers and syncs", bool(pv), json.dumps(providers(c))[:500]):
        raise SystemExit
    caps = pv["rex"]["capabilities"]
    check("declares stored status, exit codes, no per-terminal env, text-only input",
          caps["status.osc7501"]["stored"] is True and caps["terminal.exit"]["exit_code"] is True
          and caps["terminal.spawn"]["env"] is False and caps["terminal.input"]["bytes"] == "utf8", json.dumps(caps))
    check("owned server's socket is under the private vqro state", os.path.exists(f"{REXDIR}/server.sock"))
    instance = pv["rex"]["instance"]
    ext = c.call("extension.list")["extensions"].get("rex") or {}
    check("its manifest loads from the optional directory", (ext.get("manifest") or {}).get("title") == "Rex backend" and not ext.get("manifest_error"),
          json.dumps({k: ext.get(k) for k in ("manifest_source", "manifest_error")}))

    # env is refused before anything starts.
    try:
        r = c.call("resource.spawn", {"provider": "rex", "argv": ["/bin/sh"], "env": {"X": "1"}})
        refused = r.get("outcome") == "rejected"
    except RuntimeError as e:
        refused = "environment" in str(e)
    check("spawn with env is rejected (nothing started)", refused and not resources(c))

    # A program that printed before anyone attached: the attach repaint shows it.
    r = c.call("resource.spawn", {"provider": "rex", "argv": ["/bin/sh", "-c", "printf 'early-\\033[31mred\\033[0m-marker\\n'; exec /bin/sh"],
                                  "name": "e2e-rex", "cols": 80, "rows": 24})
    rid = r["resource"]["id"]
    check("spawn through the core", r.get("outcome") == "observed" and r["resource"]["provider"] == "rex", json.dumps(r)[:300])
    time.sleep(0.5)
    a = c.call("resource.attach", {"resource": rid, "cols": 80, "rows": 24})
    sid = a["stream"]
    check("attach starts with a labelled repaint", a.get("initial") == "repaint", json.dumps(a))
    found, tail = c.stream_until(sid, "marker")
    check("the repaint shows output from before the attach, colour kept", found and b"red" in tail and b"\x1b[" in tail,
          tail[-300:].decode(errors="replace"))
    check("Rex's own metadata never reaches the viewer", b"\x1b_rex1;" not in tail)
    c.notify("stream.input", {"stream": sid, "text": "echo e2e-$((6*7))\r"})
    found, tail = c.stream_until(sid, "e2e-42")
    check("input reaches the program and live output comes back", found, tail[-200:].decode(errors="replace"))
    resized = c.call("stream.resize", {"stream": sid, "cols": 100, "rows": 30})
    assert resized["outcome"] == "dispatched", resized
    time.sleep(0.3)
    c.notify("stream.input", {"stream": sid, "text": "/usr/bin/stty size\r"})
    found, tail = c.stream_until(sid, "30 100")
    check("resize applies", found, tail[-200:].decode(errors="replace"))
    c.notify("stream.input", {"stream": sid, "text": "printf '\\033]7501;state=working:app=e2e\\007'\r"})
    st = wait(lambda: (by_id(c, rid).get("status") or {}).get("state") == "working", 10)
    check("OSC 7501 status (stored by Rex) reaches the resource", bool(st), json.dumps(by_id(c, rid))[:400])
    c.notify("stream.input", {"stream": sid, "text": "exit 7\r"})
    done = wait(lambda: (lambda x: x if x is None or x["state"] == "finished" else None)(by_id(c, rid)) or not by_id(c, rid), 10)
    check("a normal exit: the terminal finishes or goes (Rex closes it)", bool(done))
    # A quick failure stays open in Rex (so its error can be read): finished, with the exit code.
    r = c.call("resource.spawn", {"provider": "rex", "argv": ["/bin/sh", "-c", "exit 3"]})
    qid = r["resource"]["id"]
    fin = wait(lambda: (lambda x: x if x and x["state"] == "finished" else None)(by_id(c, qid)), 10)
    check("program exit code reported", bool(fin) and (fin.get("exit") or {}).get("exit_code") == 3, json.dumps(by_id(c, qid))[:300])

    # Kill.
    r = c.call("resource.spawn", {"provider": "rex", "argv": ["/bin/sleep", "600"]})
    kid = r["resource"]["id"]
    k = c.call("resource.kill", {"resource": kid})
    check("kill", k.get("outcome") in ("accepted", "observed"), json.dumps(k)[:300])
    check("killed terminal finishes or goes", bool(wait(lambda: (lambda x: x is None or x["state"] == "finished")(by_id(c, kid)), 10)))

    # Provider restart: same ids; Rex keeps running.
    r = c.call("resource.spawn", {"provider": "rex", "argv": ["/bin/sleep", "600"]})
    keep = r["resource"]["id"]
    pids = provider_pid(core)
    assert len(pids) == 1, pids
    os.kill(pids[0], 9)
    check("provider killed -> its resources unavailable", bool(wait(lambda: by_id(c, keep).get("available") is False, 10)))
    check("provider restarted -> same resource id available again",
          bool(wait(lambda: by_id(c, keep).get("available") is True and providers(c)["rex"]["state"] == "connected", 30)))
    check("same instance after provider restart", providers(c)["rex"]["instance"] == instance)

    # Rex server restart: same blocks run new programs, so the old ids retire.
    status = json.loads(subprocess.run([PREX, "server", "status", "--state-dir", f"{R}/st", "--rex", REX], env=ENV,
                                       capture_output=True, text=True).stdout or "{}")
    stop = subprocess.run([PREX, "server", "stop", "--state-dir", f"{R}/st", "--rex", REX, "--expect-instance", str(status.get("instance"))],
                          env=ENV, capture_output=True, text=True)
    stopped = json.loads(stop.stdout or "{}")
    check("owned server stops only with its instance confirmed and reports checked inventory",
          stop.returncode == 0 and stopped.get("checked", {}).get("sessions", 0) >= 1
          and stopped.get("checked", {}).get("terminals", 0) >= 1 and "outside vqro" in stopped.get("warning", ""), stop.stderr)
    pids = provider_pid(core)
    for p in pids:
        os.kill(p, 9)  # the supervisor restarts it, which starts the owned server again
    back = wait(lambda: (lambda p: p if p.get("state") == "connected" and p.get("synced") else None)(providers(c).get("rex", {})), 40)
    check("after a server restart the provider registers again", bool(back))
    check("old resource id retired (the block runs a new program)", bool(wait(lambda: by_id(c, keep) is None, 10)),
          json.dumps(by_id(c, keep))[:300])

    # The user's own server (--rex-socket): an existing session shows up; vqro never starts or stops that server.
    OWN = f"{R}/own"
    os.makedirs(OWN, mode=0o700)
    own_env = dict(ENV, REX_RUNTIME_DIR=OWN)
    started = subprocess.run([REX, "--autostart=false", "server", "start", "--state-dir", f"{OWN}/state"], env=own_env, capture_output=True, text=True)
    check("a second private Rex server stands in for the user's own", started.returncode == 0, started.stderr)
    made = subprocess.run([REX, "--autostart=false", "new", "mine", "--shell=none", "--", "/bin/sleep", "600"], env=own_env, capture_output=True, text=True)
    c.call("workspace.apply", {"ops": [{"op": "set", "key": "extension/rexown",
        "value": {"argv": [PREX, "--rex", REX, "--rex-socket", f"{OWN}/server.sock", "--name", "rexown"]}}], "note": "e2e rex own"})
    own = wait(lambda: [r for r in c.call("resource.list")["resources"] if r.get("provider") == "rexown"], 30)
    check("its existing terminal appears as a resource", bool(own) and made.returncode == 0, made.stderr)
    c.call("workspace.apply", {"ops": [{"op": "delete", "key": "extension/rexown"}], "note": "e2e rex own off"})
    time.sleep(1)
    alive = subprocess.run([REX, "--autostart=false", "ls", "--json"], env=own_env, capture_output=True, text=True)
    check("turning the provider off leaves that server and its sessions running", alive.returncode == 0 and "mine" in alive.stdout, alive.stderr)
    subprocess.run([REX, "--autostart=false", "server", "stop"], env=own_env, capture_output=True)
finally:
    for p in procs:
        p.terminate()
        try:
            p.wait(10)
        except subprocess.TimeoutExpired:
            p.kill()
    st = subprocess.run([PREX, "server", "status", "--state-dir", f"{R}/st", "--rex", REX], env=ENV, capture_output=True, text=True)
    try:
        inst = json.loads(st.stdout).get("instance")
    except ValueError:
        inst = None
    if inst:
        subprocess.run([PREX, "server", "stop", "--state-dir", f"{R}/st", "--rex", REX, "--expect-instance", inst], env=ENV, capture_output=True)
    time.sleep(1.5)
    left = []
    for pid in os.listdir("/proc"):
        if pid.isdigit():
            try:
                if R.encode() in open(f"/proc/{pid}/environ", "rb").read() or R.encode() in open(f"/proc/{pid}/cmdline", "rb").read():
                    left.append(pid)
            except OSError:
                pass
    check("default Rex runtime directory existence unchanged (not an access audit)", os.path.exists(USER_REX) == user_rex_before)
    if not check("nothing left running", not left, " ".join(left)):
        for pid in left:
            try:
                print(f"  diag left {pid}: {open(f'/proc/{pid}/cmdline', 'rb').read().replace(b'\0', b' ')[:200]!r}", flush=True)
            except OSError:
                pass
    for pid in left:
        try:
            os.kill(int(pid), 9)
        except OSError:
            pass
    print(f"root {R}; passed {sum(results)} failed {len(results) - sum(results)}")
    failed = not all(results) or sys.exc_info()[0] is not None
    if not failed and os.environ.get("E2E_KEEP") is None:
        shutil.rmtree(R, ignore_errors=True)
    elif failed:
        print(f"kept failed run root {R} (remove it when done)", flush=True)
sys.exit(0 if all(results) else 1)
