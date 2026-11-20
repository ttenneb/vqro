#!/usr/bin/python3
"""End to end: the OpenTUI trial client (`vqro ui opentui`, D29) against a private core, with a real shell in a
private herdr-core through vqro-herdr. Drives it in an outer tmux harness like tests/e2e_tui_providers.py.

Usage: tests/e2e_opentui.py TARGET_DIR   (debug binaries in TARGET_DIR/debug)
Skips (exit 77) when Bun or the client's node_modules are absent: the trial client is optional.
Private /tmp root; stops only what it started.
"""
import json, os, re, shutil, socket, subprocess, sys, tempfile, time

TARGET = sys.argv[1]
VQRO, PHERDR = (os.path.join(TARGET, "debug", b) for b in ("vqro", "vqro-herdr"))
HC = os.path.expanduser("~/.local/bin/herdr-core")
REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CLIENT = os.path.join(REPO, "extensions", "clients", "opentui")
BUN = shutil.which("bun") or os.path.expanduser("~/.local/bin/bun")
if not os.path.isfile(BUN) or not os.path.isdir(os.path.join(CLIENT, "node_modules", "@opentui")):
    print("SKIP OpenTUI trial client: Bun or extensions/clients/opentui/node_modules missing (bun install)")
    sys.exit(77)
R = tempfile.mkdtemp(prefix="pott", dir="/tmp")
for d in ("home", "st", "run", "bin"):
    os.makedirs(os.path.join(R, d))
os.chmod(os.path.join(R, "run"), 0o700)
shutil.copytree(os.path.join(REPO, "extensions"), os.path.join(R, "ext"), ignore=shutil.ignore_patterns("__pycache__", "node_modules"))
os.symlink(os.path.join(CLIENT, "node_modules"), os.path.join(R, "ext", "clients", "opentui", "node_modules"))  # 85 MB: share, don't copy
for name, target in (("python3", shutil.which("python3") or "/usr/bin/python3"), ("bun", BUN)):
    os.symlink(target, os.path.join(R, "bin", name))
with open(f"{R}/mouse.py", "w") as f:  # asks for SGR mouse reports, prints the first one it gets
    f.write("import os, sys, termios, tty\nold = termios.tcgetattr(0)\nos.write(1, b'\\x1b[?1000h\\x1b[?1006hmouse-ready\\r\\n')\n"
            "tty.setraw(0)\ndata = os.read(0, 64)\ntermios.tcsetattr(0, termios.TCSADRAIN, old)\n"
            "os.write(1, b'\\x1b[?1000l\\x1b[?1006l')\nprint('got', repr(data.decode(errors='replace')))\n")
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


COUNT = re.compile(r"\d+ of \d+ · ")  # a list's "n of m · " footer, whatever the catalogue's size


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
    check("tab bar and status bar render", bool(wait(lambda: "₁ " in screen() and "vqro ▾" in screen())), screen())
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
    # Mouse: a program that asked for the mouse (SGR mode) receives a click in its own encoding.
    keys("/usr/bin/clear; /usr/bin/python3 " + f"{R}/mouse.py", literal=True); keys("Enter")
    wait(lambda: "mouse-ready" in screen(), 10)
    lines = screen().splitlines()
    y = next(i for i, l in enumerate(lines) if "mouse-ready" in l) + 1
    keys(f"\x1b[<0;10;{y}M\x1b[<0;10;{y}m", literal=True)
    check("a click reaches a program that asked for the mouse, in SGR encoding", bool(wait(lambda: "got '\\x1b[<0;" in screen(), 10)), screen())
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
    # embedded_views: a panel that references another panel shows its rows, read-only.
    call("workspace.apply", {"note": "e2e embedded", "ops": [
        {"op": "set", "key": "view/e2e-src", "value": {"kind": "panel", "items": [{"text": "embedded-source-row", "command": "tabs.open"}]}},
        {"op": "set", "key": "view/e2e-root", "value": {"kind": "panel", "title": "Composed", "requires": ["embedded_views"],
         "items": [{"text": "own-row"}, {"type": "view", "view": "e2e-src", "readonly": True, "text": "source unavailable"}]}}]})
    call("command.invoke", {"name": "tabs.show", "args": {"view": "e2e-root", "title": "Composed"}})
    check("embedded_views: the referenced panel's rows show in place", bool(wait(lambda: "own-row" in screen() and "embedded-source-row" in screen())), screen())
    # markdown text rows and pin: "bottom" rows (agent UI building blocks, D29).
    call("workspace.apply", {"note": "e2e markdown", "ops": [{"op": "set", "key": "view/e2e-md", "value": {"kind": "panel", "title": "Formatted", "items": [
        {"type": "text", "format": "markdown", "text": "## Plan heading\nRun `cargo test` then **ship**\n- first point\n- second point"},
        *[{"text": f"filler {n}"} for n in range(40)],
        {"text": "Messages: 2 waiting", "pin": "bottom"}]}}]})
    call("command.invoke", {"name": "tabs.show", "args": {"view": "e2e-md", "title": "Formatted"}})
    check("markdown: heading, inline code and list markers render without markup", bool(wait(lambda: "Plan heading" in screen() and "Run cargo test then ship" in screen() and "• second point" in screen() and "**" not in screen())), screen())
    check("pin bottom: the pinned row stays visible below a long list", bool(wait(lambda: "Messages: 2 waiting" in screen() and "filler 39" not in screen())), screen())
    # Composer (feature composer): autofocus, multi-line typing, a control, Send through Enter.
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import e2e_echo
    echo = e2e_echo.start(ENV["VQRO_SOCKET"], f"{R}/echo.jsonl")
    call("workspace.apply", {"note": "e2e composer", "ops": [{"op": "set", "key": "view/e2e-comp", "value": {"kind": "panel", "title": "Compose", "items": [
        {"text": "conversation row"},
        {"type": "composer", "id": "c1", "text": "Message Pi", "pin": "bottom", "autofocus": True, "command": "e2e.echo", "args": {"resource": "r2"}, "placeholder": "Message Pi",
         "controls": [{"type": "chips", "arg": "mode", "chips": [{"text": "auto", "value": "auto", "checked": True}, {"text": "steer", "value": "steer"}]}],
         "fallback": {"text": "Message Pi…", "command": "e2e.echo", "input": "Message", "multiline": True}}]}}]})
    call("command.invoke", {"name": "tabs.show", "args": {"view": "e2e-comp", "title": "Compose"}})
    check("composer shows its placeholder, controls and key hint", bool(wait(lambda: "Message Pi" in screen() and "[● auto]" in screen() and "Enter send" in screen())), screen())
    time.sleep(0.5)
    keys("hello there", literal=True); keys("M-Enter"); keys("second line", literal=True)
    check("autofocus: typing goes into the composer, Alt-Enter adds a line", bool(wait(lambda: "hello there" in screen() and "second line" in screen() and "2 lines" in screen())), screen())
    # A click on a control while typing chooses it; it never sends the draft.
    rows = screen().splitlines()
    y = next(i for i, l in enumerate(rows) if "[steer]" in l)
    x = rows[y].index("[steer]") + 2
    keys(f"\x1b[<0;{x};{y + 1}M\x1b[<0;{x};{y + 1}m", literal=True)
    check("a click on a control while typing chooses it and sends nothing", bool(wait(lambda: "[● steer]" in screen() and "hello there" in screen(), 10))
          and not e2e_echo.calls(f"{R}/echo.jsonl"), screen())
    keys("Tab"); time.sleep(0.3); keys("Right"); time.sleep(0.3); keys("Enter"); time.sleep(0.3)
    check("a control sets this composer's value", bool(wait(lambda: "[● steer]" in screen())), screen())
    keys("Left"); keys("Left"); time.sleep(0.3); keys("Enter"); time.sleep(0.3); keys("Enter")
    sent = wait(lambda: e2e_echo.calls(f"{R}/echo.jsonl"), 10)
    check("Enter sends text, controls and args; the draft clears after the reply", bool(sent) and sent[-1].get("text") == "hello there\nsecond line" and sent[-1].get("mode") == "steer"
          and sent[-1].get("resource") == "r2" and bool(wait(lambda: "hello there" not in screen())), f"{sent} {screen()}")
    # Scoped keys (no prefix): Alt-S while typing sends as steer; x in the pane (not typing) runs its binding.
    call("workspace.apply", {"note": "e2e scoped keys", "ops": [
        {"op": "set", "key": "binding/e2e.steer", "value": {"key": "M-s", "scope": "composer", "command": "client.composer", "args": {"action": "send", "set": {"mode": "auto"}}, "when": {"view": "e2e-comp"}}},
        {"op": "set", "key": "binding/e2e.x", "value": {"key": "x", "scope": "pane", "command": "e2e.echo", "args": {"pane_key": "x"}, "when": {"view": "e2e-comp"}}}]})
    time.sleep(0.5)
    keys("quick note", literal=True); time.sleep(0.3); keys("M-s")
    check("a composer-scoped key sends with its preset", bool(wait(lambda: any(c.get("text") == "quick note" and c.get("mode") == "auto" for c in e2e_echo.calls(f"{R}/echo.jsonl")), 10)), str(e2e_echo.calls(f"{R}/echo.jsonl")))
    keys("Escape"); time.sleep(0.6); keys("x")
    check("a pane-scoped key runs without the prefix while not typing", bool(wait(lambda: any(c.get("pane_key") == "x" for c in e2e_echo.calls(f"{R}/echo.jsonl")), 10)), screen())
    # Search (search: true): `/` jumps to matches, opening collapsed blocks; n steps; a pane key copies the selection.
    call("workspace.apply", {"note": "e2e search", "ops": [
        {"op": "set", "key": "view/e2e-comp", "value": {"kind": "panel", "title": "Compose", "search": True, "items": [
            {"text": "you: is the lockfile stale?"}, {"id": "act", "text": "Activity · 2 tools", "children": [{"text": "cargo update rewrote the lockfile", "copy": "COPIED-TEXT"}]},
            {"type": "composer", "id": "c1", "text": "Message Pi", "pin": "bottom", "command": "e2e.echo", "placeholder": "Message Pi"}]}},
        {"op": "set", "key": "binding/e2e.y", "value": {"key": "y", "scope": "pane", "command": "client.copy", "when": {"view": "e2e-comp"}}}]})
    check("collapsed block starts closed", bool(wait(lambda: "Activity · 2 tools" in screen() and "rewrote" not in screen())), screen())
    keys("/"); time.sleep(0.3); keys("lockfile", literal=True)
    check("search shows the match count while typing", bool(wait(lambda: "1 of 2" in screen())), screen())
    keys("Enter"); time.sleep(0.3); keys("n")
    check("n opens the collapsed block holding the next match", bool(wait(lambda: "rewrote the lockfile" in screen() and "2 of 2" in screen())), screen())
    keys("y")
    check("a pane key copies the selected row (reports what the terminal took)", bool(wait(lambda: "copy: " in screen())), screen())
    echo.close()
    keys("C-g")
    check("prefix menu lists recovery actions", bool(wait(lambda: "undo last change" in screen() and "safe mode / recovery choices" in screen())), screen())
    keys(",")
    check("a command's popup reply shows Settings over the terminal", bool(wait(lambda: "Settings" in screen() and "Extensions" in screen() and COUNT.search(screen()))), screen())
    # Find Tabs by search (the list's order is its own: flat, sortable), then switch it.
    keys("/"); time.sleep(0.2); keys("tabs", literal=True); keys("Enter"); time.sleep(0.3)
    keys("Space")
    check("dependency warning as a pick-one menu over the popup", bool(wait(lambda: "Turn off Tabs: breaks" in screen())), screen())
    # Escape followed at once by another key reads as Alt+key: pause like a person would.
    keys("Escape"); time.sleep(0.5)
    # list_detail: the selected extension's details beside the list; Tab moves into them.
    # The window is 90 columns now: the detail sits below the list (narrow layout).
    check("list_detail shows the selected row's details with its actions", bool(wait(lambda: COUNT.search(screen()) and "Restart" in screen() and "View log" in screen()
        and any("[x] Tabs" in line for line in screen().splitlines()))), screen())
    keys("Tab"); time.sleep(0.3); keys("Enter")
    check("Tab focuses the detail; its first action runs from there", bool(wait(lambda: "Turn off Tabs: breaks" in screen())), screen())
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
