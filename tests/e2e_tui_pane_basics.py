#!/usr/bin/python3
"""Opt-in TUI check: tab and pane management basics through the real `vqro tui`, keys and mouse.

On a private herdr-core: a tab shows its program's OSC title; prefix `<` moves a tab and dragging a tab chip
onto another moves it back; `r` renames a tab; `h` splits, then `;` returns to the last pane, `}` swaps,
Ctrl-Right resizes, Space applies a layout preset and `b` breaks a pane out to a tab; the tmux profile's `{`
swaps too. Layout effects are read back from the core.

Usage: tests/e2e_tui_pane_basics.py TARGET_DIR   (debug binaries in TARGET_DIR/debug)
Private /tmp root; nothing live is touched. Stops only what it started.
"""
import json, os, shutil, socket, subprocess, sys, tempfile, time

TARGET = sys.argv[1]
VQRO, PHERDR = (os.path.join(TARGET, "debug", b) for b in ("vqro", "vqro-herdr"))
HC = os.path.expanduser("~/.local/bin/herdr-core")
REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
R = tempfile.mkdtemp(prefix="ppb", dir="/tmp")
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
       "XDG_RUNTIME_DIR": f"{R}/run", "XDG_CONFIG_HOME": f"{R}/cfg", "HC_SESSION": "panes", "HC_SOCKET": f"{R}/hc.sock",
       "LANG": "C.UTF-8"}
HARNESS = ["/usr/bin/tmux", "-S", f"{R}/harness.sock", "-f", "/dev/null"]
results = []


def check(name, ok, detail=""):
    results.append(bool(ok))
    print(("PASS " if ok else "FAIL ") + name + (f"\n{detail}" if detail and not ok else ""), flush=True)


def call(method, params=None):
    s = socket.socket(socket.AF_UNIX)
    s.connect(ENV["VQRO_SOCKET"])
    f = s.makefile("rwb", buffering=0)
    for i, (m, p) in enumerate([("hello", {"client": "e2e-panes"}), (method, params or {})]):
        f.write((json.dumps({"jsonrpc": "2.0", "id": i, "method": m, "params": p}) + "\n").encode())
        while True:
            msg = json.loads(f.readline())
            if msg.get("id") == i:
                break
    s.close()
    if "error" in msg:
        raise RuntimeError(f"{method}: {msg['error']}")
    return msg["result"]


def wait(pred, timeout=15):
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
    time.sleep(0.4)


def prefix(*k, literal=False, pfx="C-g"):
    keys(pfx)
    keys(*k, literal=literal)


def tabs():
    """The main layout's tab group (the sidebar may sit beside it)."""
    root = call("workspace.snapshot")["entries"]["layout/main"]["value"]
    stack = [root]
    while stack:
        n = stack.pop()
        if "tabs" in n:
            return n
        stack.extend(n.get("children", []))


def views(tab):
    out, stack = [], [tab["node"]]
    while stack:
        n = stack.pop(0)
        if "view" in n:
            out.append(n["view"])
        stack.extend(n.get("children", []))
    return out


def bar():
    """The screen line holding the tab bar (it has the subscript tab numbers)."""
    return next((line for line in screen().splitlines() if "₁" in line), "")


def click_drag(x0, x1, y):
    """Left press at column x0, drag, release at x1 (0-based cells), as SGR mouse reports."""
    for seq in (f"\x1b[<0;{x0 + 1};{y + 1}M", f"\x1b[<32;{(x0 + x1) // 2 + 1};{y + 1}M", f"\x1b[<32;{x1 + 1};{y + 1}M", f"\x1b[<0;{x1 + 1};{y + 1}m"):
        subprocess.run([*HARNESS, "send-keys", "-l", seq])
        time.sleep(0.15)


def side_of(text):
    """Which half of the screen a line containing text starts in ("left"/"right"), or None."""
    for line in screen().splitlines():
        if text in line:
            return "left" if line.index(text) < 60 else "right"
    return None


core = None
try:
    core = subprocess.Popen([VQRO, "serve", "--fake"], env=ENV, stdout=open(f"{R}/serve.log", "w"), stderr=subprocess.STDOUT)
    wait(lambda: os.path.exists(ENV["VQRO_SOCKET"]), 10)
    call("workspace.apply", {"note": "provider", "ops": [
        {"op": "set", "key": "extension/herdr", "value": {"argv": ["/usr/bin/env", "PATH=", PHERDR, "--hc", HC, "--name", "herdr"], "kind": "provider", "default": True}}]})
    check("herdr provider connected", wait(lambda: call("workspace.snapshot")["providers"].get("herdr", {}).get("state") == "connected", 30))
    wait(lambda: any(c["name"] == "panes.swap" and c.get("available") for c in call("command.list")["commands"]), 20)
    for _ in range(2):
        call("command.invoke", {"name": "tabs.open", "args": {"argv": ["/bin/sh"]}})
    with open(f"{R}/tui.sh", "w") as f:
        f.write("".join(f"export {k}='{v}'\n" for k, v in ENV.items()) + f"exec {VQRO} tui\n")
    subprocess.run([*HARNESS, "new-session", "-d", "-x", "120", "-y", "34", f"/bin/sh {R}/tui.sh"], env={"PATH": "/usr/bin:/bin", "HOME": f"{R}/home"})
    check("two shell tabs", wait(lambda: "₂" in bar() and len(tabs()["tabs"]) == 2), screen())
    first, second = (views(t)[0] for t in tabs()["tabs"])

    # Program titles: the active (second) shell sets its OSC title; its tab shows it.
    keys("printf '\\033]0;MYTITLE\\007'", literal=True); keys("Enter")
    check("a shell tab shows its program's OSC title", wait(lambda: "MYTITLE₂" in bar()), bar())
    print("CAPTURE program-title\n" + bar(), flush=True)

    # Tab move by key, then back by dragging its chip onto the other tab.
    prefix("<", literal=True)
    check("prefix < moves the tab left and keeps it shown", wait(lambda: [views(t)[0] for t in tabs()["tabs"]] == [second, first]
                                                                   and tabs()["active"] == 0), tabs())
    wait(lambda: "MYTITLE₁" in bar())
    line = bar()
    y = screen().splitlines().index(line)
    x_dragged, x_target = line.index("MYTITLE₁"), line.index("₂") - 1
    click_drag(x_dragged + 2, x_target, y)
    check("dragging a tab chip onto another moves it there", wait(lambda: [views(t)[0] for t in tabs()["tabs"]] == [first, second]), tabs())
    print("CAPTURE after-drag\n" + bar(), flush=True)

    # Rename with the default key.
    prefix("r")
    keys(*["BSpace"] * 12); keys("renamed", literal=True); keys("Enter")
    check("prefix r renames the tab", wait(lambda: any(t["title"] == "renamed" for t in tabs()["tabs"])), tabs())

    # Panes in the shown tab: split, last pane, swap, resize, preset, break.
    prefix("h")
    check("prefix h splits the tab", wait(lambda: len(views(tabs()["tabs"][tabs()["active"]])) == 2), tabs())
    time.sleep(1)
    keys("echo P-ONE", literal=True); keys("Enter")
    prefix("o")
    keys("echo P-TWO", literal=True); keys("Enter")
    keys("C-g"); keys("-H", "3b")  # `;` as hex: a lone ; argument is tmux's command separator
    keys("echo P-THREE", literal=True); keys("Enter")
    wait(lambda: side_of("P-THREE") and side_of("P-TWO"))
    check("prefix ; returns to the last pane", side_of("P-THREE") == side_of("P-ONE") != side_of("P-TWO"),
          f"P-ONE {side_of('P-ONE')} P-TWO {side_of('P-TWO')} P-THREE {side_of('P-THREE')}\n{screen()}")
    node = lambda: tabs()["tabs"][tabs()["active"]]["node"]
    before = views(tabs()["tabs"][tabs()["active"]])
    prefix("}", literal=True)
    check("prefix } swaps the panes", wait(lambda: views(tabs()["tabs"][tabs()["active"]]) == before[::-1]), node())
    prefix("C-Right")
    check("prefix Ctrl-Right moves the divider", wait(lambda: len(set(node().get("sizes", [1, 1]))) == 2), node())
    prefix("Space")
    check("prefix Space applies the next layout preset", wait(lambda: node().get("preset") == "even-h"), node())
    count = len(tabs()["tabs"])
    prefix("b")
    check("prefix b breaks the pane out to a tab of its own", wait(lambda: len(tabs()["tabs"]) == count + 1 and len(views(tabs()["tabs"][tabs()["active"]])) == 1), tabs())

    # The tmux profile binds the same commands on tmux's keys.
    call("command.invoke", {"name": "keys.profile", "args": {"value": "tmux"}})
    wait(lambda: call("workspace.snapshot")["entries"].get("binding/keys.swap-prev"))
    keys("Escape")
    prefix("%", literal=True, pfx="C-b")
    check("tmux profile: C-b % splits", wait(lambda: len(views(tabs()["tabs"][tabs()["active"]])) == 2), tabs())
    before = views(tabs()["tabs"][tabs()["active"]])
    prefix("{", literal=True, pfx="C-b")
    check("tmux profile: C-b { swaps the panes", wait(lambda: views(tabs()["tabs"][tabs()["active"]]) == before[::-1]), node())
    prefix("C-o", pfx="C-b")
    check("tmux profile: C-b C-o rotates them back", wait(lambda: views(tabs()["tabs"][tabs()["active"]]) == before), node())
    print("CAPTURE end\n" + screen(), flush=True)
    keys("C-b"); keys("d")
    check("TUI quits", wait(lambda: subprocess.run([*HARNESS, "list-sessions"], capture_output=True).returncode != 0, 10))
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
            if pid.isdigit() and int(pid) != os.getpid():
                try:
                    if R.encode() in open(f"/proc/{pid}/environ", "rb").read() or R.encode() in open(f"/proc/{pid}/cmdline", "rb").read():
                        found.append(pid)
                except OSError:
                    pass
        return found
    stray = wait(lambda: not leftovers() and [True], 10)
    check("nothing left running", bool(stray), str(leftovers()))
    shutil.rmtree(R, ignore_errors=True)
print(f"root {R}; passed {sum(results)} failed {len(results) - sum(results)}")
sys.exit(0 if all(results) else 1)
