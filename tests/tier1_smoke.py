#!/usr/bin/env python3
"""tier1 real-model smoke (opt-in, costs money: run only with approval). One runtime per run, through the real
tier1 launcher, as in tests/tier1_e2e.py but with real models.

  claude: dedicated auth (i0 pattern) in the pane's environment only: HOME=<auth>/home, CLAUDE_CONFIG_DIR=<auth>/config.
          One prompt "Reply with just: ok", then one settle message wakes the idle session (MANGO). Aborts if more
          than 4 model requests are seen. The auth config's settings.json must stay byte-identical.
  pi:     pi1's openai-codex auth bound READ-ONLY into the private Pi agent dir (never copied, never the real ~/.pi).
          A short-lived subprocess projects only its expiry; the run aborts unless it is more than 10 min ahead, so a
          refresh is never needed. One start message wakes the idle Pi (PAPAYA).

Run inside a private user/mount/PID namespace with network (the Pi bind needs the mount namespace):
  unshare -Urmpf --mount-proc --kill-child python3 tests/tier1_smoke.py --runtime claude|pi --target T --root R
      [--claude C --claude-auth DIR] [--pi PI_CLI_JS --pi-auth FILE]
"""

import sys
if sys.platform != "linux":
    print("SKIP tier1_smoke: requires Linux /proc and namespace/process probes")
    raise SystemExit(77)

from python_interpreter import python
from extension_layout import copy_packages
import argparse
import glob
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import signal
import socket
import subprocess
import sys
import time

ap = argparse.ArgumentParser()
ap.add_argument("--runtime", choices=["claude", "pi"], required=True)
ap.add_argument("--target", required=True)
ap.add_argument("--root", required=True)
ap.add_argument("--claude")
ap.add_argument("--claude-auth")
ap.add_argument("--pi")
ap.add_argument("--pi-auth")
a = ap.parse_args()
HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
BIN = {name: os.path.join(a.target, "debug", name) for name in ("vqro", "vqro-tmux", "vqro-claude-hook")}
root = os.path.abspath(a.root)
P = {k: os.path.join(root, k) for k in ("home", "bin", "ext", "state", "work", "xrun", "hookstate", "agent")}
for path in P.values():
    os.makedirs(path, mode=0o700, exist_ok=True)
SOCK, TSOCK = os.path.join(root, "v.sock"), os.path.join(root, "t.sock")
procs, receipt = [], {"runtime": a.runtime, "started": time.strftime("%Y-%m-%d %H:%M:%S %Z")}
START = time.time()
EXPIRY_PROJECTION = '''import json,sys
try:
    with open(sys.argv[1],encoding="utf-8") as stream:
        value=json.load(stream).get("openai-codex",{}).get("expires")
    print(value if type(value) is int and value>0 else "absent")
except Exception:
    print("absent")
'''


def save(status):
    receipt["status"] = status
    write_json(os.path.join(root, "receipt.json"), receipt, indent=1, default=str)
    print(json.dumps({"status": status}), flush=True)


def wait(predicate, timeout=30, step=0.3):
    end = time.time() + timeout
    while time.time() < end:
        value = predicate()
        if value:
            return value
        time.sleep(step)
    return None


def tmux(*args):
    return subprocess.run(["/usr/bin/tmux", "-S", TSOCK, "-f", "/dev/null", *args], capture_output=True, text=True, env=TENV)


def stop():
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


class Client:
    def __init__(self):
        self.s = socket.socket(socket.AF_UNIX)
        self.s.connect(SOCK)
        self.f, self.n = self.s.makefile("rw"), 0
        self.call("hello", {"client": "tier1-smoke", "identity": {"kind": "owner"}})

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

    def send(self, rid, mode, text):
        op = self.cmd("coord.inbox.window")["epoch"] + f":smoke:{time.time_ns()}"
        return self.cmd("coord.inbox.send", op=op, submission=op, to=rid, mode=mode, send_new=True,
                        content=[{"type": "text", "text": text}], render=["context"])["delivery"]


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest() if os.path.exists(path) else None


def screen(row):
    return tmux("capture-pane", "-p", "-J", "-S", "-200", "-t", row["backend_ref"]).stdout


# --- runtime-specific environment, before anything starts ----------------------------------------------------------
node_dir = os.path.dirname(shutil.which("node") or "/usr/bin/node")
PATH = f"{P['bin']}:{node_dir}:/usr/bin:/bin"
TENV = {"PATH": PATH, "XDG_RUNTIME_DIR": P["xrun"], "LANG": "C.UTF-8", "TERM": "xterm-256color",
        "VQRO_CLAUDE_STATE": P["hookstate"], "VQRO_PULL_LOG": os.path.join(root, "pi-plugin.jsonl")}
if a.runtime == "claude":
    os.symlink(os.path.abspath(a.claude), os.path.join(P["bin"], "claude"))
    auth = os.path.abspath(a.claude_auth)
    TENV.update(HOME=os.path.join(auth, "home"), CLAUDE_CONFIG_DIR=os.path.join(auth, "config"), DISABLE_TELEMETRY="1",
                DISABLE_ERROR_REPORTING="1", DISABLE_AUTOUPDATER="1")
    guarded = os.path.join(auth, "config", "settings.json")
    argv = ["claude", "--model", "haiku"]
else:
    with open(os.path.join(P["bin"], "pi"), "w") as f:
        f.write(f'#!/bin/sh\nexec node {os.path.abspath(a.pi)} "$@"\n')
    os.chmod(os.path.join(P["bin"], "pi"), 0o755)
    assert os.getuid() == 0 and os.getpid() == 1, "pi smoke requires a private user/mount/PID namespace"
    subprocess.run(["/usr/bin/mount", "--make-rprivate", "/"], check=True)
    target = os.path.join(P["agent"], "auth.json")
    write_text(target, "")
    os.chmod(target, 0o600)
    subprocess.run(["/usr/bin/mount", "--bind", os.path.abspath(a.pi_auth), target], check=True)
    subprocess.run(["/usr/bin/mount", "-o", "remount,bind,ro", target], check=True)
    receipt["credential_readonly"] = bool(os.statvfs(target).f_flag & os.ST_RDONLY)
    projection = subprocess.run([python(), "-I", "-c", EXPIRY_PROJECTION, target],
                                env={"HOME": P["home"], "PATH": "/usr/bin:/bin"}, capture_output=True, text=True, timeout=5)
    projected = projection.stdout.strip()
    expiry = int(projected) if projection.returncode == 0 and projected.isdecimal() else None
    receipt.update(expiry_ms=expiry, expiry_projection_source=EXPIRY_PROJECTION)
    if not receipt["credential_readonly"] or expiry is None or expiry <= time.time() * 1000 + 600000:
        save("blocked: credential not read-only, or a refresh would be needed (expiry absent or under 10 min); nothing started")
        sys.exit(3)
    # lastChangelogVersion: Pi records it itself on a first run (an existing user has it), not a vqro change.
    version = read_json(os.path.join(os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(a.pi)))), "package.json"))["version"]
    settings = {"retry": {"enabled": False, "maxRetries": 0, "provider": {"maxRetries": 0, "maxRetryDelayMs": 0, "timeoutMs": 60000}},
                "compaction": {"enabled": False}, "cacheWarming": "off", "transport": "sse", "lastChangelogVersion": version}
    write_json(os.path.join(P["agent"], "settings.json"), settings)
    TENV.update(HOME=P["home"], PI_CODING_AGENT_DIR=P["agent"], PI_SKIP_VERSION_CHECK="1", PI_TELEMETRY="0")
    guarded = os.path.join(P["agent"], "settings.json")
    # The real credential is never read here: only its metadata, before and after.
    real = os.stat(os.path.abspath(a.pi_auth))
    receipt["credential_metadata_before"] = [real.st_size, real.st_mtime_ns]
    argv = ["pi", "--provider", "openai-codex", "--model", "gpt-6-luna", "--thinking", "off", "--no-tools", "--no-session"]
before = sha(guarded)

# --- private tmux server, core with coord + tier1, tmux provider ---------------------------------------------------
tmux("new-session", "-d", "-s", "work", "-x", "140", "-y", "40", "-c", P["work"], "/bin/sh")
tmux("set-option", "-g", "remain-on-exit", "on")
for name in os.listdir(os.path.join(REPO, "extensions")):
    if name.endswith(".py"):
        shutil.copy(os.path.join(REPO, "extensions", name), P["ext"])
shutil.copytree(os.path.join(REPO, "extensions", "manifests"), os.path.join(P["ext"], "manifests"))
copy_packages(os.path.join(REPO,'extensions'),P['ext'])
os.makedirs(os.path.join(P["ext"], "tier1"))
shutil.copy(os.path.join(REPO, "integrations", "pi", "vqro-pull.ts"), os.path.join(P["ext"], "tier1"))
defaults = read_json(os.path.join(REPO, "extensions", "defaults.json"))
defaults["extensions"] = [e for e in defaults["extensions"] if e["name"] in ("coord", "tier1")]
write_json(os.path.join(P["ext"], "defaults.json"), defaults)
cenv = {"HOME": P["home"], "PATH": f"{os.path.dirname(BIN['vqro'])}:{PATH}", "XDG_RUNTIME_DIR": P["xrun"], "VQRO_STATE_DIR": P["state"],
        "VQRO_SOCKET": SOCK, "VQRO_EXTENSIONS_DIR": P["ext"], "HC_SOCKET": os.path.join(root, "h.sock"), "HC_SESSION": "tier1-smoke",
        "VQRO_HERDR": '["/bin/false"]'}
with open(os.path.join(root, "core.log"), "w") as log:  # the child keeps its own descriptor
    procs.append(subprocess.Popen([BIN["vqro"], "serve"], env=cenv, start_new_session=True, stdout=log, stderr=subprocess.STDOUT))
wait(lambda: os.path.exists(SOCK), 20)
c = Client()
c.call("workspace.apply", {"ops": [
    {"op": "set", "key": "extension/tmux", "value": {"argv": [BIN["vqro-tmux"], "--tmux", "/usr/bin/tmux", "--tmux-socket", TSOCK, "--name", "tmux"]}},
    {"op": "set", "key": "doc/settings/tier1", "value": {a.runtime: argv, "hook": BIN["vqro-claude-hook"]}}]})
wait(lambda: {"tier1." + a.runtime, "coord.grants"} <= {x.get("name") for x in c.call("command.list")["commands"]}, 60)
wait(lambda: (c.call("workspace.snapshot").get("providers", {}).get("tmux") or {}).get("synced"), 40)
wait(lambda: c.cmd("coord.status").get("state") == "ready", 30)

reply = c.cmd("tier1." + a.runtime, cwd=P["work"])
rid = reply["resource"]
row = next(r for r in c.call("resource.list", {"id": rid})["resources"])
receipt.update(resource=rid, principal_recorded=row.get("principal") == reply["principal"])

if a.runtime == "claude":
    def transcript():
        files = [f for f in glob.glob(os.path.join(TENV["CLAUDE_CONFIG_DIR"], "projects", "*", "*.jsonl")) if os.path.getmtime(f) >= START]
        return [json.loads(line) for f in files for line in read_lines(f) if line.strip()]

    def requests():
        """One row per model response (a response can span several transcript entries with the same message id)."""
        seen = {}
        for r in transcript():
            m = r.get("message") or {}
            if r.get("type") == "assistant" and m.get("model") not in (None, "<synthetic>"):
                seen[m.get("id") or r.get("uuid")] = r
        return list(seen.values())
    time.sleep(6)
    # A first interactive start in the dedicated config: onboarding and the private work dir's trust dialog.
    for _ in range(8):
        text = re.sub(r"\s+", "", screen(row))
        if "Itrustthisfolder" in text:  # "No, exit" is preselected: move to "Yes, I trust this folder"
            tmux("send-keys", "-t", row["backend_ref"], "Down"); time.sleep(0.5)
            tmux("send-keys", "-t", row["backend_ref"], "Enter"); time.sleep(4)
        elif "PressEntertocontinue" in text or "Choosethetextstyle" in text or "Entertoconfirm" in text:
            tmux("send-keys", "-t", row["backend_ref"], "Enter"); time.sleep(4)
        else:
            break
    tmux("send-keys", "-t", row["backend_ref"], "-l", "Reply with just: ok"); time.sleep(0.5)
    tmux("send-keys", "-t", row["backend_ref"], "Enter")
    log = os.path.join(P["hookstate"], "hook.log")
    armed = wait(lambda: os.path.exists(log) and "waiter_armed" in read_text(log), 120)
    if not armed:
        receipt.update(screen_tail=screen(row)[-2500:], model_requests=len(requests()))
        stop()
        save("FAIL: the first prompt never completed (no settle message sent)")
        sys.exit(1)
    delivery = c.send(rid, "settle", "Teammate note for the vqro tier1 smoke test: reply with just the word MANGO.")
    woke = wait(lambda: len(requests()) > 4 or any("MANGO" in json.dumps((r.get("message") or {}).get("content")) for r in requests()), 120)
    time.sleep(4)
    rows = requests()
    usage = {k: sum(((r.get("message") or {}).get("usage") or {}).get(k, 0) or 0 for r in rows)
             for k in ("input_tokens", "output_tokens", "cache_read_input_tokens", "cache_creation_input_tokens")}
    record = c.cmd("coord.inbox.get", delivery=delivery)
    receipt.update(armed=bool(armed), model_requests=len(rows), models=sorted({(r["message"] or {}).get("model") for r in rows}),
                   usage=usage, stage=record.get("state"), answered=any("MANGO" in json.dumps((r.get("message") or {}).get("content")) for r in rows))
    ok = armed and receipt["answered"] and len(rows) <= 4 and record.get("state") in ("supplied", "read")
    # Transcripts never stay in the shared auth config.
    for f in glob.glob(os.path.join(TENV["CLAUDE_CONFIG_DIR"], "projects", "*t1smoke*")):
        shutil.move(f, os.path.join(root, "transcripts-" + os.path.basename(f)[-32:]))
else:
    started = wait(lambda: os.path.exists(TENV["VQRO_PULL_LOG"]) and '"reconciled"' in read_text(TENV["VQRO_PULL_LOG"]), 60)
    delivery = c.send(rid, "start", "Teammate note for the vqro tier1 smoke test: reply with just the word PAPAYA.")
    answered = wait(lambda: c.cmd("coord.inbox.get", delivery=delivery).get("state") == "read"
                    and "PAPAYA" in screen(row).split("PAPAYA.", 1)[-1], 120)
    time.sleep(3)
    record = c.cmd("coord.inbox.get", delivery=delivery)
    tail = screen(row)[-1500:]
    receipt.update(plugin_started=bool(started), answered=bool(answered), stage=record.get("state"),
                   screen_footer=[line for line in tail.splitlines() if re.search(r"[↑↓$]|\d+(\.\d+)?k?\s*tok", line)][-3:])
    ok = started and answered and record.get("state") in ("inserted", "read")
receipt["screen_tail"] = screen(row)[-2500:]
receipt["config_unchanged"] = sha(guarded) == before
if a.runtime == "pi":
    real = os.stat(os.path.abspath(a.pi_auth))
    receipt["credential_unchanged"] = receipt["credential_metadata_before"] == [real.st_size, real.st_mtime_ns]
    receipt["config_unchanged"] = receipt["config_unchanged"] and receipt["credential_unchanged"]
ok = ok and receipt["config_unchanged"] and receipt["principal_recorded"]
receipt["seconds"] = round(time.time() - START, 1)
c.close()
stop()
save("PASS" if ok else "FAIL")
sys.exit(0 if ok else 1)
