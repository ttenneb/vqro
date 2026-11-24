#!/usr/bin/env python3
"""tier1 launcher end to end: the real launcher on a real terminal backend, real Claude Code and Pi, fake models.

A private vqro core loads coord (K2) and tier1, plus vqro-tmux on a private tmux server. As the owner it invokes
`tier1.claude` and `tier1.pi`. Each runtime runs in a real tmux pane, started by tier1_run.py with its plugin wired
for that launch only. It checks:
- the terminal's recorded principal is the fresh one the plugin uses, and the declaration and grant exist;
- a boundary message reaches Claude Code at its next turn point, and a start message wakes the idle Pi;
- the private HOME's ~/.claude and ~/.pi configuration files are byte-identical afterwards.

Usage (opt-in; needs the runtimes, never real models, no network): run inside a private network namespace:
  unshare -rn sh -c 'ip link set lo up && exec python3 tests/tier1_e2e.py --target T --claude C --pi PI_CLI_JS --root R'
"""

import sys
if sys.platform != "linux":
    print("SKIP tier1_e2e: requires Linux /proc and namespace/process probes")
    raise SystemExit(77)

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
from extension_layout import copy_packages
import signal
import socket
import subprocess
import sys
import time

ap = argparse.ArgumentParser()
ap.add_argument("--target", required=True, help="cargo target dir with debug/{vqro,vqro-tmux,vqro-claude-hook}")
ap.add_argument("--claude", required=True)
ap.add_argument("--pi", required=True, help="Pi's cli.js")
ap.add_argument("--root", required=True)
ap.add_argument("--only", choices=["claude", "pi"])
a = ap.parse_args()
HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
BIN = {name: os.path.join(a.target, "debug", name) for name in ("vqro", "vqro-tmux", "vqro-claude-hook")}
root = os.path.abspath(a.root)
P = {k: os.path.join(root, k) for k in ("home", "bin", "ext", "state", "work", "xrun", "hookstate")}
for path in P.values():
    os.makedirs(path, mode=0o700, exist_ok=True)
SOCK, TSOCK = os.path.join(root, "v.sock"), os.path.join(root, "t.sock")
ANTHROPIC, OPENAI = 18850, 18851
procs, fails, results = [], [], {}


def check(name, ok, detail=""):
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"  ({str(detail)[:600]})"), flush=True)
    results[name] = {"ok": bool(ok), "detail": None if ok else str(detail)[:2000]}
    if not ok:
        fails.append(name)


def wait(predicate, timeout=30, step=0.2):
    end = time.time() + timeout
    while time.time() < end:
        value = predicate()
        if value:
            return value
        time.sleep(step)
    return None


def stop():
    # The runtimes in the panes (and their children) go with the server; kill their groups so none lingers.
    panes = subprocess.run(["/usr/bin/tmux", "-S", TSOCK, "list-panes", "-a", "-F", "#{pane_pid}"], capture_output=True, text=True).stdout
    subprocess.run(["/usr/bin/tmux", "-S", TSOCK, "kill-server"], capture_output=True)
    for pid in panes.split():
        try:
            os.killpg(int(pid), signal.SIGKILL)
        except (ProcessLookupError, PermissionError, ValueError):
            pass
    for p in procs:
        try:
            os.killpg(p.pid, signal.SIGKILL)
        except (ProcessLookupError, PermissionError):
            pass
        p.wait()


import atexit  # noqa: E402
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from textfiles import read_json, read_lines, read_text, write_json, write_text  # noqa: E402
atexit.register(stop)
signal.signal(signal.SIGTERM, lambda *_: sys.exit(1))


class Client:
    def __init__(self):
        self.s = socket.socket(socket.AF_UNIX)
        self.s.connect(SOCK)
        self.f, self.n = self.s.makefile("rw"), 0
        self.call("hello", {"client": "tier1-e2e", "identity": {"kind": "owner"}})

    def call(self, method, params=None):
        self.n += 1
        self.f.write(json.dumps({"jsonrpc": "2.0", "id": self.n, "method": method, "params": params or {}}) + "\n")
        self.f.flush()
        while True:
            row = json.loads(self.f.readline())
            if row.get("id") == self.n:
                if "error" in row:
                    raise RuntimeError(row["error"])
                return row["result"]

    def close(self):
        self.f.close()
        self.s.close()

    def cmd(self, name, **args):
        return self.call("command.invoke", {"name": name, "args": args})

    def op(self):
        self.n += 1
        return self.cmd("coord.inbox.window")["epoch"] + f":e2e:{self.n}"


def digest(paths):
    return {p: hashlib.sha256(Path(p).read_bytes()).hexdigest() if os.path.exists(p) else None for p in paths}


def tmux(*args):
    return subprocess.run(["/usr/bin/tmux", "-S", TSOCK, "-f", "/dev/null", *args], capture_output=True, text=True,
                          env=TENV)


def model_requests(log):
    return [json.loads(line) for line in read_lines(log)] if os.path.exists(log) else []


def seen(log, token):
    return any(token in json.dumps(r) for r in model_requests(log))


# --- the human's own configuration in the private HOME (must stay byte-identical) ----------------------------------
home = P["home"]
claude_dir, pi_dir = os.path.join(home, ".claude"), os.path.join(home, ".pi", "agent")
os.makedirs(claude_dir, exist_ok=True)
os.makedirs(pi_dir, exist_ok=True)
key = "sk-ant-fake-key-for-tier1-tests-0123456789"
write_json(os.path.join(claude_dir, "settings.json"), {"permissions": {"allow": ["Bash"]}, "note": "the human's own settings; vqro must not add hooks here"})
write_json(os.path.join(home, ".claude.json"), {"hasCompletedOnboarding": True, "theme": "dark", "numStartups": 5,
           "customApiKeyResponses": {"approved": [key[-20:]], "rejected": []},
           "projects": {P["work"]: {"hasTrustDialogAccepted": True, "hasCompletedProjectOnboarding": True}}})  # Claude Code's state file; it writes it itself (not guarded)
write_json(os.path.join(pi_dir, "models.json"), {"providers": {"fake": {"baseUrl": f"http://127.0.0.1:{OPENAI}/v1", "api": "openai-completions", "apiKey": "fake",
                                  "models": [{"id": "fake-1", "input": ["text"]}]}}})
# An existing Pi user's settings (Pi records lastChangelogVersion itself on a first run; a real user has it already).
pi_version = subprocess.run(["node", a.pi, "--version"], capture_output=True, text=True).stdout.strip().split()[-1]
write_json(os.path.join(pi_dir, "settings.json"), {"defaultProvider": "fake", "defaultModel": "fake-1", "retry": {"enabled": False}, "tuiMode": "regular",
           "lastChangelogVersion": pi_version}, indent=2)
GUARDED = [os.path.join(claude_dir, "settings.json"), os.path.join(pi_dir, "settings.json"), os.path.join(pi_dir, "models.json")]
before = digest(GUARDED)

# --- runtimes on PATH, fake models ---------------------------------------------------------------------------------
os.symlink(os.path.abspath(a.claude), os.path.join(P["bin"], "claude"))
with open(os.path.join(P["bin"], "pi"), "w") as f:
    f.write(f'#!/bin/sh\nexec node {os.path.abspath(a.pi)} "$@"\n')
os.chmod(os.path.join(P["bin"], "pi"), 0o755)
node_dir = os.path.dirname(shutil.which("node") or "/usr/bin/node")
PATH = f"{P['bin']}:{node_dir}:/usr/bin:/bin"
base = {"HOME": home, "PATH": PATH, "XDG_RUNTIME_DIR": P["xrun"], "LANG": "C.UTF-8"}
claude_log, pi_log = os.path.join(root, "anthropic.jsonl"), os.path.join(root, "openai.jsonl")
for argv in ([sys.executable, os.path.join(HERE, "fixtures", "fake_anthropic.py"), "--port", str(ANTHROPIC), "--log", claude_log],
             [sys.executable, os.path.join(HERE, "fixtures", "fake_openai.py"), "--port", str(OPENAI), "--log", pi_log]):
    procs.append(subprocess.Popen(argv, env=base, start_new_session=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL))

# --- private tmux server: the runtimes' environment (fake endpoints, no telemetry, private hook state) -------------
TENV = dict(base, TERM="xterm-256color", ANTHROPIC_BASE_URL=f"http://127.0.0.1:{ANTHROPIC}", ANTHROPIC_API_KEY=key,
            DISABLE_TELEMETRY="1", DISABLE_ERROR_REPORTING="1", CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC="1",
            DISABLE_AUTOUPDATER="1", VQRO_CLAUDE_STATE=P["hookstate"], PI_OFFLINE="1", PI_SKIP_VERSION_CHECK="1",
            PI_TELEMETRY="0", VQRO_PULL_LOG=os.path.join(root, "pi-plugin.jsonl"))
out = tmux("new-session", "-d", "-s", "work", "-x", "140", "-y", "40", "-c", P["work"], "/bin/sh")
check("private tmux server started", out.returncode == 0, out.stderr)
tmux("set-option", "-g", "remain-on-exit", "on")  # a runtime that exits stays readable for the checks

# --- private core: coord (K2) + tier1, then the tmux provider -------------------------------------------------------
for name in os.listdir(os.path.join(REPO, "extensions")):
    if name.endswith(".py"):
        shutil.copy(os.path.join(REPO, "extensions", name), P["ext"])
shutil.copytree(os.path.join(REPO, "extensions", "manifests"), os.path.join(P["ext"], "manifests"))
copy_packages(os.path.join(REPO,'extensions'),P['ext'])
os.makedirs(os.path.join(P["ext"], "tier1"))
shutil.copy(os.path.join(REPO, "integrations", "pi", "vqro-pull.ts"), os.path.join(P["ext"], "tier1"))  # as dev-install does
defaults = read_json(os.path.join(REPO, "extensions", "defaults.json"))
defaults["extensions"] = [e for e in defaults["extensions"] if e["name"] in ("coord", "tier1")]
write_json(os.path.join(P["ext"], "defaults.json"), defaults)
cenv = dict(base, VQRO_STATE_DIR=P["state"], VQRO_SOCKET=SOCK, VQRO_EXTENSIONS_DIR=P["ext"], HC_SOCKET=os.path.join(root, "h.sock"),
            HC_SESSION="tier1-e2e", VQRO_HERDR='["/bin/false"]', PATH=f"{os.path.dirname(BIN['vqro'])}:{PATH}")
with open(os.path.join(root, "core.log"), "w") as log:  # the child keeps its own descriptor
    procs.append(subprocess.Popen([BIN["vqro"], "serve"], env=cenv, start_new_session=True, stdout=log, stderr=subprocess.STDOUT))
check("core socket", bool(wait(lambda: os.path.exists(SOCK), 20)))
c = Client()
c.call("workspace.apply", {"ops": [
    {"op": "set", "key": "extension/tmux", "value": {"argv": [BIN["vqro-tmux"], "--tmux", "/usr/bin/tmux", "--tmux-socket", TSOCK, "--name", "tmux"]}},
    {"op": "set", "key": "doc/settings/tier1", "value": {"claude": ["claude", "--model", "claude-sonnet-4-5", "--allowedTools", "Bash"],
                                                         "pi": ["pi", "--no-skills", "--no-context-files", "--tools", "bash"],
                                                         "hook": BIN["vqro-claude-hook"]}}]})
ready = wait(lambda: (lambda cmds: {"tier1.claude", "tier1.pi", "coord.grants", "coord.scope"} <= cmds)(
    {x.get("name") for x in c.call("command.list")["commands"]}), 60)
check("tier1 and coord commands registered", bool(ready))
check("tmux provider connected", bool(wait(lambda: (c.call("workspace.snapshot").get("providers", {}).get("tmux") or {}).get("synced"), 40)))
wait(lambda: c.cmd("coord.status").get("state") == "ready", 30)


def launched(runtime):
    reply = c.cmd("tier1." + runtime, cwd=P["work"])
    rid = reply["resource"]
    row = next((r for r in c.call("resource.list", {"id": rid})["resources"]), {})
    declared = c.call("workspace.get", {"key": "doc/agents/terminals/" + rid})
    granted = c.call("workspace.get", {"key": "doc/agents/grants/tier1-" + rid})
    routed = c.call("workspace.get", {"key": "doc/agents/request-routes/" + rid})
    check(f"{runtime}: request route recorded as human (owner-started)", bool(routed) and routed["value"] == {"route": "human"}, routed)
    check(f"{runtime}: launched in a terminal with its fresh principal recorded", row.get("kind") == "terminal"
          and row.get("principal") == reply["principal"] and reply["principal"].startswith("p:"), {"reply": reply, "row": row})
    check(f"{runtime}: declaration (plugin's own) and receive grant recorded",
          bool(declared) and declared["value"]["app"] == runtime and granted and granted["value"]["who"] == reply["principal"],
          {"declared": declared, "granted": granted})
    return rid, row


def pane(row):
    return row.get("backend_ref")


def send(rid, mode, text):
    op = c.op()
    return c.cmd("coord.inbox.send", op=op, submission=op, to=rid, mode=mode, send_new=True,
                 content=[{"type": "text", "text": text}], render=["context"])["delivery"]


if a.only in (None, "claude"):
    rid, row = launched("claude")
    hooked = wait(lambda: os.path.exists(os.path.join(P["hookstate"], "hook.log"))
                  and '"session"' in read_text(os.path.join(P["hookstate"], "hook.log")), 40)
    panes = tmux("list-panes", "-a", "-F", "#{pane_id} #{pane_current_command} #{pane_dead}").stdout
    check("claude: its hooks run (SessionStart via the per-launch --settings file)", bool(hooked),
          {"ref": pane(row), "panes": panes, "screen": "".join(tmux("capture-pane", "-p", "-t", p.split()[0]).stdout[-700:]
                                                              for p in panes.splitlines())})
    settings_files = [n for n in os.listdir(os.path.join(P["state"], "tier1")) if n.startswith("claude-")]
    check("claude: the per-launch settings file lives in the extension's private state", len(settings_files) == 1, settings_files)
    time.sleep(2)
    delivery = send(rid, "boundary", "TIER1-CLAUDE-BOUNDARY")
    tmux("send-keys", "-t", pane(row), "-l", "steps:1 sleep:1 first task")
    time.sleep(0.5)
    tmux("send-keys", "-t", pane(row), "Enter")
    got = wait(lambda: seen(claude_log, "TIER1-CLAUDE-BOUNDARY"), 60)
    record = c.cmd("coord.inbox.get", delivery=delivery)
    check("claude: a boundary message reaches the model at the next turn point via the plugin",
          bool(got) and record.get("state") in ("supplied", "read"), record.get("state"))
    log = read_text(os.path.join(P["hookstate"], "hook.log")) if hooked else ""
    check("claude: the plugin runs as this terminal's agent", f'"session"' in log and "taken" in log, log[-400:])

if a.only in (None, "pi"):
    rid, row = launched("pi")
    started = wait(lambda: os.path.exists(TENV["VQRO_PULL_LOG"]) and '"reconciled"' in read_text(TENV["VQRO_PULL_LOG"]), 60)
    check("pi: its plugin loaded via -e and reconciled as this terminal's consumer", bool(started))
    delivery = send(rid, "start", "TIER1-PI-START")
    got = wait(lambda: seen(pi_log, "TIER1-PI-START"), 60)
    record = c.cmd("coord.inbox.get", delivery=delivery)
    check("pi: a start message wakes the idle Pi and reaches the model", bool(got) and record.get("state") in ("inserted", "read"),
          record.get("state"))

after = digest(GUARDED)
check("the human's ~/.claude and ~/.pi configuration files are byte-identical", before == after,
      {k: (before[k], after[k]) for k in before if before[k] != after[k]})
check("no vqro hook or plugin was written into ~/.claude or ~/.pi",
      "vqro-claude-hook" not in read_text(GUARDED[0]) and "vqro-pull" not in read_text(GUARDED[1]))
write_json(os.path.join(root, "results.json"), results, indent=1)
c.close()
stop()
print(f"{'FAILED' if fails else 'OK'}: {len(fails)} failures; root {root}")
sys.exit(1 if fails else 0)
