#!/usr/bin/python3
"""End to end: the OpenTUI trial client (`vqro ui opentui`, D29) against a private core, with a real shell in a
private herdr-core through vqro-herdr. Drives it in an outer tmux harness like tests/e2e_tui_providers.py.

Usage: tests/e2e_opentui.py TARGET_DIR   (debug binaries in TARGET_DIR/debug)
Skips (exit 0) when Bun or the client's node_modules are absent: the trial client is optional.
Private /tmp root; stops only what it started.
"""
import json, os, shutil, socket, subprocess, sys, tempfile, time

TARGET = sys.argv[1]
VQRO, PHERDR = (os.path.join(TARGET, "debug", b) for b in ("vqro", "vqro-herdr"))
HC = os.path.expanduser("~/.local/bin/herdr-core")
REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CLIENT = os.path.join(REPO, "extensions", "clients", "opentui")
BUN = shutil.which("bun") or os.path.expanduser("~/.local/bin/bun")
if not os.path.isfile(BUN) or not os.path.isdir(os.path.join(CLIENT, "node_modules", "@opentui")):
    print("SKIP OpenTUI trial client: Bun or extensions/clients/opentui/node_modules missing (bun install)")
    sys.exit(0)
R = tempfile.mkdtemp(prefix="pott", dir="/tmp")
for d in ("home", "st", "run", "bin"):
    os.makedirs(os.path.join(R, d))
os.chmod(os.path.join(R, "run"), 0o700)
shutil.copytree(os.path.join(REPO, "extensions"), os.path.join(R, "ext"), ignore=shutil.ignore_patterns("__pycache__", "node_modules"))
os.symlink(os.path.join(CLIENT, "node_modules"), os.path.join(R, "ext", "clients", "opentui", "node_modules"))  # 85 MB: share, don't copy
for name, target in (("python3", shutil.which("python3") or "/usr/bin/python3"), ("bun", BUN)):
    os.symlink(target, os.path.join(R, "bin", name))
ENV = {"HOME": f"{R}/home", "PATH": f"{R}/bin", "TERM": "xterm-256color", "SHELL": "/bin/sh", "VQRO_STATE_DIR": f"{R}/st",
       "VQRO_SOCKET": f"{R}/p.sock", "VQRO_EXTENSIONS_DIR": f"{R}/ext", "XDG_RUNTIME_DIR": f"{R}/run",
       "HC_SOCKET": f"{R}/hc.sock", "HC_SESSION": "ot", "LANG": "C.UTF-8"}
HARNESS = ["/usr/bin/tmux", "-S", f"{R}/harness.sock", "-f", "/dev/null"]
results = []


def check(name, ok, detail=""):
    results.append(bool(ok))
    print(("PASS " if ok else "FAIL ") + name + (f"  [{detail}]" if detail and not ok else ""), flush=True)


def call(method, params=None):
    s = socket.socket(socket.AF_UNIX)
    s.connect(ENV["VQRO_SOCKET"])
    f = s.makefile("rwb", buffering=0)
    for i, (m, p) in enumerate([("hello", {"client": "e2e-opentui"}), (method, params or {})]):
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
    core = subprocess.Popen([VQRO, "serve"], env=ENV, stdout=open(f"{R}/serve.log", "w"), stderr=subprocess.STDOUT)
    wait(lambda: os.path.exists(ENV["VQRO_SOCKET"]), 10)
    herdr = call("workspace.snapshot")["entries"]["extension/herdr"]["value"]
    call("workspace.apply", {"note": "private herdr", "ops": [{"op": "set", "key": "extension/herdr",
        "value": dict(herdr, argv=["/usr/bin/env", "PATH=", PHERDR, "--hc", HC, "--name", "herdr"])}]})
    check("private herdr-core backend connected", wait(lambda: call("workspace.snapshot")["providers"].get("herdr", {}).get("state") == "connected", 30))
    call("command.invoke", {"name": "tabs.open"})
    with open(f"{R}/client.sh", "w") as f:
        f.write("".join(f"export {k}='{v}'\n" for k, v in ENV.items()) + f"{VQRO} ui opentui 2>{R}/client.err; echo CLIENT-EXIT $?; /usr/bin/sleep 30\n")
    subprocess.run([*HARNESS, "new-session", "-d", "-x", "110", "-y", "30", f"/bin/sh {R}/client.sh"], env={"PATH": "/usr/bin:/bin", "HOME": f"{R}/home"})
    found = wait(lambda: next((c for c in call("client.list")["clients"] if c.get("client") == "opentui"), None))
    check("hello declares base, keys and terminal profiles", found and found["profiles"] == ["base", "keys", "terminal"] and "follow" in found["features"], found)
    check("tab bar and status bar render", bool(wait(lambda: " 1:" in screen() and "vqro ▾" in screen())), screen())
    shown = None
    for _ in range(3):
        keys("echo ot-$((6*7))", literal=True); keys("Enter")
        shown = wait(lambda: "ot-42" in screen(), 8)
        if shown:
            break
    check("a real shell through Ghostty's emulator: typing reaches it and output shows", bool(shown), screen())
    keys("printf '\\033[1;31mred-text\\033[0m\\n'", literal=True); keys("Enter")
    check("escape sequences are interpreted, not printed", bool(wait(lambda: any(l.strip(" │") == "red-text" for l in screen().splitlines()))), screen())
    subprocess.run([*HARNESS, "resize-window", "-x", "90", "-y", "26"], capture_output=True)
    sized = None
    for _ in range(5):  # resize dispatch is not settlement: ask the shell again until the PTY follows
        keys("/usr/bin/stty size", literal=True); keys("Enter")
        sized = wait(lambda: any(l.strip(" │") == "22 88" for l in screen().splitlines()), 2)
        if sized:
            break
    check("pane size follows the window (stream.resize: 88x22 inside the border)", bool(sized), screen())
    # The core restarts (an upgrade): the client reconnects, reattaches with a fresh repaint, and the shell lives on.
    call("core.shutdown")
    core.wait(15)
    core = subprocess.Popen([VQRO, "serve"], env=ENV, stdout=open(f"{R}/serve2.log", "w"), stderr=subprocess.STDOUT)
    check("client reconnects after a core restart", bool(wait(lambda: "reconnected" in screen(), 30)), screen())
    again = None
    for _ in range(5):
        keys("echo back-$((6*7))", literal=True); keys("Enter")
        again = wait(lambda: "back-42" in screen(), 4)
        if again:
            break
    check("the same shell answers after reattaching", bool(again), screen())
    keys("C-g")
    check("prefix menu lists recovery actions", bool(wait(lambda: "undo last change" in screen() and "safe mode / recovery choices" in screen())), screen())
    keys(",")
    check("a command's popup reply shows Settings over the terminal", bool(wait(lambda: "Settings" in screen() and "Extensions" in screen() and "Tabs (tabs)" in screen())), screen())
    for _ in range(7):
        keys("Down")
    time.sleep(0.3)
    keys("Space")
    check("dependency warning as a pick-one menu over the popup", bool(wait(lambda: "Turn off Tabs: breaks" in screen())), screen())
    # Escape followed at once by another key reads as Alt+key: pause like a person would.
    keys("Escape"); time.sleep(0.5); keys("Escape"); time.sleep(0.5)
    keys("C-g", "!")
    check("safe-mode chooser is the shared recovery content", bool(wait(lambda: "Safe mode recovery" in screen() and "Providers / backends" in screen(), 15)), screen())
    keys("End"); time.sleep(0.3); keys("Enter")
    check("leave safe mode from the chooser", bool(wait(lambda: call("workspace.snapshot")["safe_mode"] is False, 15)))
    time.sleep(0.5)
    keys("C-g", "q")
    check("client quits cleanly", bool(wait(lambda: "CLIENT-EXIT 0" in screen(), 10)), screen() + open(f"{R}/client.err").read()[-500:])
finally:
    subprocess.run([*HARNESS, "kill-server"], capture_output=True)
    if core:
        core.terminate()
        try:
            core.wait(10)
        except subprocess.TimeoutExpired:
            core.kill()
    subprocess.run([HC, "server", "stop"], env={**ENV, "PATH": ""}, capture_output=True)

    def mine(pid):
        try:
            return R.encode() in open(f"/proc/{pid}/environ", "rb").read() or R.encode() in open(f"/proc/{pid}/cmdline", "rb").read()
        except OSError:
            return False
    left = []
    for _ in range(25):
        left = [p for p in os.listdir("/proc") if p.isdigit() and mine(p)]
        if not left:
            break
        time.sleep(0.2)
    check("nothing left running", not left, " ".join(left))
    print(f"root {R}; passed {sum(results)} failed {len(results) - sum(results)}")
    if os.environ.get("E2E_KEEP") is None:
        shutil.rmtree(R, ignore_errors=True)
sys.exit(0 if all(results) else 1)
