#!/usr/bin/python3
"""Opt-in TUI smoke over wire providers: the real `prim tui` (in an outer tmux harness) shows and drives
terminals that live in prim-herdr (private herdr-core) and prim-tmux (its private tmux server).

Usage: tests/e2e_tui_providers.py TARGET_DIR   (debug binaries in TARGET_DIR/debug)
Private /tmp root; providers and herdr-core run with an empty PATH; default extensions get python3 only.
Stops only what it started.
"""
import json, os, shutil, socket, subprocess, sys, tempfile, time

TARGET = sys.argv[1]
PRIM, PHERDR, PTMUX = (os.path.join(TARGET, "debug", b) for b in ("prim", "prim-herdr", "prim-tmux"))
HC = os.path.expanduser("~/.local/bin/herdr-core")
REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
R = tempfile.mkdtemp(prefix="pe2t", dir="/tmp")
for d in ("home", "st", "xst", "run", "cfg", "ext", "bin"):
    os.makedirs(os.path.join(R, d))
os.chmod(os.path.join(R, "run"), 0o700)
for f in os.listdir(os.path.join(REPO, "extensions")):
    if f.endswith(".py") or f == "defaults.json":
        shutil.copy(os.path.join(REPO, "extensions", f), os.path.join(R, "ext", f))
os.symlink(shutil.which("python3") or "/usr/bin/python3", os.path.join(R, "bin", "python3"))
ENV = {"HOME": f"{R}/home", "PATH": f"{R}/bin", "TERM": "xterm-256color", "SHELL": "/bin/sh", "PRIM_STATE_DIR": f"{R}/st",
       "PRIM_SOCKET": f"{R}/p.sock", "PRIM_EXTENSIONS_DIR": f"{R}/ext", "XDG_STATE_HOME": f"{R}/xst",
       "XDG_RUNTIME_DIR": f"{R}/run", "XDG_CONFIG_HOME": f"{R}/cfg", "HC_SESSION": "tui", "LANG": "C.UTF-8"}
HARNESS = ["/usr/bin/tmux", "-S", f"{R}/harness.sock", "-f", "/dev/null"]
results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(("PASS " if ok else "FAIL ") + name + (f"  [{detail}]" if detail and not ok else ""), flush=True)


def call(method, params=None):
    s = socket.socket(socket.AF_UNIX)
    s.connect(ENV["PRIM_SOCKET"])
    f = s.makefile("rwb", buffering=0)
    for i, (m, p) in enumerate([("hello", {"client": "e2e-tui"}), (method, params or {})]):
        f.write((json.dumps({"jsonrpc": "2.0", "id": i, "method": m, "params": p}) + "\n").encode())
        while True:
            msg = json.loads(f.readline())
            if msg.get("id") == i:
                break
    s.close()
    if "error" in msg:
        raise RuntimeError(f"{method}: {msg['error']}")
    return msg["result"]


def wait(pred, timeout=20):
    end = time.time() + timeout
    while time.time() < end:
        try:
            v = pred()
            if v:
                return v
        except Exception:
            pass
        time.sleep(0.3)
    return None


def screen():
    return subprocess.run([*HARNESS, "capture-pane", "-p"], capture_output=True, text=True).stdout


def keys(*k, literal=False):
    subprocess.run([*HARNESS, "send-keys", *(["-l"] if literal else []), *k])


core = None
try:
    core = subprocess.Popen([PRIM, "serve", "--fake"], env=ENV, stdout=open(f"{R}/serve.log", "w"), stderr=subprocess.STDOUT)
    wait(lambda: os.path.exists(ENV["PRIM_SOCKET"]), 10)
    call("workspace.apply", {"note": "providers", "ops": [
        {"op": "set", "key": "extension/herdr", "value": {"argv": ["/usr/bin/env", "PATH=", PHERDR, "--hc", HC, "--name", "herdr"]}},
        {"op": "set", "key": "extension/tmux", "value": {"argv": ["/usr/bin/env", "PATH=", PTMUX, "--tmux", "/usr/bin/tmux", "--name", "tmux"]}}]})
    ok = wait(lambda: all(call("workspace.snapshot")["providers"].get(n, {}).get("state") == "connected" for n in ("herdr", "tmux")), 30)
    check("providers connected (with default extensions running)", bool(ok))
    with open(f"{R}/tui.sh", "w") as f:
        f.write("".join(f"export {k}='{v}'\n" for k, v in ENV.items()) + f"exec {PRIM} tui\n")
    subprocess.run([*HARNESS, "new-session", "-d", "-x", "120", "-y", "32", f"/bin/sh {R}/tui.sh"], env={"PATH": "/usr/bin:/bin", "HOME": f"{R}/home"})
    time.sleep(3)
    for name in ("herdr", "tmux"):
        rid = call("resource.spawn", {"provider": name, "argv": ["/bin/sh"], "name": f"tui-{name}"})["resource"]["id"]
        call("command.invoke", {"name": "prim.layout.reset"})
        time.sleep(2)
        # One tab per live terminal: the newest is last; make it active.
        tabs = call("workspace.snapshot")["entries"]["layout/main"]["value"]["tabs"]
        index = next(i for i, t in enumerate(tabs) if t["node"]["view"] == f"term-{rid}")
        value = call("workspace.snapshot")["entries"]["layout/main"]["value"]
        value["active"] = index
        call("workspace.apply", {"note": "e2e active tab", "ops": [{"op": "set", "key": "layout/main", "value": value}]})
        time.sleep(1.5)
        seen = None
        for _ in range(2):  # the tab switch may still be settling: type once more if nothing showed
            keys(f"echo tui-$((6*7))-{name}", literal=True)
            keys("Enter")
            seen = wait(lambda: f"tui-42-{name}" in screen(), 10)
            if seen:
                break
        check(f"{name}: TUI shows the provider's terminal and typing reaches it", bool(seen), screen()[-600:])
        call("resource.kill", {"resource": rid})
        time.sleep(1)
    keys("C-g")
    time.sleep(0.4)
    keys("q")
    gone = wait(lambda: subprocess.run([*HARNESS, "list-sessions"], capture_output=True).returncode != 0, 10)
    check("TUI quits", bool(gone))
finally:
    subprocess.run([*HARNESS, "kill-server"], capture_output=True)
    if core:
        core.terminate()
        try:
            core.wait(10)
        except subprocess.TimeoutExpired:
            core.kill()
    subprocess.run([HC, "server", "stop"], env={**ENV, "PATH": ""}, capture_output=True)
    def leftovers():
        found = []
        for pid in os.listdir("/proc"):
            if pid.isdigit():
                try:
                    if R.encode() in open(f"/proc/{pid}/environ", "rb").read() or R.encode() in open(f"/proc/{pid}/cmdline", "rb").read():
                        found.append(pid)
                except OSError:
                    pass
        return found
    left = leftovers()
    for _ in range(25):  # herdr-core and providers wind down asynchronously
        if not left:
            break
        time.sleep(0.2)
        left = leftovers()
    check("nothing left running", not left, " ".join(left))
    for pid in left:
        os.kill(int(pid), 9)
    print(f"root {R}; passed {sum(results)} failed {len(results) - sum(results)}")
    if os.environ.get("E2E_KEEP") is None:
        shutil.rmtree(R, ignore_errors=True)
sys.exit(0 if all(results) else 1)
