#!/usr/bin/python3
"""End to end: the plain Python text client (`vqro ui text`, the second base-profile client, D29) against a
private core with the bundled extensions and the in-process fake backend.

Usage: tests/e2e_text_client.py TARGET_DIR   (debug binary TARGET_DIR/debug/vqro)
Private /tmp root; herdr/tmux providers are not used. Stops only what it started. Prints PASS/FAIL lines.
"""
import json, os, shutil, subprocess, sys, tempfile, time

TARGET = sys.argv[1]
VQRO = os.path.join(TARGET, "debug", "vqro")
REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
R = tempfile.mkdtemp(prefix="ptxt", dir="/tmp")
for d in ("home", "st", "run", "bin"):
    os.makedirs(os.path.join(R, d))
os.chmod(os.path.join(R, "run"), 0o700)
shutil.copytree(os.path.join(REPO, "extensions"), os.path.join(R, "ext"), ignore=shutil.ignore_patterns("__pycache__", "node_modules"))
os.symlink(shutil.which("python3") or "/usr/bin/python3", os.path.join(R, "bin", "python3"))
ENV = {"HOME": f"{R}/home", "PATH": f"{R}/bin", "VQRO_STATE_DIR": f"{R}/st", "VQRO_SOCKET": f"{R}/p.sock",
       "VQRO_EXTENSIONS_DIR": f"{R}/ext", "XDG_RUNTIME_DIR": f"{R}/run", "HC_SOCKET": f"{R}/hc.sock", "HC_SESSION": "txt",
       "VQRO_HERDR": '["/bin/false"]', "LANG": "C.UTF-8"}
results = []


def check(name, ok, detail=""):
    results.append(bool(ok))
    print(("PASS " if ok else "FAIL ") + name + (f"  [{detail}]" if detail and not ok else ""), flush=True)


def vqro(*args, timeout=30):
    return subprocess.run([VQRO, *args], env=ENV, capture_output=True, text=True, timeout=timeout)


def call(method, params=None):
    out = vqro("call", method, json.dumps(params or {}))
    return json.loads(out.stdout) if out.returncode == 0 else {"error": out.stderr}


def text(commands):
    out = vqro("ui", "text", "--", "--once", commands)
    return out.stdout + out.stderr


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


core = interactive = None
try:
    core = subprocess.Popen([VQRO, "serve", "--fake"], env=ENV, stdout=open(f"{R}/serve.log", "w"), stderr=subprocess.STDOUT)
    wait(lambda: os.path.exists(ENV["VQRO_SOCKET"]), 10)
    wait(lambda: call("extension.list").get("extensions", {}).get("settings", {}).get("display_status") == "running")
    # The fake backend serves spawns: take the declared herdr default away (privately) and stop both providers.
    herdr = call("workspace.snapshot")["entries"]["extension/herdr"]["value"]
    call("workspace.apply", {"ops": [{"op": "set", "key": "extension/herdr", "value": dict(herdr, default=False, enabled=False)}]})
    call("command.invoke", {"name": "tabs.open"})
    listed = vqro("ui", "--list").stdout
    check("vqro ui --list names the built-in and the text client", listed.startswith("tui\t") and "\ntext\tText client" in listed, listed)
    out = text("show")
    check("show outlines the layout with a numbered pane", "tabs: [1:" in out and "*pane 1:" in out, out)
    out = text("view 1")
    check("terminal panes are named, not drawn (no terminal profile)", "this client does not show terminal contents" in out, out)
    out = text("p")
    check("prefix menu lists recovery actions", all(s in out for s in ("undo last change", "reset layout", "safe mode / recovery choices")), out)
    out = text("menu tab")
    check("contributed menus resolve for the focused pane", "Rename tab" in out and "Close tab" in out, out)
    # Settings through its own command, then the dependency warning as pick-one options (D29).
    out = text("run settings.open; filter tabs")
    check("a popup reply shows the panel", "== Settings" in out and "Tabs (tabs)" in out, out)
    check("turning off a needed extension is offered as one confirmation", "Turn off Tabs: breaks" in out, out)
    out = text("run settings.open; filter tabs; do 1 2")
    check("cancel switches nothing", call("extension.list")["extensions"]["tabs"]["display_status"] == "running", out)
    text("run settings.open; filter tabs; do 1 1")
    check("confirmed choice turns it off", wait(lambda: call("extension.list")["extensions"]["tabs"]["display_status"] == "off"))
    # The sidebar needs tabs.show and nothing needs the sidebar: off directly, then on with the offer.
    out = text("run settings.open; filter sidebar; do 1")
    check("an extension nobody needs turns off directly", wait(lambda: call("extension.list")["extensions"]["sidebar"]["display_status"] == "off"), out)
    out = text("run settings.open; filter sidebar")
    check("turning on offers the needed extension", "Turn on Sidebar and Tabs" in out, out)
    text("run settings.open; filter sidebar; do 1 1")
    check("offer turns both on", wait(lambda: all(call("extension.list")["extensions"][n]["display_status"] == "running" for n in ("tabs", "sidebar"))))
    # A view of an unknown kind (the logs extension's logs.tail) is shown through its fallback panel.
    with open(f"{R}/st/ext-tabs.log", "a") as log:
        log.write("text-client-log-line\n")
    out = wait(lambda: (lambda o: o if "text-client-log-line" in o else None)(text('run logs.open {"name": "tabs"}')))
    check("a custom view kind is drawn through its fallback", out and "== Log: tabs" in out, out)
    # Recovery: safe mode, the shared chooser, session allowance, leave.
    text("p 5")
    check("prefix row enters safe mode", wait(lambda: call("workspace.snapshot").get("safe_mode") is True))
    out = text("safe")
    check("safe-mode chooser is the shared content", "== Safe mode recovery" in out and "Providers / backends" in out and "Enable selected" in out, out)
    check("chooser names what extensions need", "agents" in out and "-- needs" in out, out)
    text("safe toggle settings; safe apply")
    check("chooser applies a session allowance", wait(lambda: call("extension.list")["extensions"]["settings"]["display_status"] == "running"))
    check("saved enabled declarations are unchanged", call("workspace.snapshot")["entries"]["extension/settings"]["value"].get("enabled", True) is True)
    text("safe leave")
    check("leave safe mode", wait(lambda: call("workspace.snapshot").get("safe_mode") is False))
    # An interactive client declares its profile and features; the core reports them.
    interactive = subprocess.Popen([VQRO, "ui", "text"], env=ENV, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    found = wait(lambda: next((c for c in call("client.list")["clients"] if c.get("client") == "text"), None))
    check("client.list shows the text client's base profile and features", found and found["profiles"] == ["base"] and "icon_names" in found["features"], found)
    interactive.stdin.write("show\nquit\n")
    interactive.stdin.flush()
    check("interactive client quits", interactive.wait(10) == 0)
    banner = interactive.stdout.read()
    check("interactive banner points to help and recovery", "`help` for commands, `p` for recovery" in banner, banner)
    out = vqro("ui", "missing-client")
    check("an unknown client names the fallback", out.returncode != 0 and "vqro ui" in out.stderr, out.stderr)
finally:
    if interactive and interactive.poll() is None:
        interactive.kill()
    if core:
        core.terminate()
        try:
            core.wait(10)
        except subprocess.TimeoutExpired:
            core.kill()
    def mine(pid):
        try:
            return R.encode() in open(f"/proc/{pid}/environ", "rb").read()
        except OSError:
            return False
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
