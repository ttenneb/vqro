#!/usr/bin/env python3
"""Opt-in TUI check: long pick-one lists are searchable; short ones are unchanged.

The real `vqro tui` runs in an outer tmux harness against a private `vqro serve --fake` with no
extensions. A test client registers `e2e.pick` and records what each choice sends. Checks:
- A select with 24 options opens a menu titled "24 of 24 · type to filter"; typing narrows it ("1 of 24"),
  Enter sends exactly the kept row's value.
- Esc clears the typed text first (menu stays, "24 of 24"), a second Esc closes it.
- A mouse click on a filtered row sends that row's value (clicks follow the filter).
- A two-option select shows no count and ignores typing.

Usage: tests/e2e_tui_pick_search.py TARGET_DIR   (debug binaries in TARGET_DIR/debug)
Private /tmp root; nothing live is touched. Stops only what it started.
"""

import sys
if sys.platform != "linux":
    print("SKIP e2e_tui_pick_search: requires Linux /proc and namespace/process probes")
    raise SystemExit(77)

import json, os, shutil, socket, subprocess, sys, tempfile, threading, time

TARGET = sys.argv[1]
VQRO = os.path.join(TARGET, "debug", "vqro")
R = tempfile.mkdtemp(prefix="pps", dir="/tmp")
for d in ("home", "st", "xst", "run", "cfg", "ext"):
    os.makedirs(os.path.join(R, d))
os.chmod(os.path.join(R, "run"), 0o700)
with open(os.path.join(R, "ext", "defaults.json"), "w") as f:
    json.dump({"version": 2, "extensions": []}, f)  # no providers or UI extensions: nothing else starts
ENV = {"HOME": f"{R}/home", "PATH": "/usr/bin:/bin", "TERM": "xterm-256color", "SHELL": "/bin/sh", "VQRO_STATE_DIR": f"{R}/st",
       "VQRO_SOCKET": f"{R}/p.sock", "VQRO_EXTENSIONS_DIR": f"{R}/ext", "XDG_STATE_HOME": f"{R}/xst",
       "XDG_RUNTIME_DIR": f"{R}/run", "XDG_CONFIG_HOME": f"{R}/cfg", "HC_SESSION": "pick", "HC_SOCKET": f"{R}/hc.sock",
       "VQRO_HERDR": '["/bin/false"]', "LANG": "C.UTF-8"}
HARNESS = ["/usr/bin/tmux", "-S", f"{R}/harness.sock", "-f", "/dev/null"]
results, picked = [], []


def check(name, ok, detail=""):
    results.append(ok)
    print(("PASS " if ok else "FAIL ") + name + (f"\n{detail}" if detail and not ok else ""), flush=True)


def call(method, params=None):
    s = socket.socket(socket.AF_UNIX)
    s.connect(ENV["VQRO_SOCKET"])
    f = s.makefile("rwb", buffering=0)
    for i, (m, p) in enumerate([("hello", {"client": "e2e-pick"}), (method, params or {})]):
        f.write((json.dumps({"jsonrpc": "2.0", "id": i, "method": m, "params": p}) + "\n").encode())
        while True:
            msg = json.loads(f.readline())
            if msg.get("id") == i:
                break
    s.close()
    if "error" in msg:
        raise RuntimeError(f"{method}: {msg['error']}")
    return msg["result"]


def serve_command():
    """Own `e2e.pick` on one connection and record the value of every invocation."""
    s = socket.socket(socket.AF_UNIX)
    s.connect(ENV["VQRO_SOCKET"])
    f = s.makefile("rwb", buffering=0)
    for i, (m, p) in enumerate([("hello", {"client": "e2e-pick-owner"}), ("command.register", {"name": "e2e.pick", "title": "E2E pick"})]):
        f.write((json.dumps({"jsonrpc": "2.0", "id": f"own-{i}", "method": m, "params": p}) + "\n").encode())
    registered.set()
    for line in f:
        msg = json.loads(line)
        if msg.get("method") == "command.invoke":
            picked.append(msg["params"].get("args", {}).get("value"))
            f.write((json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {}}) + "\n").encode())


def wait(pred, timeout=10):
    end = time.time() + timeout
    while time.time() < end:
        try:
            v = pred()
            if v:
                return v
        except Exception:
            pass
        time.sleep(0.2)
    return None


def screen():
    return subprocess.run([*HARNESS, "capture-pane", "-p"], capture_output=True, text=True).stdout


def keys(*k, literal=False):
    subprocess.run([*HARNESS, "send-keys", *(["-l"] if literal else []), *k])


def click(text):
    """Left-click the first screen line containing `text`, on that text."""
    for y, line in enumerate(screen().splitlines()):
        if text in line:
            x = line.index(text) + 2
            keys(f"\x1b[<0;{x};{y+1}M\x1b[<0;{x};{y+1}m", literal=True)
            return True
    return False


registered = threading.Event()
core = None
try:
    core = subprocess.Popen([VQRO, "serve", "--fake"], env=ENV, stdout=open(f"{R}/serve.log", "w"), stderr=subprocess.STDOUT)
    wait(lambda: os.path.exists(ENV["VQRO_SOCKET"]), 10)
    threading.Thread(target=serve_command, daemon=True).start()
    registered.wait(5)
    wait(lambda: any(c["name"] == "e2e.pick" for c in call("command.list")["commands"]), 5)
    options = [{"label": f"Model {i:02}", "value": f"m{i:02}"} for i in range(24)]
    call("workspace.apply", {"note": "e2e pick panel", "ops": [
        {"op": "set", "key": "view/pick", "value": {"kind": "panel", "title": "Pick test", "items": [
            {"type": "select", "text": "Model choice", "value": "m03", "command": "e2e.pick", "options": options},
            {"type": "select", "text": "Short choice", "value": "a", "command": "e2e.pick",
             "options": [{"label": "Alpha", "value": "a"}, {"label": "Beta", "value": "b"}]}]}},
        {"op": "set", "key": "layout/main", "value": {"view": "pick"}},
        {"op": "set", "key": "workspace/active", "value": {"layout": "main"}}]})
    with open(f"{R}/tui.sh", "w") as f:
        f.write("".join(f"export {k}='{v}'\n" for k, v in ENV.items()) + f"exec {VQRO} tui\n")
    subprocess.run([*HARNESS, "new-session", "-d", "-x", "100", "-y", "40", f"/bin/sh {R}/tui.sh"], env={"PATH": "/usr/bin:/bin", "HOME": f"{R}/home"})
    check("panel shown", bool(wait(lambda: "Model choice" in screen() and "Short choice" in screen(), 15)), screen())

    # 1. Type to filter, Enter sends the kept row.
    keys("Enter")
    check("long select opens a searchable menu", bool(wait(lambda: "24 of 24" in screen() and "type to filter" in screen() and "Model 23" in screen())), screen())
    keys("17", literal=True)
    check("typing narrows it", bool(wait(lambda: "1 of 24" in screen() and "> 17" in screen() and "Model 16" not in screen())), screen())
    keys("Enter")
    check("Enter sends the kept row's value", bool(wait(lambda: picked == ["m17"])), f"picked={picked}\n{screen()}")

    # 2. Esc clears the text, then closes.
    keys("Enter")
    wait(lambda: "24 of 24" in screen())
    keys("2", literal=True)
    wait(lambda: "6 of 24" in screen())  # 02, 12, 20-23
    keys("Escape")
    check("Esc clears the typed text and keeps the menu", bool(wait(lambda: "24 of 24" in screen() and "Model 05" in screen())), screen())
    keys("Escape")
    check("a second Esc closes it", bool(wait(lambda: "of 24" not in screen() and "Model choice" in screen())), screen())

    # 3. Clicks follow the filter.
    keys("Enter")
    wait(lambda: "24 of 24" in screen())
    keys("2", literal=True)
    wait(lambda: "6 of 24" in screen())
    click("Model 12")
    check("a click on a filtered row sends that row", bool(wait(lambda: picked == ["m17", "m12"])), f"picked={picked}\n{screen()}")

    # 4. Short lists are unchanged.
    keys("Down"); time.sleep(0.3); keys("Enter")
    opened = wait(lambda: "Beta" in screen())
    keys("b", literal=True); time.sleep(0.5)
    shot = screen()
    check("a short select shows no count and ignores typing", bool(opened) and "of 2" not in shot and "Alpha" in shot and "> b" not in shot, shot)
    keys("Escape"); time.sleep(0.3)
    check("no extra value was sent", picked == ["m17", "m12"], f"picked={picked}")

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
