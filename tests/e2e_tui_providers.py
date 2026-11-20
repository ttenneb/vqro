#!/usr/bin/python3
"""Opt-in TUI smoke over wire providers: the real `vqro tui` (in an outer tmux harness) shows and drives
terminals that live in vqro-herdr (private herdr-core) and vqro-tmux (its private tmux server).

Usage: tests/e2e_tui_providers.py TARGET_DIR   (debug binaries in TARGET_DIR/debug)
Private /tmp root; providers and herdr-core run with an empty PATH; default extensions get python3 only.
Stops only what it started.
"""
import json, os, shutil, socket, subprocess, sys, tempfile, time

TARGET = sys.argv[1]
VQRO, PHERDR, PTMUX = (os.path.join(TARGET, "debug", b) for b in ("vqro", "vqro-herdr", "vqro-tmux"))
HC = os.path.expanduser("~/.local/bin/herdr-core")
REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
with open(os.path.join(REPO, "extensions", "defaults.json"), encoding="utf-8") as defaults:
    BUILTIN_COUNT = len(json.load(defaults)["extensions"])
R = tempfile.mkdtemp(prefix="pe2t", dir="/tmp")
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
       "XDG_RUNTIME_DIR": f"{R}/run", "XDG_CONFIG_HOME": f"{R}/cfg", "HC_SOCKET": f"{R}/hc.sock", "HC_SESSION": "tui", "LANG": "C.UTF-8"}
HARNESS = ["/usr/bin/tmux", "-S", f"{R}/harness.sock", "-f", "/dev/null"]
results = []


def check(name, ok, detail=""):
    results.append(ok)
    print(("PASS " if ok else "FAIL ") + name + (f"  [{detail}]" if detail and not ok else ""), flush=True)


def call(method, params=None):
    s = socket.socket(socket.AF_UNIX)
    s.connect(ENV["VQRO_SOCKET"])
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


POPUPS = ("╭ Settings", "╭ Log:", "╭ Pi runtime")


def open_settings():
    """Open Settings from a known state. The popup appears when the reply to `settings.open` is applied, and that
    resets the popup's state (filter, selection). A popup still open (Escape only cleared its filter) would look
    ready at once, and keys typed into it would be lost when the late reply lands. So: close, then open."""
    for _ in range(5):
        if not any(p in screen() for p in POPUPS):
            break
        keys("Escape"); wait(lambda: not any(p in screen() for p in POPUPS), 2)
    assert not any(p in screen() for p in POPUPS), screen()
    keys("C-g", ",")
    return wait(lambda: "╭ Settings" in screen() and f"{BUILTIN_COUNT} of {BUILTIN_COUNT}" in screen())


core = None
try:
    core = subprocess.Popen([VQRO, "serve", "--fake"], env=ENV, stdout=open(f"{R}/serve.log", "w"), stderr=subprocess.STDOUT)
    wait(lambda: os.path.exists(ENV["VQRO_SOCKET"]), 10)
    call("workspace.apply", {"note": "providers", "ops": [
        {"op": "set", "key": "extension/herdr", "value": {"argv": ["/usr/bin/env", "PATH=", PHERDR, "--hc", HC, "--name", "herdr"]}},
        {"op": "set", "key": "extension/tmux", "value": {"argv": ["/usr/bin/env", "PATH=", PTMUX, "--tmux", "/usr/bin/tmux", "--name", "tmux"]}}]})
    ok = wait(lambda: all(call("workspace.snapshot")["providers"].get(n, {}).get("state") == "connected" for n in ("herdr", "tmux")), 30)
    check("providers connected (with default extensions running)", bool(ok))
    with open(f"{R}/effects.sh", "w") as f:
        f.write("printf '\\033]52;c;aGk=\\007\\007\\033]9;e2e-note\\007\\033]777;notify;T;B\\007\\033]2;e2e-title\\007\\033[5 q\\033[2mdim\\033[0m\\ne2e-effects-done\\n'\n")
    with open(f"{R}/tui.sh", "w") as f:
        f.write("".join(f"export {k}='{v}'\n" for k, v in ENV.items()) + f"exec {VQRO} tui\n")
    subprocess.run([*HARNESS, "new-session", "-d", "-x", "120", "-y", "32", f"/bin/sh {R}/tui.sh"], env={"PATH": "/usr/bin:/bin", "HOME": f"{R}/home"})
    time.sleep(3)
    for name in ("herdr", "tmux"):
        rid = call("resource.spawn", {"provider": name, "argv": ["/bin/sh"], "name": f"tui-{name}"})["resource"]["id"]
        call("command.invoke", {"name": "vqro.layout.reset"})
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
        # A program's clipboard write, bell, notification, dim text and cursor shape reach the outer terminal; its
        # title names the pane. Raw bytes from the TUI to the harness, as the user's terminal would get them.
        raw = f"{R}/raw-{name}.log"
        subprocess.run([*HARNESS, "pipe-pane", "-o", f"cat >> {raw}"])
        keys(f"/bin/sh {R}/effects.sh", literal=True); keys("Enter")
        got = lambda: open(raw, "rb").read() if os.path.exists(raw) else b""
        wanted = [b"\x1b]52;c;aGk=\x07", b"\x07", b"\x1b]9;e2e-note\x07", b"\x1b]777;notify;T;B\x07", b"\x1b[5 q", b"\x1b[2m"]
        ok = wait(lambda: all(w in got() for w in wanted), 10)
        check(f"{name}: program clipboard, bell, notifications, dim and cursor shape reach the terminal", bool(ok), repr([w for w in wanted if w not in got()]))
        if name == "herdr":  # herdr-core reports program titles (OSC 2); unnamed panes show it (unit test in tui)
            title = lambda: next(r for r in call("workspace.snapshot")["resources"] if r["id"] == rid).get("title")
            check(f"{name}: the backend reports the program's title for pane names", wait(lambda: title() == "e2e-title", 10) is not None, str(title()))
        if name == "herdr":
            # Terminal reading: a mouse drag selects and copies (OSC 52); prefix [ scrolls back and / searches.
            keys("i=0; while [ $i -lt 300 ]; do echo line-$i; i=$((i+1)); done", literal=True); keys("Enter")
            wait(lambda: "line-299" in screen(), 10)
            rows = screen().splitlines()
            y = next(i for i, l in enumerate(rows) if "line-299" in l)
            x = rows[y].index("line-299")
            keys(f"\x1b[<0;{x + 1};{y + 1}M\x1b[<32;{x + 4};{y + 1}M\x1b[<32;{x + 8};{y + 1}M\x1b[<0;{x + 8};{y + 1}m", literal=True)
            b64 = __import__("base64").b64encode(b"line-299").decode()
            check(f"{name}: dragging over terminal text copies it", bool(wait(lambda: f"\x1b]52;c;{b64}\x07".encode() in got(), 10)), screen()[-300:])
            keys("C-g"); time.sleep(0.4); keys("["); time.sleep(0.4); keys("/"); keys("line-12", literal=True)
            check(f"{name}: scroll mode searches the history", bool(wait(lambda: "line-12" in screen() and " of " in screen() and "n/N" in screen(), 10)), screen()[-400:])
            keys("Enter"); time.sleep(0.2); keys("Escape"); time.sleep(0.5)
            check(f"{name}: Esc leaves scroll mode at the live screen", bool(wait(lambda: "line-299" in screen() and "↑" not in screen(), 10)), screen()[:300])
        subprocess.run([*HARNESS, "pipe-pane"])
        # Reattach (a new TUI): the backend's history repaint must not copy or ring again.
        raw2 = f"{R}/raw2-{name}.log"
        subprocess.run([*HARNESS, "respawn-pane", "-k", f"/bin/sh {R}/tui.sh"], env={"PATH": "/usr/bin:/bin", "HOME": f"{R}/home"})
        subprocess.run([*HARNESS, "pipe-pane", "-o", f"cat >> {raw2}"])
        marker = "line-299" if name == "herdr" else "e2e-effects-done"  # the last output before the reattach
        wait(lambda: marker in screen(), 15); time.sleep(1)
        again = open(raw2, "rb").read() if os.path.exists(raw2) else b""
        check(f"{name}: reattaching does not replay a copy or a notification", b"]52;" not in again and b"]9;e2e" not in again and marker.encode() in again, repr(again[-300:]))
        subprocess.run([*HARNESS, "pipe-pane"])
        call("resource.kill", {"resource": rid})
        time.sleep(1)
    # Tab bar: chips with subscript numbers after the name (doc/settings/tabs default), then only "+" (new tab) and
    # the compact new-agent "+<agent icon>"; the other buttons moved to menus and keys.
    bar = lambda: screen().splitlines()[0]
    check("tab bar shows chips with subscript numbers and only + and +agent", bool(wait(lambda: "tui-tmux₁ " in bar() and
        bar().rstrip().endswith(" +  +◆"), 10)) and not any(
        word in bar() for word in ("New Pi agent", "Agents", "Close ended tabs", "Switch workspace", "1:")), bar())
    colored = subprocess.run([*HARNESS, "capture-pane", "-p", "-e"], capture_output=True, text=True).stdout.splitlines()[0]
    print("CAPTURE tabbar-chips\n" + bar() + "\nANSI: " + repr(colored), flush=True)
    # Its program was killed: the tabs extension hints `state` and the chip is drawn dim, the title unchanged.
    check("an ended tab is dim with its title unchanged (no [gone] text)", "[gone]" not in bar() and "[ended]" not in bar()
          and ("\x1b[2m" in colored or ";2m" in colored or "[2;" in colored), repr(colored))
    call("command.invoke", {"name": "tabs.set", "args": {"field": "numbers", "value": "superscript"}})
    call("command.invoke", {"name": "tabs.set", "args": {"field": "position", "value": "before"}})
    check("the tabs setting changes the number style and position", bool(wait(lambda: " ¹tui-" in bar() and "₁" not in bar(), 10)), bar())
    print("CAPTURE tabbar-superscript-before\n" + bar(), flush=True)
    call("workspace.apply", {"ops": [{"op": "delete", "key": "doc/settings/tabs"}]})
    # A running provider process is not proof of a reachable backend. An alternative remains connected.
    saved_herdr = call("workspace.snapshot")["entries"]["extension/herdr"]["value"]
    call("workspace.apply", {"ops":[{"op":"set","key":"extension/herdr","value":dict(saved_herdr,
        argv=["/bin/cat"],kind="provider",default=True)}]})
    check("unreachable default notice is visible despite alternative backends", bool(wait(lambda:
        "No default backend is ready" in screen() and
        call("extension.list")["extensions"]["herdr"]["display_status"] == "provider-not-connected")))
    check("catalogue opens before typing its search", bool(open_settings()) and "Agent controls" in screen(), screen())
    keys("/"); keys("herdr", literal=True); keys("Enter")
    # The detail's Runtime section (`[` from About wraps to the last section) shows the process separately.
    keys("Tab", "[")
    check("catalogue separates disconnected provider from running process", bool(wait(lambda:
        "provider-not-connected" in screen() and "Process" in screen() and "running" in screen().split("Process", 1)[1][:40])), screen())
    keys("BTab")
    call("workspace.apply", {"ops":[{"op":"set","key":"extension/herdr","value":saved_herdr}]})
    check("backend notice and catalogue recover without reopening settings", bool(wait(lambda:
        call("extension.list")["extensions"]["herdr"]["display_status"] == "running" and
        "provider-not-connected" not in screen() and "No default backend is ready" not in screen())))
    # Settings is an extension using the same list/detail primitives available to every panel.
    check("Extensions page opens with static manifests", bool(open_settings()) and "Agent controls" in screen(), screen())
    wide = screen()
    print("CAPTURE extensions-wide\n" + wide, flush=True)
    keys("/"); keys("pi launcher", literal=True); keys("Enter")  # "pi" alone also matches tier1's New Pi agent
    check("manifest list search and count", bool(wait(lambda: f"1 of {BUILTIN_COUNT}" in screen() and "Pi launcher" in screen())))
    # Clicking a row selects details; only its checkbox or explicit keyboard activation changes state.
    row = next((i, line) for i, line in enumerate(screen().splitlines()) if "[x]" in line and "Pi launcher" in line)
    before = call("extension.list")["extensions"]["pi"]["generation"]
    x = row[1].index("Pi launcher") + 2; y = row[0] + 1
    keys(f"\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m", literal=True); time.sleep(.2)
    check("mouse row selection has no toggle effect", call("extension.list")["extensions"]["pi"]["generation"] == before)
    # Tab enters detail actions; restart is generic and keeps the list selection/filter.
    keys("Tab", "Right", "Enter")
    check("detail restart action", bool(wait(lambda: call("extension.list")["extensions"]["pi"]["generation"] > before)))
    check("detail restart preserves list search", bool(wait(lambda: f"1 of {BUILTIN_COUNT}" in screen())))
    # A restart stops the old process (its commands are fenced) and starts a new one, which registers its
    # settings command a moment later. The open popup still shows the chips drawn before the restart, so
    # acting on them now invokes a command that is gone. Wait for the new process's command, then reopen
    # Settings: its content is built from that state, and its keyboard position starts at the first chip.
    # Probe the command itself (it only publishes its settings view): an answer means the new process has it.
    check("restarted extension registered its settings command again", bool(wait(lambda: call("extension.list")["extensions"]["pi"]["generation"] >= before + 2
        and call("command.invoke", {"name": "pi.settings", "args": {}}))))
    open_settings()
    keys("/"); keys("pi launcher", literal=True); keys("Enter")  # "pi" alone also matches tier1's New Pi agent
    wait(lambda: f"1 of {BUILTIN_COUNT}" in screen() and "Extension settings" in screen())
    keys("Tab", "Right", "Right", "Enter")  # Turn off, Restart, Extension settings
    check("manifest settings_command opens its own settings", bool(wait(lambda: "Pi runtime argv" in screen())), screen())
    open_settings()
    keys("End", "PageUp", "Home")
    check("Home End and paging retain catalogue", bool(wait(lambda: f"{BUILTIN_COUNT} of {BUILTIN_COUNT}" in screen())))
    # Dependencies: turning off a needed extension warns once; turning on one with an off need offers it too.
    keys("/"); keys("tabs", literal=True); keys("Enter")
    wait(lambda: f"1 of {BUILTIN_COUNT}" in screen() and "[x] Tabs" in screen())  # one dense row: title, id and kind columns
    keys("Space")
    check("turning off a needed extension warns what breaks", bool(wait(lambda: "Turn off Tabs: breaks" in screen() and "Sidebar" in screen())), screen())
    print("CAPTURE dependency-warning\n" + screen(), flush=True)
    check("the warning does not switch anything by itself", call("extension.list")["extensions"]["tabs"]["enabled"] is True)
    keys("Enter")
    check("confirmed warning turns it off", bool(wait(lambda: call("extension.list")["extensions"]["tabs"]["display_status"] == "off")))
    check("turn-off confirmation closes before changing the list filter", bool(wait(lambda: "Turn off Tabs: breaks" not in screen())))
    # The sidebar needs tabs.show (workspaces only works with tabs.open since each workspace has its own tabs).
    check("core reports the dependent's missing need", bool(wait(lambda: call("extension.list")["extensions"]["sidebar"]["dependencies"]["status"] == "missing")))
    # A fresh popup has no filter or search under way: keys typed into it act on a known state.
    open_settings(); keys("/"); keys("sidebar", literal=True); keys("Enter")
    # The detail's Needs section lists each need as a row: provider, capability, relation, and its status chip.
    keys("Tab", "]")
    check("dependents show a needs badge and the need in detail", bool(wait(lambda: f"1 of {BUILTIN_COUNT}" in screen() and "needs Tabs (off)" in screen()
        and any("Tabs" in line and "command/tabs.show" in line and " off " in line for line in screen().splitlines()))), screen())
    print("CAPTURE extension-needs\n" + screen(), flush=True)
    keys("Enter")  # still in the detail: the Turn off chip is first; turn the sidebar off directly (nothing needs it)
    check("an extension nobody needs turns off without a warning", bool(wait(lambda: call("extension.list")["extensions"]["sidebar"]["display_status"] == "off")))
    check("Sidebar off state is rendered before toggling it back on", bool(wait(lambda: "[ ] Sidebar" in screen())))
    keys("BTab"); time.sleep(.3); keys("Space")
    check("turning on offers the needed extension in one confirmation", bool(wait(lambda: "Turn on Sidebar and Tabs" in screen())), screen())
    print("CAPTURE dependency-offer\n" + screen(), flush=True)
    keys("Enter")
    check("confirmed offer turns both on", bool(wait(lambda: all(call("extension.list")["extensions"][n]["display_status"] == "running" for n in ("tabs", "sidebar")))))
    # Another RPC can observe running before the TUI receives/applies its confirmation result.
    check("confirmation closes before the next catalogue action", bool(wait(lambda: "Turn on Sidebar and Tabs" not in screen())))
    open_settings()
    # Extension logs (review P2): a view of its own kind drawn through its fallback panel, following new lines.
    with open(f"{R}/st/ext-tabs.log", "a") as log:
        log.write("".join(f"e2e-tail-{n}\n" for n in range(5)))
    keys("/"); keys("tabs", literal=True); keys("Enter")
    wait(lambda: f"1 of {BUILTIN_COUNT}" in screen() and "View log" in screen())
    keys("Tab", "Right", "Right", "Right", "Enter")  # Turn off, Restart, Extension settings, View log
    check("View log opens the log viewer through its fallback panel", bool(wait(lambda: "Log: tabs" in screen() and "e2e-tail-4" in screen())), screen())
    with open(f"{R}/st/ext-tabs.log", "a") as log:
        log.write("".join(f"e2e-tail-{n}\n" for n in range(5, 80)))
    check("the viewer follows new lines without a key press", bool(wait(lambda: "e2e-tail-79" in screen())), screen())
    print("CAPTURE log-viewer\n" + screen(), flush=True)
    keys("Up", "Up", "Up")
    with open(f"{R}/st/ext-tabs.log", "a") as log:
        log.write("e2e-tail-80\n")
    time.sleep(2.5)
    check("reading earlier lines stops following", "e2e-tail-80" not in screen() and "e2e-tail-76" in screen(), screen())
    check("Settings reopens after the log viewer", bool(open_settings()), screen())
    keys("/"); keys("icons", literal=True); keys("Enter")
    check("Icons search and details are ready before keyboard activation", bool(wait(lambda: "/icons" in screen() and "[x] Icons" in screen() and "Publishes icon sets" in screen())), screen())
    keys("Space")
    check("keyboard turns extension off", bool(wait(lambda: call("extension.list")["extensions"]["icons"]["display_status"] == "off")))
    check("disabled extension keeps static description", bool(wait(lambda: "saved icon set remains" in screen())))
    wait(lambda: "[ ] Icons" in screen())
    icon_row = next((i, line) for i, line in enumerate(screen().splitlines()) if "[ ] Icons" in line)
    x = icon_row[1].index("[ ]") + 2; y = icon_row[0] + 1
    keys(f"\x1b[<0;{x};{y}M\x1b[<0;{x};{y}m", literal=True)
    check("mouse checkbox turns extension on", bool(wait(lambda: call("extension.list")["extensions"]["icons"]["display_status"] == "running")))
    broken = dict(call("workspace.snapshot")["entries"]["extension/icons"]["value"], argv=["vqro-extension-page-test-missing"])
    call("workspace.apply", {"ops":[{"op":"set","key":"extension/icons","value":broken}]})
    check("failed extension retains manifest and visible reason", bool(wait(lambda: "failed" in screen() and "extension program" in screen() and "vqro-extension-page-test-missing" in screen() and "Publishes icon sets" in screen())))
    subprocess.run([*HARNESS, "resize-window", "-x", "72", "-y", "36"], capture_output=True)
    time.sleep(.3)
    check("narrow list detail stacks below", bool(wait(lambda: "Icons (icons)" in screen() and f"1 of {BUILTIN_COUNT}" in screen())))
    narrow = screen(); print("CAPTURE extensions-narrow\n" + narrow, flush=True)
    if len(sys.argv) > 2:
        os.makedirs(sys.argv[2], exist_ok=True)
        for name, content in (("extensions-wide.txt", wide), ("extensions-narrow.txt", narrow)):
            with open(os.path.join(sys.argv[2], name), "w") as f: f.write(content)
    # Restore only our private missing executable, without changing the failed declaration.
    wrapper=f"{R}/bin/vqro-extension-page-test-missing"
    with open(wrapper,"w") as output:
        output.write(f"#!/bin/sh\nexec /usr/bin/python3 '{R}/ext/icons.py'\n")
    os.chmod(wrapper,0o700)
    keys("Escape")  # clear the list filter, then close the popup after its frame arrives
    check("recovery setup clears catalogue filter", bool(wait(lambda:not any(line.strip(" │")=="/icons" for line in screen().splitlines()))))
    keys("Escape")
    check("failed status badge offers recovery", bool(wait(lambda:"failed · recover" in screen() and "Settings ─" not in screen())),screen())
    call("command.invoke",{"name":"vqro.extension.disable","args":{"name":"settings"}})
    check("recovery does not require the Settings extension",call("extension.list")["extensions"]["settings"]["enabled"] is False)
    status=screen().splitlines()
    y=next(i for i,line in enumerate(status) if "failed · recover" in line)
    x=status[y].index("failed")+1
    keys(f"\x1b[<0;{x};{y+1}M\x1b[<0;{x};{y+1}m",literal=True)
    check("failed badge opens built-in restart even without Settings", bool(wait(lambda:"restart icons (failed)" in screen())),screen())
    keys("Down","Enter")
    check("built-in restart recovers failed extension", bool(wait(lambda:call("extension.list")["extensions"]["icons"]["status"]=="running" and
        call("extension.list")["extensions"]["icons"].get("failure") is None)))
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
    # Provider/core exit intentionally no longer owns the tmux backend's lifetime.
    subprocess.run(["/usr/bin/tmux", "-N", "-S", f"{R}/st/tmux.sock", "kill-server"],
                   env={"HOME": f"{R}/home", "PATH": "/usr/bin:/bin"}, capture_output=True)
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
