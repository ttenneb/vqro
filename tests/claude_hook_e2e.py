"""A3 hook-level test (no Claude Code, no model): a private vqro core + the fake inbox, and vqro-claude-hook driven
with the hook inputs Claude Code documents (shapes from code.claude.com/docs/en/hooks). Checks the pull contract
(coordination-v0 §7): eligibility per point, batch order, supplied -> inserted -> read from transcript evidence,
reconcile -> unknown, subagent hooks ignored, idle waiter wake via asyncRewake exit 2, retirement on a new turn.

  python3 tests/claude_hook_e2e.py --vqro target/debug/vqro --hook target/debug/vqro-claude-hook
"""
import argparse, json, os, shutil, socket, subprocess, sys, tempfile, time

ap = argparse.ArgumentParser()
ap.add_argument("--vqro", required=True)
ap.add_argument("--hook", required=True)
ap.add_argument("--keep", action="store_true")
a = ap.parse_args()
HERE = os.path.dirname(os.path.abspath(__file__))
root = tempfile.mkdtemp(prefix="a3-hook-", dir=os.environ.get("A3_SCRATCH"))
sock = os.path.join(root, "v.sock")
env = {"HOME": os.path.join(root, "home"), "PATH": os.environ["PATH"], "XDG_RUNTIME_DIR": os.path.join(root, "xrun"),
       "VQRO_STATE_DIR": os.path.join(root, "st"), "VQRO_SOCKET": sock, "VQRO_EXTENSIONS_DIR": os.path.join(root, "noext"),
       "VQRO_HERDR": '["/bin/false"]', "HC_SOCKET": os.path.join(root, "h.sock"), "HC_SESSION": "a3"}
for d in ("home", "xrun", "st", "noext"):
    os.makedirs(os.path.join(root, d))
procs = []
fails = []


def check(name, ok, detail=""):
    print(("PASS " if ok else "FAIL ") + name + (f"  ({detail})" if detail and not ok else ""))
    if not ok:
        fails.append(name)


core = subprocess.Popen([a.vqro, "serve", "--fake"], env=env, stdout=open(os.path.join(root, "core.log"), "w"), stderr=subprocess.STDOUT)
procs.append(core)
for _ in range(100):
    if os.path.exists(sock):
        break
    time.sleep(0.05)
inbox_log = os.path.join(root, "inbox.jsonl")
inbox = subprocess.Popen([sys.executable, os.path.join(HERE, "fixtures", "fake_inbox.py"), inbox_log],
                         env=dict(env, VQRO_EXT_PATH=os.path.join(HERE, "..", "extensions")), stderr=open(os.path.join(root, "inbox.err"), "w"))
procs.append(inbox)


class Client:
    def __init__(self):
        self.s = socket.socket(socket.AF_UNIX); self.s.connect(sock); self.f = self.s.makefile("rw"); self.n = 0
        self.call("hello", {"client": "a3-test"})

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
for _ in range(100):
    if any(x.get("name") == "inbox.take" for x in c.call("command.list", {})["commands"]):
        break
    time.sleep(0.1)
transcript = os.path.join(root, "transcript.jsonl")
open(transcript, "w").close()
hook_env = dict(env, VQRO_AGENT="r7", VQRO_CLAUDE_STATE=os.path.join(root, "hookstate"))


def hook(event, extra=None, mode="hook", wait=True):
    payload = {"session_id": "s1", "transcript_path": transcript, "cwd": root, "hook_event_name": event, **(extra or {})}
    p = subprocess.Popen([a.hook, mode], env=hook_env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    if not wait:
        p.stdin.write(json.dumps(payload)); p.stdin.close()
        procs.append(p)
        return p
    out, err = p.communicate(json.dumps(payload), timeout=20)
    return p.returncode, out, err


def context(out):
    return (json.loads(out).get("hookSpecificOutput") or {}).get("additionalContext", "") if out.strip() else ""


def get(d):
    return c.cmd("inbox.get", delivery=d)


def transcript_add(*rows):
    with open(transcript, "a") as f:
        for r in rows:
            f.write(json.dumps(r) + "\n")


# 1. SessionStart: consumer + reconcile; status idle
rc, out, err = hook("SessionStart", {"source": "startup"})
check("session start exit 0, no output", rc == 0 and out == "", err)
# 2. turn point serves boundary only, in order, as one batch
b1 = c.cmd("inbox.send", to="r7", mode="boundary", content=[{"type": "text", "text": "B1 first"}])["delivery"]
s1 = c.cmd("inbox.send", to="r7", mode="settle", content="S1 settle")["delivery"]
st1 = c.cmd("inbox.send", to="r7", mode="start", content="ST1 start")["delivery"]
b2 = c.cmd("inbox.send", to="r7", mode="boundary", content="B2 second")["delivery"]
rc, out, _ = hook("PreToolUse", {"tool_name": "Bash", "tool_input": {"command": "true"}})
rc, out, _ = hook("PostToolBatch", {"tool_calls": [{"tool_name": "Bash", "tool_input": {}, "tool_use_id": "t1", "tool_response": "ok"}]})
ctx = context(out)
check("turn take returns boundary records in order", ctx.find(f"vqro message {b1} ") >= 0 and ctx.find(f"vqro message {b2} ") > ctx.find(f"vqro message {b1} "), ctx)
check("turn take never returns settle/start", s1 not in ctx and st1 not in ctx and get(s1)["state"] == "held" and get(st1)["state"] == "held")
check("hand-off reported supplied (insert: context)", get(b1).get("stage") == "supplied" and get(b2).get("stage") == "supplied", get(b1))
# 3. subagent hooks are ignored entirely
rc, out, _ = hook("PostToolBatch", {"agent_id": "sub1", "agent_type": "Explore", "tool_calls": []})
check("subagent PostToolBatch takes nothing", out == "" and get(s1)["state"] == "held")
# 4. transcript evidence: entry -> inserted; later assistant message -> read
transcript_add({"type": "user", "uuid": "u-ctx", "message": {"role": "user", "content": f"<system-reminder>{ctx}</system-reminder>"}})
hook("PreToolUse", {"tool_name": "Bash", "tool_input": {}})
check("entry in transcript -> inserted", get(b1).get("stage") == "inserted", get(b1))
transcript_add({"type": "assistant", "uuid": "a1", "message": {"role": "assistant", "stop_reason": "end_turn", "content": [{"type": "text", "text": "ok"}]}})
# 5. Stop serves settle (boundary + settle), continues once
rc, out, _ = hook("Stop", {"stop_hook_active": False, "last_assistant_message": "ok"})
ctx2 = context(out)
check("b1 read after a completed assistant message", get(b1).get("stage") == "read", get(b1))
check("settle take at Stop returns S1, not start", s1 in ctx2 and st1 not in ctx2, ctx2)
check("Stop output is hook feedback (additionalContext) for Stop", json.loads(out)["hookSpecificOutput"]["hookEventName"] == "Stop")
rc, out, _ = hook("Stop", {"stop_hook_active": True, "last_assistant_message": "done"})
check("second Stop with nothing eligible ends the turn", out == "")
# 6. idle waiter: takes the start message at once (agent idle), exits 2 with the text on stderr
w = hook("Stop", None, mode="wait", wait=False)
try:
    w.wait(15)
except subprocess.TimeoutExpired:
    pass
werr = w.stderr.read() if w.returncode is not None else ""
check("idle waiter takes start and exits 2 (asyncRewake)", w.returncode == 2 and st1 in werr, f"rc={w.returncode} err={werr[:200]}")
# 7. waiter armed while idle wakes on a later admission (counts-only wake), not by polling
hook("Notification", {"notification_type": "idle_prompt", "message": "Claude is waiting"})
w2 = hook("Stop", None, mode="wait", wait=False)
time.sleep(1.0)
check("armed waiter with nothing eligible keeps waiting", w2.poll() is None)
t0 = time.time()
st2 = c.cmd("inbox.send", to="r7", mode="start", content="ST2 later")["delivery"]
try:
    w2.wait(10)
except subprocess.TimeoutExpired:
    pass
check("wake on admission -> exit 2 with the message", w2.returncode == 2 and st2 in w2.stderr.read(), f"rc={w2.returncode}")
print(f"     wake latency {time.time() - t0:.3f}s")
# 8. a new turn retires an armed waiter without taking anything
hook("Notification", {"notification_type": "idle_prompt", "message": "idle"})
w3 = hook("Stop", None, mode="wait", wait=False)
time.sleep(0.8)
hook("UserPromptSubmit", {"prompt": "human typed this"})
st3 = c.cmd("inbox.send", to="r7", mode="start", content="ST3 while busy")["delivery"]
try:
    w3.wait(40)
except subprocess.TimeoutExpired:
    pass
check("new turn retires the waiter (exit 0) and nothing is taken while busy", w3.returncode == 0 and get(st3)["state"] == "held", f"rc={w3.returncode} state={get(st3)['state']}")
# 9. restart: a claim handed over but never in the transcript becomes unknown; one in the transcript inserted
b3 = c.cmd("inbox.send", to="r7", mode="boundary", content="B3 lost")["delivery"]
rc, out, _ = hook("PostToolBatch", {"tool_calls": []})
check("b3 supplied", b3 in context(out) and get(b3).get("stage") == "supplied")
rc, out, _ = hook("SessionStart", {"source": "resume"})
check("reconcile: supplied without transcript entry -> unknown, never resent", get(b3)["state"] == "unknown", get(b3))
rc, out, _ = hook("PostToolBatch", {"tool_calls": []})
check("unknown is never taken again", b3 not in out)
try:
    c.cmd("inbox.report", op="late-1", delivery=b3, claim=get(b3)["claim"], stage="inserted")
    late = "accepted (BAD)"
except RuntimeError as e:
    late = str(e)
check("old consumer's late report after reconcile is rejected", "stale" in late, late)
# 10. status reports
statuses = [json.loads(l) for l in open(inbox_log) if '"status"' in l]
seen = {(s["source"], s["state"]) for s in statuses}
check("status: SessionStart idle, UserPromptSubmit working, Stop idle",
      {("SessionStart", "idle"), ("UserPromptSubmit", "working"), ("Stop", "idle")} <= seen, seen)
# 11. hook latency (no mail): what every tool batch costs
t = time.time()
for _ in range(10):
    hook("PostToolBatch", {"tool_calls": []})
print(f"     PostToolBatch without mail: {(time.time() - t) / 10 * 1000:.1f} ms per hook (debug build, incl. process start)")

for p in procs:
    if p.poll() is None:
        p.terminate()
        try:
            p.wait(5)
        except subprocess.TimeoutExpired:
            p.kill()
if not a.keep:
    shutil.rmtree(root, ignore_errors=True)
else:
    print("kept", root)
print(f"{'FAILED' if fails else 'OK'}: {len(fails)} failures")
sys.exit(1 if fails else 0)
