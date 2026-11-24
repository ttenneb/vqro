#!/usr/bin/env python3
"""Opt-in TUI check: tab and pane management basics and terminal links through the real `vqro tui`, keys and mouse.

On a private herdr-core: a tab shows its program's OSC title; prefix `<` moves a tab and dragging a tab chip
onto another moves it back; `r` renames a tab; `h` splits, then `;` returns to the last pane, `}` swaps,
Ctrl-Right resizes, Space applies a layout preset and `b` breaks a pane out to a tab; the tmux profile's `{`
swaps too. Links: Ctrl-click on a URL runs the opener, on `path:line` opens the editor in a split at that line,
and a right-click offers "Open link"; a `repeat` binding (Ctrl-arrows resize) repeats without the prefix only
for a moment. Layout effects are read back from the core.

Usage: tests/e2e_tui_pane_basics.py TARGET_DIR   (debug binaries in TARGET_DIR/debug)
Private /tmp root; nothing live is touched. Stops only what it started.
"""

import sys
if sys.platform != "linux":
    print("SKIP e2e_tui_pane_basics: requires Linux /proc and namespace/process probes")
    raise SystemExit(77)

from python_interpreter import python
from extension_layout import copy_packages
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
copy_packages(os.path.join(REPO,'extensions'),os.path.join(R,'ext'))
os.symlink(python(), os.path.join(R, "bin", "python3"))
ENV = {"HOME": f"{R}/home", "PATH": f"{R}/bin", "TERM": "xterm-256color", "SHELL": "/bin/sh", "VQRO_STATE_DIR": f"{R}/st",
       "VQRO_SOCKET": f"{R}/p.sock", "VQRO_EXTENSIONS_DIR": f"{R}/ext", "XDG_STATE_HOME": f"{R}/xst",
       "XDG_RUNTIME_DIR": f"{R}/run", "XDG_CONFIG_HOME": f"{R}/cfg", "HC_SESSION": "panes", "HC_SOCKET": f"{R}/hc.sock",
       "LANG": "C.UTF-8"}
HARNESS = ["/usr/bin/tmux", "-S", f"{R}/harness.sock", "-f", "/dev/null"]
results = []


def check(name, ok, detail=""):
    results.append(bool(ok))
    print(("PASS " if ok else "FAIL ") + name + (f"\n{detail}" if detail and not ok else ""), flush=True)


CALL_TIMEOUT = 10  # seconds for one call (hello and reply) to the core: a silent core fails the call, not the run


def call(method, params=None):
    """One request on its own connection, bounded in total by CALL_TIMEOUT: the deadline is checked while each reply
    line is framed, so neither silence nor a reply dripped past it can hold the run (TimeoutError; inside wait() it
    counts as "not yet"). Nothing is retried here."""
    deadline = time.monotonic() + CALL_TIMEOUT
    with socket.socket(socket.AF_UNIX) as s:
        s.settimeout(CALL_TIMEOUT)
        s.connect(ENV["VQRO_SOCKET"])
        buf = bytearray()
        for i, (m, p) in enumerate([("hello", {"client": "e2e-panes"}), (method, params or {})]):
            s.sendall((json.dumps({"jsonrpc": "2.0", "id": i, "method": m, "params": p}) + "\n").encode())
            while True:
                if time.monotonic() >= deadline:
                    raise TimeoutError(f"{method}: no reply within {CALL_TIMEOUT} s")
                end = buf.find(b"\n")
                if end < 0:
                    s.settimeout(max(0.001, deadline - time.monotonic()))  # rechecked at the loop top
                    chunk = s.recv(65536)
                    if not chunk:
                        raise ConnectionError(f"{method}: core closed the connection")
                    buf += chunk
                    continue
                msg = json.loads(bytes(buf[:end]))
                del buf[:end + 1]
                if msg.get("id") == i:
                    break
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


def click(x, y, button=0, mods=0):
    """A press and release at a cell (0-based), as SGR mouse reports; mods 16 = Ctrl."""
    code = button + mods
    for seq in (f"\x1b[<{code};{x + 1};{y + 1}M", f"\x1b[<{code};{x + 1};{y + 1}m"):
        subprocess.run([*HARNESS, "send-keys", "-l", seq])
        time.sleep(0.15)


def find(text, last=True):
    """(x, y) of text on the screen (its last occurrence by default), or None."""
    rows = screen().splitlines()
    hits = [(line.index(text), y) for y, line in enumerate(rows) if text in line]
    return (hits[-1] if last else hits[0]) if hits else None


def side_of(text):
    """Which half of the screen a line containing text starts in ("left"/"right"), or None."""
    for line in screen().splitlines():
        if text in line:
            return "left" if line.index(text) < 60 else "right"
    return None


core = None
try:
    core = subprocess.Popen([VQRO, "serve", "--fake", "--safe"], env=ENV, stdout=open(f"{R}/serve.log", "w"), stderr=subprocess.STDOUT)
    wait(lambda: os.path.exists(ENV["VQRO_SOCKET"]), 10)
    # This fixture owns its tab numbering; opt out before any extension can bootstrap.
    call("workspace.apply", {"note": "provider", "ops": [
        {"op": "set", "key": "doc/settings/workspaces", "value": {"open_shell_when_empty": False}},
        {"op": "set", "key": "extension/herdr", "value": {"argv": ["/usr/bin/env", "PATH=", PHERDR, "--hc", HC, "--name", "herdr"], "kind": "provider", "default": True}}]})
    call("command.invoke", {"name": "vqro.safe_mode", "args": {"on": False}})
    check("herdr provider connected", wait(lambda: call("workspace.snapshot")["providers"].get("herdr", {}).get("state") == "connected", 30))
    # Every command the test drives, not one of them: extensions register independently (a gate's load showed
    # panes ready before tabs).
    needed = {"panes.swap", "panes.split", "tabs.open", "tabs.move", "tabs.rename", "tabs.set", "keys.profile", "links.open"}
    check("the extensions' commands are registered", wait(lambda: needed <= {c["name"] for c in call("command.list")["commands"] if c.get("available")}, 40))
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
    colored = subprocess.run([*HARNESS, "capture-pane", "-p", "-e"], capture_output=True, text=True).stdout
    chip = next((line for line in colored.splitlines() if "MYTITLE" in line), "")
    check("the active (live) tab chip is bold", "\x1b[1m" in chip or ";1m" in chip or "[1;" in chip, repr(chip))

    # Marks on other tabs (opt-in): this tab's program reports working, then blocked, then done (OSC 7501)
    # while the first tab is shown. Each next state waits for the test to release it (a file), after the test
    # observed the previous one: no timed windows that load could miss (testing audit item 5).
    call("command.invoke", {"name": "tabs.set", "args": {"field": "marks", "value": True}})
    gate = f"{R}/home/release"
    keys("printf '\\033]7501;state=working:app=e2e\\007';"
         f" until [ -e {gate}-blocked ]; do /usr/bin/sleep 0.1; done; printf '\\033]7501;state=blocked:app=e2e\\007';"
         f" until [ -e {gate}-done ]; do /usr/bin/sleep 0.1; done; printf '\\033]7501;state=done:app=e2e\\007'", literal=True)
    keys("Enter")
    resource = lambda view: call("workspace.snapshot")["entries"][f"view/{view}"]["value"]["resource"]
    status = lambda: next(r.get("status") or {} for r in call("workspace.snapshot")["resources"] if r["id"] == resource(second))
    check("the core sees the agent working", wait(lambda: status().get("state") == "working", 10), status())
    prefix("1")
    open(f"{gate}-blocked", "w").close()
    check("a background agent that waits for you marks its tab ⚠", wait(lambda: "MYTITLE₂ ⚠" in bar(), 10), bar())
    print("CAPTURE mark-blocked\n" + bar(), flush=True)
    open(f"{gate}-done", "w").close()
    check("one that finished since you looked marks it ✓", wait(lambda: "MYTITLE₂ ✓" in bar(), 10),
          bar() + "\n" + str([{k: r.get(k) for k in ("id", "state", "status", "available", "stale")} for r in call("workspace.snapshot")["resources"]]))
    prefix("2")
    check("showing the tab clears its mark", wait(lambda: "✓" not in bar()), bar())
    prefix("1")
    time.sleep(1)
    check("a finish you have seen stays unmarked", "✓" not in bar() and "MYTITLE₂" in bar(), bar())
    call("command.invoke", {"name": "tabs.set", "args": {"field": "marks", "value": False}})
    prefix("2")

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

    # Links: a fake opener records what it is asked to open; the "editor" prints its arguments and waits.
    opener = f"{R}/home/opener.sh"
    with open(opener, "w") as f:
        f.write(f"#!/bin/sh\nprintf '%s\\n' \"$@\" >> {R}/home/opened\n")
    os.chmod(opener, 0o755)
    call("workspace.apply", {"note": "e2e links", "ops": [{"op": "set", "key": "doc/settings/links", "value": {
        "opener": [opener], "editor": ["/bin/sh", "-c", 'echo "EDITOR $*"; exec /usr/bin/sleep 120', "editor"]}}]})
    keys(f"cd {R}/home && /usr/bin/mkdir -p sub && /usr/bin/touch sub/file.txt && /usr/bin/clear && echo https://example.org/zz see sub/file.txt:12", literal=True)
    keys("Enter")
    url = wait(lambda: find("https://example.org/zz see"))
    click(url[0] + 5, url[1], mods=16)
    check("Ctrl-click on a URL runs the opener with it", wait(lambda: open(f"{R}/home/opened").read() == "https://example.org/zz\n"),
          os.path.exists(f"{R}/home/opened") and open(f"{R}/home/opened").read())
    click(url[0] + 5, url[1], button=2)
    check("right-click on a URL offers Open link", wait(lambda: "Open link" in screen()), screen())
    keys("Escape")
    path = wait(lambda: find("sub/file.txt:12"), 5)
    if path is None:
        print("NO PATH ON SCREEN\n" + screen(), flush=True)
    click(path[0] + 3, path[1], mods=16)
    check("Ctrl-click on path:line opens the editor at that line in a split", wait(lambda: f"EDITOR +12 {R}/home/sub/file.txt" in screen()
          and len(views(tabs()["tabs"][tabs()["active"]])) == 2), screen())
    print("CAPTURE links\n" + screen(), flush=True)
    sizes = lambda: tabs()["tabs"][tabs()["active"]]["node"].get("sizes")
    subprocess.run([*HARNESS, "send-keys", "C-g", "C-Left", "C-Left"])  # one burst: well inside the repeat window
    check("a repeat binding repeats without the prefix", wait(lambda: sizes() == [400, 600]), sizes())
    time.sleep(1.2)
    keys("C-Left")
    time.sleep(0.8)
    check("after the repeat window the key goes to the program", sizes() == [400, 600], sizes())
    prefix("x")  # close the editor pane (its program keeps running until the backend stops)
    wait(lambda: len(views(tabs()["tabs"][tabs()["active"]])) == 1)

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
