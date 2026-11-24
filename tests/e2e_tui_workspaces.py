#!/usr/bin/env python3
"""Opt-in TUI check: each window shows its own workspace.

Two real `vqro tui` windows run in an outer tmux harness against a private `serve --fake` core with the
bundled tabs, panes, workspaces and sidebar extensions. Checks:
- a new window opens on the workspace last switched to (the shared `workspace/active`);
- switching (prefix w, the switcher popup) moves only that window; the other keeps its workspace;
- the status bar names each window's own workspace;
- a new tab from a window goes to that window's workspace and starts in its folder;
- prefix R (reset layout) shows `main` in that window only.

Usage: tests/e2e_tui_workspaces.py TARGET_DIR   (debug binaries in TARGET_DIR/debug)
Private /tmp root; nothing live is touched (no herdr-core: `VQRO_HERDR=/bin/false`). Stops only what it started.
"""
from python_interpreter import python
from extension_layout import copy_packages
import json, os, shutil, socket, subprocess, sys, tempfile, time

TARGET = sys.argv[1]
VQRO = os.path.join(TARGET, "debug", "vqro")
REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
R = tempfile.mkdtemp(prefix="ptw", dir="/tmp")
for d in ("home", "st", "xst", "run", "cfg", "ext", "bin", "a", "b"):
    os.makedirs(os.path.join(R, d))
os.chmod(os.path.join(R, "run"), 0o700)
for f in os.listdir(os.path.join(REPO, "extensions")):
    if f.endswith(".py"):
        shutil.copy(os.path.join(REPO, "extensions", f), os.path.join(R, "ext", f))
shutil.copytree(os.path.join(REPO, "extensions", "manifests"), os.path.join(R, "ext", "manifests"))
copy_packages(os.path.join(REPO,'extensions'),os.path.join(R,'ext'))
with open(os.path.join(R, "ext", "defaults.json"), "w") as f:
    json.dump({"version": 1, "extensions": [{"name": n, "argv": ["python3", f"{n}.py"]} for n in ("tabs", "panes", "workspaces", "sidebar")]}, f)
os.symlink(python(), os.path.join(R, "bin", "python3"))
ENV = {"HOME": f"{R}/home", "PATH": f"{R}/bin", "TERM": "xterm-256color", "SHELL": "/bin/sh", "VQRO_STATE_DIR": f"{R}/st",
       "VQRO_SOCKET": f"{R}/p.sock", "VQRO_EXTENSIONS_DIR": f"{R}/ext", "XDG_STATE_HOME": f"{R}/xst",
       "XDG_RUNTIME_DIR": f"{R}/run", "XDG_CONFIG_HOME": f"{R}/cfg", "HC_SESSION": "tw", "HC_SOCKET": f"{R}/hc.sock",
       "VQRO_HERDR": '["/bin/false"]', "LANG": "C.UTF-8"}
HARNESS = ["/usr/bin/tmux", "-S", f"{R}/harness.sock", "-f", "/dev/null"]
results = []


def check(name, ok, detail=""):
    results.append(bool(ok))
    print(("PASS " if ok else "FAIL ") + name + (f"\n{detail}" if detail and not ok else ""), flush=True)


def call(method, params=None):
    s = socket.socket(socket.AF_UNIX)
    s.connect(ENV["VQRO_SOCKET"])
    f = s.makefile("rwb", buffering=0)
    for i, (m, p) in enumerate([("hello", {"client": "e2e-tui-workspaces"}), (method, params or {})]):
        f.write((json.dumps({"jsonrpc": "2.0", "id": i, "method": m, "params": p}) + "\n").encode())
        while True:
            msg = json.loads(f.readline())
            if msg.get("id") == i:
                break
    s.close()
    if "error" in msg:
        raise RuntimeError(f"{method}: {msg['error']}")
    return msg["result"]


def run(name, **args):
    return call("command.invoke", {"name": name, "args": args})


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


def screen(win):
    return subprocess.run([*HARNESS, "capture-pane", "-p", "-t", f"{win}:0.0"], capture_output=True, text=True).stdout


def status(win):
    lines = [line for line in screen(win).splitlines() if line.strip()]
    return lines[-1] if lines else ""


def keys(win, *k):
    for key in k:
        subprocess.run([*HARNESS, "send-keys", "-t", f"{win}:0.0", key])
        time.sleep(0.3)


def window(win):
    subprocess.run([*HARNESS, "new-session", "-d", "-s", win, "-x", "120", "-y", "30", f"/bin/sh {R}/tui.sh"],
                   env={"PATH": "/usr/bin:/bin", "HOME": f"{R}/home"})


def shows(win, title):
    return wait(lambda: f"▫ {title} " in status(win), 10)


def active():
    return call("workspace.get", {"key": "workspace/active"})["value"]["layout"]


core = None
try:
    core = subprocess.Popen([VQRO, "serve", "--fake", "--safe"], env=ENV, stdout=open(f"{R}/serve.log", "w"), stderr=subprocess.STDOUT)
    wait(lambda: os.path.exists(ENV["VQRO_SOCKET"]), 10)
    # These windows use explicitly scripted tabs; seed policy before extensions start.
    call("workspace.apply", {"ops": [{"op": "set", "key": "doc/settings/workspaces", "value": {"open_shell_when_empty": False}}]})
    call("command.invoke", {"name": "vqro.safe_mode", "args": {"on": False}})
    ready = wait(lambda: {"tabs.open", "workspaces.open", "workspaces.switch"} <= {c["name"] for c in call("command.list")["commands"]}, 30)
    check("extensions ready", bool(ready))
    a, b = f"{R}/a", f"{R}/b"
    run("tabs.open")
    run("workspaces.add", text=a)
    run("workspaces.add", text=b)
    run("workspaces.open", path=a)
    run("workspaces.open", path=b)
    with open(f"{R}/tui.sh", "w") as f:
        f.write("".join(f"export {k}='{v}'\n" for k, v in ENV.items()) + f"exec {VQRO} tui\n")
    window("wa")
    check("a new window opens on the workspace last switched to", bool(shows("wa", "b")), screen("wa"))
    # Window A switches to a through the switcher popup (rows: main, a, b).
    keys("wa", "C-g", "w")
    check("prefix w opens the switcher, marking this window's workspace", bool(wait(lambda: "Switch workspace" in screen("wa") and "current" in screen("wa"), 8)), screen("wa"))
    keys("wa", "Down", "Enter")
    check("switching shows a in window A", bool(shows("wa", "a")), screen("wa"))
    check("the switch is the new shared default", wait(lambda: active().startswith("ws-") and call("workspace.get", {"key": f"doc/layout/{active()}"})["value"]["title"] == "a", 5))
    window("wb")
    check("a second window opens on a (last switched)", bool(shows("wb", "a")), screen("wb"))
    keys("wb", "C-g", "w")
    wait(lambda: "Switch workspace" in screen("wb"), 8)
    keys("wb", "Enter")
    check("window B switches to main", bool(shows("wb", "main")), screen("wb"))
    time.sleep(1)
    check("window A still shows a", bool(shows("wa", "a")), screen("wa"))
    # A new tab from window A goes to a's tabs and starts in a; main (window B) is unchanged.
    la = next(k[len("doc/layout/"):] for k, e in call("workspace.snapshot")["entries"].items()
              if k.startswith("doc/layout/") and e["value"].get("title") == "a")
    def tabs_of(name):
        root = call("workspace.get", {"key": f"layout/{name}"})["value"]
        stack = [root]
        while stack:
            node = stack.pop(0)
            if "tabs" in node:
                return node["tabs"]
            stack += node.get("children", [])
    before_a = len(tabs_of(la))
    main_before = call("workspace.get", {"key": "layout/main"})["value"]
    keys("wa", "C-g", "n")
    grew = wait(lambda: len(tabs_of(la)) == before_a + 1, 8)
    check("prefix n in window A adds a tab to a", bool(grew))
    view = tabs_of(la)[-1]["node"]["view"] if grew else None
    rid = call("workspace.get", {"key": f"view/{view}"})["value"]["resource"] if view else None
    cwd = next((r.get("cwd") for r in call("workspace.snapshot")["resources"] if r["id"] == rid), None)
    check("that shell starts in a's folder", cwd == a, f"cwd={cwd}")
    check("main is unchanged", call("workspace.get", {"key": "layout/main"})["value"] == main_before)
    check("window B still shows main", bool(shows("wb", "main")), screen("wb"))
    # R in window A resets main and shows it there only; B already shows main, so reset from B instead
    # would prove nothing: switch B back to b first.
    keys("wb", "C-g", "w")
    wait(lambda: "Switch workspace" in screen("wb"), 8)
    keys("wb", "Down", "Down", "Enter")
    check("window B switches to b", bool(shows("wb", "b")), screen("wb"))
    keys("wa", "C-g", "R")
    check("prefix R shows main in window A", bool(shows("wa", "main")), screen("wa"))
    time.sleep(1)
    check("window B keeps b after A's reset", bool(shows("wb", "b")), screen("wb"))
    for win in ("wa", "wb"):
        keys(win, "C-g", "q")
    check("both windows quit", bool(wait(lambda: subprocess.run([*HARNESS, "list-sessions"], capture_output=True).returncode != 0, 10)))
finally:
    subprocess.run([*HARNESS, "kill-server"], capture_output=True)
    if core and core.poll() is None:
        try:
            call("core.shutdown")
        except Exception:
            core.terminate()
        try:
            core.wait(timeout=10)
        except subprocess.TimeoutExpired:
            core.kill()
    shutil.rmtree(R, ignore_errors=True)
print(f"passed {sum(results)} failed {len(results) - sum(results)}")
sys.exit(0 if results and all(results) else 1)
