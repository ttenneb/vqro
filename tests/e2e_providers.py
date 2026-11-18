#!/usr/bin/python3
"""End to end: private vqro core + vqro-herdr (private herdr-core) + vqro-tmux (private tmux).

Usage: tests/e2e_providers.py TARGET_DIR   (debug binaries: TARGET_DIR/debug/{vqro,vqro-herdr,vqro-tmux})
Everything lives under a short /tmp root; PATH is empty for the core, providers, hc and tmux.
Stops only what it started. Prints PASS/FAIL lines and exits non-zero on failure.
"""
import base64, fcntl, json, os, queue, shutil, socket, struct, subprocess, sys, tempfile, termios, threading, time

TARGET = sys.argv[1]
VQRO, PHERDR, PTMUX = (os.path.join(TARGET, "debug", b) for b in ("vqro", "vqro-herdr", "vqro-tmux"))
HC = os.path.expanduser("~/.local/bin/herdr-core")
TMUX = "/usr/bin/tmux"
R = tempfile.mkdtemp(prefix="pe2e", dir="/tmp")
for d in ("home", "st", "xst", "run", "cfg", "ext"):
    os.makedirs(os.path.join(R, d))
os.chmod(os.path.join(R, "run"), 0o700)
ENV = {"HOME": f"{R}/home", "PATH": "", "TERM": "xterm-256color", "SHELL": "/bin/sh", "VQRO_STATE_DIR": f"{R}/st",
       "VQRO_SOCKET": f"{R}/p.sock", "VQRO_EXTENSIONS_DIR": f"{R}/ext", "XDG_STATE_HOME": f"{R}/xst",
       "XDG_RUNTIME_DIR": f"{R}/run", "XDG_CONFIG_HOME": f"{R}/cfg", "HC_SOCKET": f"{R}/hc.sock", "HC_SESSION": "e2e"}
TSOCK = f"{R}/t.sock"
results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(("PASS " if ok else "FAIL ") + name + (f"  [{detail}]" if detail and not ok else ""), flush=True)
    return ok


def tmux(*args, start=False):
    flags = [] if start else ["-N"]
    return subprocess.run([TMUX, *flags, "-S", TSOCK, "-f", "/dev/null", *args], env=ENV, capture_output=True, text=True)


def tmux_pty_size(ref):
    """Observe the owned native PTY; tmux 3.7b coalesces SIGWINCH with a resize timer."""
    tty = tmux("display-message", "-p", "-t", ref, "#{pane_tty}").stdout.strip()
    fd = os.open(tty, os.O_RDONLY | os.O_NOCTTY)
    try:
        rows, cols, _, _ = struct.unpack("HHHH", fcntl.ioctl(fd, termios.TIOCGWINSZ, b"\0" * 8))
        return rows, cols
    finally:
        os.close(fd)


def hc(*args):
    out = subprocess.run([HC, *args, "--json"], env=ENV, capture_output=True, text=True)
    try:
        return json.loads(out.stdout)
    except ValueError:
        return None


class Client:
    def __init__(self):
        self.s = socket.socket(socket.AF_UNIX)
        self.s.connect(ENV["VQRO_SOCKET"])
        self.f = self.s.makefile("wb", buffering=0)
        self.r = self.s.makefile("rb")  # buffered: line reads must not be byte-at-a-time
        self.n, self.replies, self.notes, self.lock = 0, {}, queue.Queue(), threading.Lock()
        self.cv = threading.Condition()
        threading.Thread(target=self.read, daemon=True).start()
        self.call("hello", {"client": "e2e"})

    def read(self):
        for line in self.r:
            msg = json.loads(line)
            if "method" in msg:
                if "id" in msg:  # requests to us: none expected
                    continue
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
        """Collect stream.data for sid until needle appears; returns (found, tail, other notes)."""
        buf, end, other = b"", time.time() + timeout, []
        while time.time() < end:
            try:
                m = self.notes.get(timeout=0.2)
            except queue.Empty:
                continue
            p = m.get("params", {})
            if m["method"] == "stream.data" and p.get("stream") == sid:
                buf = (buf + base64.b64decode(p["data"]))[-65536:]
                if needle.encode() in buf:
                    return True, buf, other
            else:
                other.append(m)
        return False, buf, other


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


def resources(c, provider=None):
    return [r for r in c.call("resource.list")["resources"] if provider is None or r.get("provider") == provider]


procs = []
try:
    # Private tmux server (a configured server for vqro-tmux; this script owns it).
    out = tmux("new-session", "-d", "-s", "work", "-x", "100", "-y", "30", "/bin/sh", start=True)
    assert out.returncode == 0, out.stderr
    core = subprocess.Popen([VQRO, "serve", "--fake"], env=ENV, stdout=open(f"{R}/serve.log", "w"), stderr=subprocess.STDOUT)
    procs.append(core)
    assert wait(lambda: os.path.exists(ENV["VQRO_SOCKET"]) and Client(), 10), "core did not start"
    c = Client()
    c.call("events.subscribe", {"partial_resources": False})
    ops = [{"op": "set", "key": "extension/herdr", "value": {"argv": [PHERDR, "--hc", HC, "--name", "herdr"]}},
           {"op": "set", "key": "extension/tmux", "value": {"argv": [PTMUX, "--tmux", TMUX, "--tmux-socket", TSOCK, "--name", "tmux"]}}]
    c.call("workspace.apply", {"ops": ops, "note": "e2e providers"})
    pv = wait(lambda: (lambda p: p if all(p.get(n, {}).get("state") == "connected" and p[n].get("synced") for n in ("herdr", "tmux")) else None)(providers(c)), 30)
    check("both providers register and sync (two hosts)", bool(pv), json.dumps(providers(c))[:600])
    if not pv:
        raise SystemExit
    check("tmux provider declares lossy status, herdr exit codes",
          pv["tmux"]["capabilities"].get("status.osc7501", {}).get("lossy") is True and pv["herdr"]["capabilities"].get("terminal.exit", {}).get("exit_code") is True)
    instances = {n: pv[n]["instance"] for n in ("herdr", "tmux")}

    ids = {}
    for name in ("herdr", "tmux"):
        try:
            r = c.call("resource.spawn", {"provider": name, "argv": ["/bin/sh"], "env": {"E2E": name}, "name": f"e2e-{name}"})
            rid = r["resource"]["id"]
            ids[name] = rid
            check(f"{name}: spawn through the core", r.get("outcome") == "observed" and r["resource"]["provider"] == name, json.dumps(r)[:300])
            a = c.call("resource.attach", {"resource": rid, "cols": 80, "rows": 24})
            sid = a["stream"]
            check(f"{name}: attach (initial {a.get('initial')})", a.get("initial") in ("history", "repaint", "none"))
            c.notify("stream.input", {"stream": sid, "text": "echo e2e-$((6*7))-$E2E\r"})
            found, tail, _ = c.stream_until(sid, f"e2e-42-{name}")
            check(f"{name}: input reaches the program (env too) and output comes back", found, tail[-200:].decode(errors="replace"))
            resized = c.call("stream.resize", {"stream": sid, "cols": 100, "rows": 30})
            assert resized["outcome"] == "dispatched", resized
            if name == "tmux":
                # Dispatch is not settlement. Observe native PTY application before asking the shell once.
                check("tmux: native PTY resize settles", bool(wait(lambda: tmux_pty_size(r["resource"]["backend_ref"]) == (30, 100), 3, .02)))
            c.notify("stream.input", {"stream": sid, "text": "/usr/bin/stty size\r"})
            found, tail, _ = c.stream_until(sid, "30 100")
            check(f"{name}: resize applies", found, tail[-200:].decode(errors="replace"))
            k = c.call("resource.kill", {"resource": rid})
            check(f"{name}: kill", k.get("outcome") in ("accepted", "observed"), json.dumps(k)[:300])
            gone = wait(lambda: (lambda rs: True if not rs or rs[0]["state"] == "finished" else None)([x for x in resources(c) if x["id"] == rid]), 10)
            check(f"{name}: killed resource finishes or goes", bool(gone))
        except Exception as e:
            check(f"{name}: spawn/attach/input/resize/kill", False, repr(e))

    # Provider kill/restart: unavailable, then available again with the same ids.
    keep = {}
    for name in ("herdr", "tmux"):
        try:
            r = c.call("resource.spawn", {"provider": name, "argv": ["/usr/bin/sleep", "600"] if name == "herdr" else ["/bin/sh"]})
            keep[name] = r["resource"]["id"]
        except Exception as e:
            check(f"{name}: spawn a long-lived resource", False, repr(e))
    for name, rid in keep.items():
        # Find only our own core's child with this executable and private endpoint. Pids are harness facts.
        children = []
        for thread in os.listdir(f"/proc/{core.pid}/task"):
            try:
                children.extend(open(f"/proc/{core.pid}/task/{thread}/children").read().split())
            except OSError:
                pass
        program = PHERDR if name == "herdr" else PTMUX
        matches = []
        for child in children:
            try:
                if os.path.realpath(f"/proc/{child}/exe") == os.path.realpath(program) and ENV["VQRO_SOCKET"].encode() in open(f"/proc/{child}/environ", "rb").read():
                    matches.append(int(child))
            except OSError:
                pass
        assert len(matches) == 1, (name, matches)
        os.kill(matches[0], 9)
        down = wait(lambda: any(x["id"] == rid and x.get("available") is False for x in resources(c)), 10)
        check(f"{name}: provider killed -> its resources unavailable (stale)", bool(down))
        up = wait(lambda: any(x["id"] == rid and x.get("available") is True for x in resources(c)) and providers(c)[name]["state"] == "connected", 30)
        check(f"{name}: provider restarted -> same resource id available again", bool(up))
        check(f"{name}: same instance after provider restart", providers(c)[name]["instance"] == instances[name])

    # Backend restart: a new instance retires the old ids (resource.removed reason replaced/closed).
    if "tmux" in keep:
        rid = keep["tmux"]
        tmux("kill-server")
        time.sleep(1)
        tmux("new-session", "-d", "-s", "work", "-x", "100", "-y", "30", "/bin/sh", start=True)
        new = wait(lambda: (lambda p: p if p.get("state") == "connected" and p["instance"] != instances["tmux"] else None)(providers(c)["tmux"]), 40)
        check("tmux: backend restart -> provider registers a new instance", bool(new), json.dumps(providers(c).get("tmux"))[:300])
        check("tmux: old resource id retired", not any(x["id"] == rid for x in resources(c)))
    if "herdr" in keep:
        rid = keep["herdr"]
        hc("server", "stop")
        shutil.rmtree(f"{R}/xst/herdr-core/e2e", ignore_errors=True)
        new = wait(lambda: (lambda p: p if p.get("state") == "connected" and p["instance"] != instances["herdr"] else None)(providers(c)["herdr"]), 40)
        check("herdr: wiped backend -> provider registers a new instance", bool(new), json.dumps(providers(c).get("herdr"))[:300])
        check("herdr: old resource id retired", not any(x["id"] == rid for x in resources(c)))
finally:
    for p in procs:
        p.terminate()
        try:
            p.wait(10)
        except subprocess.TimeoutExpired:
            p.kill()
    tmux("kill-server")
    hc("server", "stop")
    time.sleep(1)
    left = []
    for pid in os.listdir("/proc"):
        if pid.isdigit():
            try:
                if R.encode() in open(f"/proc/{pid}/environ", "rb").read():
                    left.append(pid)
            except OSError:
                pass
    check("nothing left running", not left, " ".join(left))
    for pid in left:
        os.kill(int(pid), 9)
    print(f"root {R}; passed {sum(results)} failed {len(results) - sum(results)}")
    if os.environ.get("E2E_KEEP") is None:
        shutil.rmtree(R, ignore_errors=True)
sys.exit(0 if all(results) else 1)
