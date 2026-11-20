#!/usr/bin/python3
"""Opt-in TUI check: the sidebar is one tree whose levels can be hidden; its options are also in Settings.

The real `vqro tui` runs in an outer tmux harness against a private `vqro serve --fake` with the tabs,
sidebar, workspaces and settings extensions. A private git repo has four worktrees (dirty, detached, ahead,
clean); its workspace has two tabs, a terminal working in the repo in no tab, and one terminal elsewhere.
Checks three configurations and the Settings page, and saves their screens:
- everything: workspace > tabs (○/●) > panes, "Not in a tab", all four worktrees, "Other terminals";
- panes under workspaces, at most 2 worktrees: no tab rows, panes and the loose terminal under the workspace,
  the first two worktrees and "+2 more worktrees";
- terminals only: a flat list (no workspace row);
- Settings > Sidebar shows the same switches, checked as in the Show popup.

Usage: tests/e2e_tui_sidebar_tree.py TARGET_DIR [CAPTURE_DIR]   (debug binaries in TARGET_DIR/debug)
Private /tmp root; nothing live is touched. Stops only what it started.
"""
import json, os, shutil, socket, subprocess, sys, tempfile, time

TARGET = sys.argv[1]
CAPTURES = sys.argv[2] if len(sys.argv) > 2 else None
VQRO = os.path.join(TARGET, "debug", "vqro")
REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
R = tempfile.mkdtemp(prefix="pst", dir="/tmp")
for d in ("home", "st", "xst", "run", "cfg", "ext", "bin", "out", "work"):
    os.makedirs(os.path.join(R, d))
os.chmod(os.path.join(R, "run"), 0o700)
for f in os.listdir(os.path.join(REPO, "extensions")):
    if f.endswith(".py"):
        shutil.copy(os.path.join(REPO, "extensions", f), os.path.join(R, "ext", f))
shutil.copytree(os.path.join(REPO, "extensions", "manifests"), os.path.join(R, "ext", "manifests"))
with open(os.path.join(R, "ext", "defaults.json"), "w") as f:
    json.dump({"version": 2, "extensions": []}, f)  # only what this test enables below
for tool in ("python3", "git"):
    os.symlink(shutil.which(tool, path="/usr/bin:/bin"), os.path.join(R, "bin", tool))
ENV = {"HOME": f"{R}/home", "PATH": f"{R}/bin", "TERM": "xterm-256color", "SHELL": "/bin/sh", "VQRO_STATE_DIR": f"{R}/st",
       "VQRO_SOCKET": f"{R}/p.sock", "VQRO_EXTENSIONS_DIR": f"{R}/ext", "XDG_STATE_HOME": f"{R}/xst",
       "XDG_RUNTIME_DIR": f"{R}/run", "XDG_CONFIG_HOME": f"{R}/cfg", "HC_SESSION": "tree", "HC_SOCKET": f"{R}/hc.sock",
       "VQRO_HERDR": '["/bin/false"]', "LANG": "C.UTF-8", "GIT_CONFIG_NOSYSTEM": "1"}
HARNESS = ["/usr/bin/tmux", "-S", f"{R}/harness.sock", "-f", "/dev/null"]
results = []
REPO_DIR = f"{R}/work/proj"


def check(name, ok, detail=""):
    results.append(ok)
    print(("PASS " if ok else "FAIL ") + name + (f"\n{detail}" if detail and not ok else ""), flush=True)


def call(method, params=None):
    s = socket.socket(socket.AF_UNIX)
    s.connect(ENV["VQRO_SOCKET"])
    f = s.makefile("rwb", buffering=0)
    for i, (m, p) in enumerate([("hello", {"client": "e2e-tree"}), (method, params or {})]):
        f.write((json.dumps({"jsonrpc": "2.0", "id": i, "method": m, "params": p}) + "\n").encode())
        while True:
            msg = json.loads(f.readline())
            if msg.get("id") == i:
                break
    s.close()
    if "error" in msg:
        raise RuntimeError(f"{method}: {msg['error']}")
    return msg["result"]


def invoke(name, args=None):
    return call("command.invoke", {"name": name, "args": args or {}})


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


def sidebar_text():
    """The sidebar column only (left of the first inner border)."""
    return "\n".join(line[:40] for line in screen().splitlines()[:-1])  # not the status bar


def keys(*k, literal=False):
    subprocess.run([*HARNESS, "send-keys", *(["-l"] if literal else []), *k])


def save(name):
    text = screen()
    with open(f"{R}/out/{name}.txt", "w") as f:
        f.write(text)
    return text


def git(*args, cwd=REPO_DIR):
    subprocess.run(["/usr/bin/git", "-c", "user.name=e2e", "-c", "user.email=e2e@localhost", *args], cwd=cwd,
                   env={"PATH": "/usr/bin:/bin", "HOME": f"{R}/home", "GIT_CONFIG_NOSYSTEM": "1"}, check=True, capture_output=True)


def setup_repo():
    os.makedirs(f"{REPO_DIR}/src")
    git("init", "-q", "-b", "main")
    open(f"{REPO_DIR}/README", "w").write("x\n")
    git("add", "README"); git("commit", "-q", "-m", "init")
    for name, args in (("wt-fix", ["-b", "fix"]), ("wt-detached", ["--detach"]), ("wt-ahead", ["-b", "feat"]), ("wt-clean", ["-b", "old"])):
        git("worktree", "add", "-q", *args, f"{R}/work/{name}")
    open(f"{R}/work/wt-fix/dirty.txt", "w").write("change\n")
    git("commit", "-q", "--allow-empty", "-m", "ahead", cwd=f"{R}/work/wt-ahead")


core = None
try:
    setup_repo()
    core = subprocess.Popen([VQRO, "serve", "--fake"], env=ENV, stdout=open(f"{R}/serve.log", "w"), stderr=subprocess.STDOUT)
    wait(lambda: os.path.exists(ENV["VQRO_SOCKET"]), 10)
    call("workspace.apply", {"note": "e2e extensions", "ops": [
        {"op": "set", "key": f"extension/{name}", "value": {"enabled": True, "argv": [f"{R}/bin/python3", f"{R}/ext/{name}.py"]}}
        for name in ("tabs", "sidebar", "workspaces", "settings")]})
    ready = wait(lambda: {"workspaces.add", "sidebar.preset", "settings.open", "tabs.open"} <= {c["name"] for c in call("command.list")["commands"] if c.get("available")}, 20)
    check("extensions running", bool(ready))
    invoke("workspaces.add", {"text": REPO_DIR})
    data = wait(lambda: (call("workspace.snapshot")["live"].get("workspaces") or {}).get("data"))
    wts = {os.path.basename(t["path"]): t for t in data[0].get("worktrees", [])} if data else {}
    check("workspace with 4 worktrees, ahead/dirty known", set(wts) == {"wt-fix", "wt-detached", "wt-ahead", "wt-clean"}
          and wts["wt-ahead"].get("ahead") == 1 and wts["wt-fix"].get("dirty"), json.dumps(data))
    invoke("workspaces.open", {"path": REPO_DIR})
    invoke("tabs.open", {"cwd": REPO_DIR, "name": "build"})
    call("resource.spawn", {"argv": ["/bin/sh"], "cwd": f"{REPO_DIR}/src", "name": "loose"})   # in the repo, in no tab
    call("resource.spawn", {"argv": ["/bin/sh"], "cwd": f"{R}/home", "name": "elsewhere"})    # no folder, no tab
    invoke("sidebar.toggle")
    with open(f"{R}/tui.sh", "w") as f:
        f.write("".join(f"export {k}='{v}'\n" for k, v in ENV.items()) + f"exec {VQRO} tui\n")
    subprocess.run([*HARNESS, "new-session", "-d", "-x", "120", "-y", "40", f"/bin/sh {R}/tui.sh"], env={"PATH": "/usr/bin:/bin", "HOME": f"{R}/home"})

    # 1. Everything: one tree.
    full = wait(lambda: all(t in sidebar_text() for t in ("proj", "build", "Not in a tab", "loose", "wt-clean", "Other terminals", "elsewhere")))
    text = save("1-everything")
    check("everything: workspace > tabs > panes, Not in a tab, worktrees, other terminals", bool(full), text)
    check("everything: tab rows marked ○/●", "● build" in text and "○" in sidebar_text(), text)

    # 2. Panes under workspaces, at most 2 worktrees.
    invoke("sidebar.preset", {"preset": "panes under workspaces"})
    invoke("sidebar.worktrees", {"key": "limit", "value": 2})
    two = wait(lambda: "+2 more worktrees" in sidebar_text() and "Not in a tab" not in sidebar_text() and "● build" not in sidebar_text())
    text = save("2-panes-under-workspaces")
    # The first two in git's order are kept: wt-ahead, wt-clean.
    check("tabs hidden: panes move up, worktrees limited to 2 (+2 more)", bool(two) and "loose" in text and "wt-ahead" in text and "wt-fix" not in text, text)
    keys("C-g", ","); wait(lambda: "Settings" in screen() and "Extensions" in screen(), 10)
    for _ in range(8):
        if "Per workspace" in screen():
            break
        keys("]"); time.sleep(0.4)
    page = wait(lambda: "Per workspace" in screen() and "Hide detached" in screen(), 5)
    text = save("settings-sidebar-page")
    check("Settings has a Sidebar page with the same options", bool(page) and "at most 2" in text, text)
    keys("Escape"); time.sleep(0.4)

    # 3. Terminals only: flat.
    invoke("sidebar.preset", {"preset": "terminals only"})
    flat = wait(lambda: "elsewhere" in sidebar_text() and "proj " not in sidebar_text() and "wt-fix" not in sidebar_text() and "loose" in sidebar_text())
    text = save("3-terminals-only")
    check("terminals only: a flat list, no workspace or worktree rows", bool(flat), text)
    snap = call("workspace.snapshot")
    prefs = snap["entries"]["doc/settings/sidebar"]["value"]
    popup = snap["live"]["sidebar.view"]["items"]
    page = snap["live"]["settings.page.sidebar"]["items"]
    check("one state: Settings page == Show popup options", page == popup[:len(page)] and prefs["worktrees"]["limit"] == 2, json.dumps(prefs))

    keys("C-g"); time.sleep(0.4); keys("q")
    check("TUI quits", bool(wait(lambda: subprocess.run([*HARNESS, "list-sessions"], capture_output=True).returncode != 0, 10)))
finally:
    if CAPTURES:
        os.makedirs(CAPTURES, exist_ok=True)
        for name in os.listdir(f"{R}/out"):
            shutil.copy(f"{R}/out/{name}", os.path.join(CAPTURES, name))
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
