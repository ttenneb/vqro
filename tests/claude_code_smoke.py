"""A3 real-model smoke (opt-in, costs money: run only with the coordinator's per-run approval and log it).

Real Claude Code against the real API through a signed-in, dedicated config dir (never copied or printed), with a
private vqro core and the fake inbox. Two runs on the cheapest model:
  1. `-p`: a boundary message sent while the first Bash call runs must be in the conversation before the second
     request (transcript `hook_additional_context`, then the `api-request-blob` carrying it) and reach `read`;
  2. TUI: after a first turn, a `start` message wakes the idle session through the asyncRewake waiter.

  python3 tests/claude_code_smoke.py --vqro target/debug/vqro --hook target/debug/vqro-claude-hook \
      --claude <claude> --auth <agent-runtimes/auth/claude> --root ROOT [--model haiku]
"""
import argparse, fcntl, glob, json, os, pty, re, select, shutil, socket, struct, subprocess, sys, termios, time

ap = argparse.ArgumentParser()
for k in ("vqro", "hook", "claude", "auth", "root"):
    ap.add_argument("--" + k, required=True)
ap.add_argument("--model", default="haiku")
ap.add_argument("--only", choices=["p", "tui"])
a = ap.parse_args()
HERE = os.path.dirname(os.path.abspath(__file__))
root = a.root
P = {k: os.path.join(root, k) for k in ("work", "xrun", "st", "noext", "hookstate")}
for d in P.values():
    os.makedirs(d, exist_ok=True)
sock, inbox_log = os.path.join(root, "v.sock"), os.path.join(root, "inbox.jsonl")
base = {"HOME": os.path.join(root, "home-core"), "PATH": "/usr/bin:/bin", "XDG_RUNTIME_DIR": P["xrun"],
        "VQRO_STATE_DIR": P["st"], "VQRO_SOCKET": sock, "VQRO_EXTENSIONS_DIR": P["noext"],
        "VQRO_HERDR": '["/bin/false"]', "HC_SOCKET": os.path.join(root, "h.sock"), "HC_SESSION": "a3-smoke"}
os.makedirs(base["HOME"], exist_ok=True)
procs, results, fails = [], {"model": a.model}, []
START = time.time()


def check(name, ok, detail=""):
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"  ({str(detail)[:400]})"), flush=True)
    results[name] = {"ok": bool(ok)}
    if not ok:
        fails.append(name)


def spawn(argv, **kw):
    p = subprocess.Popen(argv, **kw)
    procs.append(p)
    return p


import atexit  # noqa: E402
atexit.register(lambda: [p.kill() for p in procs if p.poll() is None])
spawn([a.vqro, "serve", "--fake"], env=base, stdout=open(os.path.join(root, "core.log"), "w"), stderr=subprocess.STDOUT)
while not os.path.exists(sock):
    time.sleep(0.05)
spawn([sys.executable, os.path.join(HERE, "fixtures", "fake_inbox.py"), inbox_log],
      env=dict(base, VQRO_EXT_PATH=os.path.join(HERE, "..", "extensions")), stderr=open(os.path.join(root, "inbox.err"), "w"))


class Client:
    def __init__(self):
        self.s = socket.socket(socket.AF_UNIX); self.s.connect(sock); self.f = self.s.makefile("rw"); self.n = 0
        self.call("hello", {"client": "a3-smoke"})

    def call(self, method, params):
        self.n += 1
        self.f.write(json.dumps({"jsonrpc": "2.0", "id": self.n, "method": method, "params": params}) + "\n"); self.f.flush()
        while True:
            row = json.loads(self.f.readline())
            if row.get("id") == self.n:
                if "error" in row:
                    raise RuntimeError(row["error"])
                return row["result"]

    def cmd(self, name, **args):
        return self.call("command.invoke", {"name": name, "args": args})


c = Client()
while not any(x.get("name") == "inbox.take" for x in c.call("command.list", {})["commands"]):
    time.sleep(0.1)
settings_file = os.path.join(root, "hooks-settings.json")
open(settings_file, "w").write(subprocess.run([a.hook, "settings", os.path.abspath(a.hook)], capture_output=True, text=True).stdout)
cenv = {"HOME": os.path.join(a.auth, "home"), "PATH": "/usr/bin:/bin", "CLAUDE_CONFIG_DIR": os.path.join(a.auth, "config"),
        "XDG_RUNTIME_DIR": P["xrun"], "DISABLE_AUTOUPDATER": "1", "DISABLE_UPDATES": "1", "TERM": "xterm-256color",
        "VQRO_SOCKET": sock, "VQRO_AGENT": "r7", "VQRO_CLAUDE_STATE": P["hookstate"]}


def txt(s):
    return [{"type": "text", "text": s}]


def get(d):
    return c.cmd("inbox.get", delivery=d)


def hooklog():
    path = os.path.join(P["hookstate"], "hook.log")
    return [json.loads(l) for l in open(path)] if os.path.exists(path) else []


def statuses():
    return [json.loads(l) for l in open(inbox_log) if '"event": "status"' in l] if os.path.exists(inbox_log) else []


def transcript_rows():
    # newest transcript written by this run (the dedicated config dir holds only these tool sessions)
    files = [f for f in glob.glob(os.path.join(cenv["CLAUDE_CONFIG_DIR"], "projects", "*", "*.jsonl")) if os.path.getmtime(f) >= START]
    files.sort(key=os.path.getmtime)
    return [json.loads(l) for l in open(files[-1]) if l.strip()] if files else []


def usage_of(rows):
    seen, total = set(), {"input_tokens": 0, "output_tokens": 0, "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}
    for r in rows:
        m = r.get("message") or {}
        if r.get("type") == "assistant" and m.get("id") not in seen and m.get("usage"):
            seen.add(m.get("id"))
            for k in total:
                total[k] += int(m["usage"].get(k) or 0)
    return len(seen), total


def kinds_with(rows, text):
    return [(r.get("type"), (r.get("attachment") or {}).get("type")) for r in rows if text in json.dumps(r)]


# --- 1. -p boundary run ---
if a.only in (None, "p"):
    t0 = time.time()
    out = os.path.join(root, "run-p.jsonl")
    run = spawn([a.claude, "--model", a.model, "--settings", settings_file, "--allowedTools", "Bash", "--output-format",
                 "stream-json", "--verbose", "-p", "Use the Bash tool to run exactly: sleep 6; echo step-one . When that has "
                 "finished, use the Bash tool to run exactly: echo step-two . Then reply with one short line."],
                cwd=P["work"], env=cenv, stdout=open(out, "w"), stderr=open(os.path.join(root, "run-p.err"), "w"))
    end = time.time() + 120
    while time.time() < end and not any(s.get("source") == "PreToolUse" for s in statuses()):
        time.sleep(0.1)
    b = c.cmd("inbox.send", to="r7", mode="boundary", content=txt("Teammate note for the vqro smoke test: please include "
                                                                     "the word PINEAPPLE in your final reply."))["delivery"]
    run.wait(240)
    time.sleep(1)
    rows = [json.loads(l) for l in open(out) if l.strip()]
    final = next((r for r in reversed(rows) if r.get("type") == "result"), {})
    tr = transcript_rows()
    results["p"] = {"seconds": round(time.time() - t0, 1), "record": get(b), "result_text": (final.get("result") or "")[:300],
                    "cost_usd": final.get("total_cost_usd"), "usage": final.get("usage"), "num_turns": final.get("num_turns"),
                    "kinds_with_msg": kinds_with(tr, "PINEAPPLE"), "assistant_requests": usage_of(tr)[0]}
    check("smoke -p: boundary message inserted as hook_additional_context and sent in a request",
          ("attachment", "hook_additional_context") in results["p"]["kinds_with_msg"]
          and ("api-request-blob", None) in results["p"]["kinds_with_msg"], results["p"])
    check("smoke -p: evidence reached read", results["p"]["record"].get("stage") == "read", results["p"]["record"])
    results["p"]["model_mentions_message"] = "PINEAPPLE" in (final.get("result") or "")
    print(json.dumps({k: results["p"][k] for k in ("seconds", "cost_usd", "num_turns", "model_mentions_message", "assistant_requests")}), flush=True)

# --- 2. TUI idle wake ---
if a.only in (None, "tui"):
    pid, fd = pty.fork()
    if pid == 0:
        os.chdir(P["work"])
        os.execve(a.claude, [a.claude, "--model", a.model, "--settings", settings_file], cenv)
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 140, 0, 0))
    screen = bytearray()

    def pump(seconds):
        end = time.time() + seconds
        while time.time() < end:
            r, _, _ = select.select([fd], [], [], 0.05)
            if r:
                try:
                    screen.extend(os.read(fd, 65536))
                except OSError:
                    return

    def plain():
        return re.sub(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07]*\x07", "", screen.decode("utf-8", "replace"))

    def wait_for(pred, timeout):
        end = time.time() + timeout
        while time.time() < end:
            if pred():
                return True
            pump(0.2)
        return False

    pump(8)
    # First interactive start in the dedicated config: onboarding (theme, security notes) and the folder trust
    # dialog for the private work dir. No model call happens before a prompt is submitted.
    mark = 0
    for _ in range(8):
        recent = re.sub(r"\s+", "", plain()[mark:])
        mark = len(plain())
        if "Itrustthisfolder" in recent:
            os.write(fd, b"\x1b[B"); pump(0.5); os.write(fd, b"\r"); pump(4)
        elif "PressEntertocontinue" in recent or "Choosethetextstyle" in recent or "Entertoconfirm" in recent:
            os.write(fd, b"\r"); pump(4)
        else:
            break
    results["tui_start_screen"] = plain()[-400:]
    for ch in "Reply with just: ok":
        os.write(fd, ch.encode()); pump(0.04)
    os.write(fd, b"\r")
    armed = wait_for(lambda: any(e["event"] == "waiter_armed" for e in hooklog()), 120)
    check("smoke TUI: first turn ended and the idle waiter is armed", armed, plain()[-600:])
    pump(2)
    s = c.cmd("inbox.send", to="r7", mode="start", content=txt("Teammate note for the vqro smoke test: reply with just the word MANGO."))["delivery"]
    woke = wait_for(lambda: any("MANGO" in json.dumps(r) and r.get("type") == "assistant" for r in transcript_rows()), 120)
    pump(3)
    tr = transcript_rows()
    os.write(fd, b"\x03"); pump(0.5); os.write(fd, b"\x03"); pump(2)  # exit: SessionEnd runs the last evidence pass
    try:
        os.kill(pid, 9); os.waitpid(pid, 0)
    except OSError:
        pass
    n, usage = usage_of(tr)
    results["tui"] = {"record": get(s), "kinds_with_msg": kinds_with(tr, "reply with just the word MANGO"), "assistant_requests": n,
                      "usage": usage, "screen_tail": plain()[-800:]}
    check("smoke TUI: a start message wakes the idle session and the model answers it", woke, results["tui"])
    print(json.dumps({"tui_requests": n, "usage": usage, "stage": results["tui"]["record"].get("stage")}), flush=True)

json.dump(results, open(os.path.join(root, "smoke-results.json"), "w"), indent=1, default=str)
for p in procs:
    if p.poll() is None:
        p.kill()
print(f"{'FAILED' if fails else 'OK'}: {len(fails)} failures; root {root}")
sys.exit(1 if fails else 0)
