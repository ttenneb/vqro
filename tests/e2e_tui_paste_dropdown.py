#!/usr/bin/python3
"""Opt-in TUI check: drop-downs keep their popup visible, and pastes reach programs intact.

The real `prim tui` runs in an outer tmux harness; its terminals live in prim-herdr (a private herdr-core)
and prim-tmux (its private tmux server). Checks:
- Settings: a drop-down opened from a popup (keyboard and mouse) is drawn over the popup, which stays
  visible; Esc returns to the popup.
- A multi-line paste into bash (which enables bracketed paste, DECSET 2004) is inserted, not run line by
  line; it runs on Enter.
- A 200 KB paste into `cat > file` arrives complete, on both backends (more than the 64 KiB per-input
  limit, and more than tmux's send-keys parser takes in one command).

Usage: tests/e2e_tui_paste_dropdown.py TARGET_DIR   (debug binaries in TARGET_DIR/debug)
Private /tmp root; nothing live is touched. Stops only what it started.
"""
import hashlib, json, os, shutil, socket, subprocess, sys, tempfile, time

TARGET = sys.argv[1]
PRIM, PHERDR, PTMUX = (os.path.join(TARGET, "debug", b) for b in ("prim", "prim-herdr", "prim-tmux"))
HC = os.path.expanduser("~/.local/bin/herdr-core")
REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
R = tempfile.mkdtemp(prefix="ppd", dir="/tmp")
for d in ("home", "st", "xst", "run", "cfg", "ext", "bin", "out"):
    os.makedirs(os.path.join(R, d))
os.chmod(os.path.join(R, "run"), 0o700)
for f in os.listdir(os.path.join(REPO, "extensions")):
    if f.endswith(".py") or f == "defaults.json":
        shutil.copy(os.path.join(REPO, "extensions", f), os.path.join(R, "ext", f))
shutil.copytree(os.path.join(REPO, "extensions", "manifests"), os.path.join(R, "ext", "manifests"))
os.symlink(shutil.which("python3") or "/usr/bin/python3", os.path.join(R, "bin", "python3"))
ENV = {"HOME": f"{R}/home", "PATH": f"{R}/bin", "TERM": "xterm-256color", "SHELL": "/bin/sh", "PRIM_STATE_DIR": f"{R}/st",
       "PRIM_SOCKET": f"{R}/p.sock", "PRIM_EXTENSIONS_DIR": f"{R}/ext", "XDG_STATE_HOME": f"{R}/xst",
       "XDG_RUNTIME_DIR": f"{R}/run", "XDG_CONFIG_HOME": f"{R}/cfg", "HC_SESSION": "paste", "HC_SOCKET": f"{R}/hc.sock",
       "LANG": "C.UTF-8", "PRIM_TUI_PROBE": f"{R}/tui-probe.json"}
HARNESS = ["/usr/bin/tmux", "-S", f"{R}/harness.sock", "-f", "/dev/null"]
SHELL_ENV = {"PATH": "/usr/bin:/bin", "HOME": f"{R}/home", "TERM": "xterm-256color", "LANG": "C.UTF-8", "PS1": "$ "}
results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(("PASS " if ok else "FAIL ") + name + (f"\n{detail}" if detail and not ok else ""), flush=True)


def call(method, params=None):
    s = socket.socket(socket.AF_UNIX)
    s.connect(ENV["PRIM_SOCKET"])
    f = s.makefile("rwb", buffering=0)
    for i, (m, p) in enumerate([("hello", {"client": "e2e-paste"}), (method, params or {})]):
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
        time.sleep(0.25)
    return None


def screen():
    return subprocess.run([*HARNESS, "capture-pane", "-p"], capture_output=True, text=True).stdout


def keys(*k, literal=False):
    subprocess.run([*HARNESS, "send-keys", *(["-l"] if literal else []), *k])


def paste(path):
    """Paste a file's bytes into the TUI the way a terminal does (bracketed, since the TUI asks for it)."""
    subprocess.run([*HARNESS, "load-buffer", "-b", "p", path])
    subprocess.run([*HARNESS, "paste-buffer", "-p", "-d", "-b", "p"])


def show(rid):
    """Make the tab of resource `rid` the active one."""
    call("command.invoke", {"name": "prim.layout.reset"})
    def activate():
        entry = call("workspace.snapshot")["entries"]["layout/main"]
        value = entry["value"]
        value["active"] = next(i for i, t in enumerate(value["tabs"]) if t["node"]["view"] == f"term-{rid}")
        call("workspace.apply", {"note": "e2e active tab", "ops": [{"op": "set", "key": "layout/main", "value": value, "expect": entry["version"]}]})
        return True
    wait(activate, 10)
    time.sleep(1.5)


def bash(provider):
    rid = call("resource.spawn", {"provider": provider, "argv": ["/bin/bash", "--norc", "--noprofile"], "cwd": f"{R}/home",
                                  "env": SHELL_ENV, "cols": 118, "rows": 28})["resource"]["id"]
    show(rid)
    for _ in range(3):  # wait until the shell answers in the focused view
        keys("echo ready-$((40+2))", literal=True); keys("Enter")
        if wait(lambda: "ready-42" in screen(), 6):
            break
    return rid


core = None
try:
    core = subprocess.Popen([PRIM, "serve", "--fake"], env=ENV, stdout=open(f"{R}/serve.log", "w"), stderr=subprocess.STDOUT)
    wait(lambda: os.path.exists(ENV["PRIM_SOCKET"]), 10)
    call("workspace.apply", {"note": "providers", "ops": [
        {"op": "set", "key": "extension/herdr", "value": {"argv": ["/usr/bin/env", "PATH=", PHERDR, "--hc", HC, "--name", "herdr"], "kind": "provider", "default": True}},
        {"op": "set", "key": "extension/tmux", "value": {"argv": ["/usr/bin/env", "PATH=", PTMUX, "--tmux", "/usr/bin/tmux", "--name", "tmux"], "kind": "provider"}}]})
    ok = wait(lambda: all(call("workspace.snapshot")["providers"].get(n, {}).get("state") == "connected" for n in ("herdr", "tmux")), 30)
    check("providers connected", bool(ok))
    with open(f"{R}/tui.sh", "w") as f:
        f.write("".join(f"export {k}='{v}'\n" for k, v in ENV.items()) + f"exec {PRIM} tui\n")
    subprocess.run([*HARNESS, "new-session", "-d", "-x", "120", "-y", "34", f"/bin/sh {R}/tui.sh"], env={"PATH": "/usr/bin:/bin", "HOME": f"{R}/home"})
    time.sleep(3)

    # 1. Settings drop-down over its popup (Keys page: Profile select).
    keys("C-g", ",")
    wait(lambda: "Settings" in screen() and "Extensions" in screen(), 10)
    for _ in range(4):
        keys("]")
    keys_page = wait(lambda: "Profile:" in screen() and "Settings" in screen(), 10)
    check("Settings Keys page shows the Profile select", bool(keys_page), screen())
    keys("Enter")
    both = wait(lambda: "● prim (C-g)" in screen() and "Settings" in screen() and "Extensions   Interface" in screen(), 5)
    check("keyboard drop-down is drawn over a still-visible Settings popup", bool(both), screen())
    open(f"{R}/out/dropdown-keyboard.txt", "w").write(screen())
    keys("Escape")
    check("Esc returns to the popup", bool(wait(lambda: "Profile:" in screen() and "● prim (C-g)" not in screen(), 5)), screen())
    row = next(((i, line) for i, line in enumerate(screen().splitlines()) if "Profile:" in line), None)
    if row:
        x = row[1].index("Profile:") + 2; y = row[0] + 1
        keys(f"\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m", literal=True)
    both = wait(lambda: "● prim (C-g)" in screen() and "Extensions   Interface" in screen(), 5)
    check("mouse drop-down is drawn over a still-visible Settings popup", bool(both), screen())
    open(f"{R}/out/dropdown-mouse.txt", "w").write(screen())
    keys("Escape"); time.sleep(0.3); keys("Escape"); time.sleep(0.5)

    big = "".join(f"paste line {i:06d} abcdefghijklmnopqrstuvwxyz 0123456789 ABCDEFGHIJKLMNOPQRSTUVWXYZ\n" for i in range(2600))
    assert len(big) > 200_000
    with open(f"{R}/big.txt", "w") as f:
        f.write(big)
    with open(f"{R}/multi.txt", "w") as f:
        f.write("echo BP-$((1+1))\necho BP-$((1+2))\n")
    want = hashlib.sha256(big.encode()).hexdigest()

    for provider in ("herdr", "tmux"):
        rid = bash(provider)
        # 2. Multi-line paste into bash (bracketed paste on): inserted, not executed line by line.
        keys("bind 'set enable-bracketed-paste on'; clear", literal=True); keys("Enter"); time.sleep(0.8)
        paste(f"{R}/multi.txt")
        time.sleep(2)
        early = screen()
        check(f"{provider}: multi-line paste is not run line by line", "BP-2" not in early.replace("BP-$((1+1))", ""), early)
        keys("Enter")
        ran = wait(lambda: "BP-2" in screen().replace("BP-$((1+1))", "") and "BP-3" in screen().replace("BP-$((1+2))", ""), 8)
        check(f"{provider}: the pasted lines run on Enter", bool(ran), screen())
        # 3. 200 KB paste arrives complete (cat: raw, no bracketed paste).
        out = f"{R}/out/{provider}.txt"
        keys(f"clear; cat > {out}", literal=True); keys("Enter"); time.sleep(0.8)
        paste(f"{R}/big.txt")
        done = wait(lambda: os.path.getsize(out) >= len(big), 90)
        keys("C-d"); time.sleep(0.8)
        got = open(out, "rb").read() if os.path.exists(out) else b""
        probe = open(f"{R}/tui-probe.json").read() if os.path.exists(f"{R}/tui-probe.json") else "{}"
        check(f"{provider}: 200 KB paste arrives complete ({len(got)} of {len(big)} bytes)",
              bool(done) and hashlib.sha256(got).hexdigest() == want,
              screen()[-400:] + "\nstream closures: " + json.dumps(json.loads(probe).get("stream_closures")))
        call("resource.kill", {"resource": rid})
        time.sleep(1)

    keys("C-g"); time.sleep(0.4); keys("q")
    check("TUI quits", bool(wait(lambda: subprocess.run([*HARNESS, "list-sessions"], capture_output=True).returncode != 0, 10)))
finally:
    if len(sys.argv) > 2:
        os.makedirs(sys.argv[2], exist_ok=True)
        for name in ("dropdown-keyboard.txt", "dropdown-mouse.txt"):
            if os.path.exists(f"{R}/out/{name}"):
                shutil.copy(f"{R}/out/{name}", os.path.join(sys.argv[2], name))
    subprocess.run([*HARNESS, "kill-server"], capture_output=True)
    if core:
        core.terminate()
        try:
            core.wait(10)
        except subprocess.TimeoutExpired:
            core.kill()
    subprocess.run([HC, "server", "stop"], env={**ENV, "PATH": ""}, capture_output=True)
    subprocess.run(["/usr/bin/tmux", "-N", "-S", f"{R}/st/tmux.sock", "kill-server"],
                   env={"HOME": f"{R}/home", "PATH": "/usr/bin:/bin"}, capture_output=True)
    def leftovers():
        found = []
        for pid in os.listdir("/proc"):
            if pid.isdigit() and int(pid) != os.getpid():
                try:
                    if R.encode() in open(f"/proc/{pid}/environ", "rb").read() or R.encode() in open(f"/proc/{pid}/cmdline", "rb").read():
                        found.append(pid)
                except OSError:
                    pass
        return found
    left = wait(lambda: not leftovers() and [True], 10)
    check("nothing left running", bool(left), " ".join(leftovers()))
    if left:
        shutil.rmtree(R, ignore_errors=True)
print(f"root {R}; passed {sum(results)} failed {len(results) - sum(results)}")
sys.exit(0 if results and all(results) else 1)
