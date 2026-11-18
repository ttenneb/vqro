#!/usr/bin/python3
"""Opt-in TUI check: keys reach programs with their modifiers, like an xterm.

The real `vqro tui` runs in an outer tmux harness. Keys tmux can name are sent with `send-keys`; keys a
modern terminal reports with the kitty protocol (Shift-Enter as `CSI 13;2u`) are written to the TUI as those
bytes, as such a terminal would. Inside, `tests/fixtures/keyecho.py`
prints the bytes of each key it receives. Checks, on a private herdr-core and the vqro-owned private tmux:
- legacy encoding with modifiers: Ctrl-Left, Shift-Up, Ctrl-F5, Alt-x, Shift-Enter (plain Enter);
- herdr: a program that pushed the kitty keyboard protocol gets Shift-Enter as `CSI 13;2u` and Alt-x as
  `CSI 120;3u`; one that enabled modifyOtherKeys gets Shift-Enter as `CSI 27;2;13~`;
- the prefix key still opens the prefix menu while such a program is focused.

Usage: tests/e2e_tui_keys.py TARGET_DIR   (debug binaries in TARGET_DIR/debug)
Private /tmp root; nothing live is touched. Stops only what it started.
"""
import json, os, shutil, socket, subprocess, sys, tempfile, time

TARGET = sys.argv[1]
VQRO, PHERDR, PTMUX = (os.path.join(TARGET, "debug", b) for b in ("vqro", "vqro-herdr", "vqro-tmux"))
HC = os.path.expanduser("~/.local/bin/herdr-core")
REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
KEYECHO = os.path.join(REPO, "tests", "fixtures", "keyecho.py")
R = tempfile.mkdtemp(prefix="pke", dir="/tmp")
for d in ("home", "st", "xst", "run", "cfg", "ext", "bin"):
    os.makedirs(os.path.join(R, d))
os.chmod(os.path.join(R, "run"), 0o700)
for f in os.listdir(os.path.join(REPO, "extensions")):
    if f.endswith(".py") or f == "defaults.json":
        shutil.copy(os.path.join(REPO, "extensions", f), os.path.join(R, "ext", f))
shutil.copytree(os.path.join(REPO, "extensions", "manifests"), os.path.join(R, "ext", "manifests"))
os.symlink(shutil.which("python3") or "/usr/bin/python3", os.path.join(R, "bin", "python3"))
ENV = {"HOME": f"{R}/home", "PATH": f"{R}/bin", "TERM": "xterm-256color", "SHELL": "/bin/sh", "VQRO_STATE_DIR": f"{R}/st",
       "VQRO_SOCKET": f"{R}/p.sock", "VQRO_EXTENSIONS_DIR": f"{R}/ext", "XDG_STATE_HOME": f"{R}/xst",
       "XDG_RUNTIME_DIR": f"{R}/run", "XDG_CONFIG_HOME": f"{R}/cfg", "HC_SESSION": "keys", "HC_SOCKET": f"{R}/hc.sock",
       "LANG": "C.UTF-8"}
HARNESS = ["/usr/bin/tmux", "-S", f"{R}/harness.sock", "-f", "/dev/null"]
results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(("PASS " if ok else "FAIL ") + name + (f"\n{detail}" if detail and not ok else ""), flush=True)


def call(method, params=None):
    s = socket.socket(socket.AF_UNIX)
    s.connect(ENV["VQRO_SOCKET"])
    f = s.makefile("rwb", buffering=0)
    for i, (m, p) in enumerate([("hello", {"client": "e2e-keys"}), (method, params or {})]):
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


def keys(*k):
    subprocess.run([*HARNESS, "send-keys", *k])


def received():
    return [line.split("KEY ", 1)[1].split()[0] for line in screen().splitlines() if "KEY " in line]


def show(rid):
    call("command.invoke", {"name": "vqro.layout.reset"})
    def activate():
        entry = call("workspace.snapshot")["entries"]["layout/main"]
        value = entry["value"]
        value["active"] = next(i for i, t in enumerate(value["tabs"]) if t["node"]["view"] == f"term-{rid}")
        call("workspace.apply", {"note": "e2e active tab", "ops": [{"op": "set", "key": "layout/main", "value": value, "expect": entry["version"]}]})
        return True
    wait(activate, 10)
    time.sleep(1.5)


def run(provider, mode, sends):
    """Start keyecho in `mode` on `provider`, send `sends`, return the hex of each key received."""
    argv = ["/usr/bin/python3", KEYECHO] + ([mode] if mode else [])
    rid = call("resource.spawn", {"provider": provider, "argv": argv, "cwd": f"{R}/home",
                                  "env": {"PATH": "/usr/bin:/bin", "TERM": "xterm-256color", "LANG": "C.UTF-8"}})["resource"]["id"]
    show(rid)
    wait(lambda: "KEYECHO-READY" in screen(), 10)
    for k in sends:
        if k in CSI_U:
            subprocess.run([*HARNESS, "send-keys", "-l", CSI_U[k]])
        else:
            keys(k)
        time.sleep(0.3)
    got = wait(lambda: len(received()) >= len(sends) and received(), 8) or received()
    return rid, got


# What a kitty-protocol terminal sends for keys tmux's send-keys can't express.
CSI_U = {"S-Enter": "\x1b[13;2u", "C-Enter": "\x1b[13;5u"}
LEGACY = [("C-Left", "1b5b313b3544"), ("S-Up", "1b5b313b3241"), ("C-F5", "1b5b31353b357e"), ("M-x", "1b78"), ("S-Enter", "0d")]
core = None
try:
    core = subprocess.Popen([VQRO, "serve", "--fake"], env=ENV, stdout=open(f"{R}/serve.log", "w"), stderr=subprocess.STDOUT)
    wait(lambda: os.path.exists(ENV["VQRO_SOCKET"]), 10)
    call("workspace.apply", {"note": "providers", "ops": [
        {"op": "set", "key": "extension/herdr", "value": {"argv": ["/usr/bin/env", "PATH=", PHERDR, "--hc", HC, "--name", "herdr"], "kind": "provider", "default": True}},
        {"op": "set", "key": "extension/tmux", "value": {"argv": ["/usr/bin/env", "PATH=", PTMUX, "--tmux", "/usr/bin/tmux", "--name", "tmux"], "kind": "provider"}}]})
    ok = wait(lambda: all(call("workspace.snapshot")["providers"].get(n, {}).get("state") == "connected" for n in ("herdr", "tmux")), 30)
    check("providers connected", bool(ok))
    with open(f"{R}/tui.sh", "w") as f:
        f.write("".join(f"export {k}='{v}'\n" for k, v in ENV.items()) + f"exec {VQRO} tui\n")
    subprocess.run([*HARNESS, "new-session", "-d", "-x", "120", "-y", "34", f"/bin/sh {R}/tui.sh"], env={"PATH": "/usr/bin:/bin", "HOME": f"{R}/home"})
    time.sleep(3)

    for provider in ("herdr", "tmux"):
        rid, got = run(provider, "", [k for k, _ in LEGACY])
        check(f"{provider}: modifiers reach a legacy program like xterm ({', '.join(k for k, _ in LEGACY)})",
              got[:len(LEGACY)] == [h for _, h in LEGACY], f"got {got}")
        call("resource.kill", {"resource": rid})
        time.sleep(1)

    rid, got = run("herdr", "kitty", ["S-Enter", "M-x", "C-Left", "Enter"])
    check("herdr: kitty-protocol program gets Shift-Enter and Alt-x as CSI u",
          got[:4] == ["1b5b31333b3275", "1b5b3132303b3375", "1b5b313b3544", "0d"], f"got {got}")
    keys("C-g")
    check("prefix menu still opens while that program is focused", bool(wait(lambda: "press a key or click" in screen(), 5)), screen())
    check("prefix uses full key names by default", bool(wait(lambda:"Ctrl-g" in screen(),5)), screen())
    print("CAPTURE key-labels-full\n"+screen(),flush=True)
    current=call("workspace.snapshot")["entries"].get("doc/settings/tui",{}).get("value",{})
    bindings={k:v for k,v in call("workspace.snapshot")["entries"].items() if k.startswith("binding/")}
    call("workspace.apply",{"ops":[{"op":"set","key":"doc/settings/tui","value":dict(current,key_labels="short")}]})
    check("open prefix switches to short key names", bool(wait(lambda:"C-g" in screen() and "Ctrl-g" not in screen(),5)), screen())
    print("CAPTURE key-labels-short\n"+screen(),flush=True)
    call("workspace.apply",{"ops":[{"op":"set","key":"doc/settings/tui","value":dict(current,key_labels="unknown")}]})
    check("unknown label preference falls back to full", bool(wait(lambda:"Ctrl-g" in screen(),5)), screen())
    check("label changes preserve stored bindings and prefix", bindings=={k:v for k,v in call("workspace.snapshot")["entries"].items() if k.startswith("binding/")} and
          call("workspace.snapshot")["entries"]["doc/settings/tui"]["value"].get("prefix","C-g")==current.get("prefix","C-g"))
    keys("Escape")
    # Restart the UI while the kitty program keeps running: its push is in no history herdr-core repeats,
    # so the new UI must start from the core's observation, or Shift-Enter would submit (sent as Enter).
    keys("C-g"); time.sleep(0.4); keys("q")
    wait(lambda: subprocess.run([*HARNESS, "list-sessions"], capture_output=True).returncode != 0, 10)
    subprocess.run([*HARNESS, "new-session", "-d", "-x", "120", "-y", "34", f"/bin/sh {R}/tui.sh"], env={"PATH": "/usr/bin:/bin", "HOME": f"{R}/home"})
    time.sleep(3)
    show(rid)
    wait(lambda: "KEYECHO-READY" in screen() or "KEY " in screen(), 10)
    before = len(received())
    for k in ("S-Enter", "M-x"):
        subprocess.run([*HARNESS, "send-keys", "-l", CSI_U[k]]) if k in CSI_U else keys(k)
        time.sleep(0.3)
    after = wait(lambda: len(received()) >= before + 2 and received(), 8) or received()
    check("herdr: after a UI restart the kitty program still gets Shift-Enter and Alt-x as CSI u",
          after[before:before + 2] == ["1b5b31333b3275", "1b5b3132303b3375"], f"got {after[before:]}")
    call("resource.kill", {"resource": rid})
    time.sleep(1)
    rid, got = run("herdr", "mok", ["S-Enter", "C-Enter", "Enter"])
    check("herdr: modifyOtherKeys program gets Shift-Enter and Ctrl-Enter as CSI 27;m;13~",
          got[:3] == ["1b5b32373b323b31337e", "1b5b32373b353b31337e", "0d"], f"got {got}")
    call("resource.kill", {"resource": rid})
    time.sleep(0.5)
    keys("C-g"); time.sleep(0.4); keys("q")
    check("TUI quits", bool(wait(lambda: subprocess.run([*HARNESS, "list-sessions"], capture_output=True).returncode != 0, 10)))
finally:
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
