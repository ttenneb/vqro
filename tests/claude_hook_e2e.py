"""Claude Code plugin, hook level (no Claude Code, no model): a private vqro core + a coordination inbox (the fake, or
K2 itself with --inbox k2), and vqro-claude-hook driven with the hook inputs Claude Code documents and the
transcript entry shapes measured on 2.1.329. Checks the pull contract (coordination-v0 §7): eligibility per point,
batch order, `supplied` → `read` from transcript evidence only, never-flattened content, subagent hooks ignored,
reconcile → unknown, stale reports rejected, idle waiter wake via asyncRewake exit 2 and retirement on a new turn.

  python3 tests/claude_hook_e2e.py --vqro target/debug/vqro --hook target/debug/vqro-claude-hook [--inbox k2 --coord-ext …]
"""
import argparse, json, os, shutil, subprocess, sys, tempfile, time

ap = argparse.ArgumentParser()
ap.add_argument("--vqro", required=True)
ap.add_argument("--hook", required=True)
ap.add_argument("--inbox", choices=["fake", "k2"], default="fake")
ap.add_argument("--coord-ext")
ap.add_argument("--keep", action="store_true")
a = ap.parse_args()
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from coord_harness import Harness, txt  # noqa: E402

root = tempfile.mkdtemp(prefix="a3-hook-", dir=os.environ.get("A3_SCRATCH"))
h = Harness(root, a.vqro, inbox=a.inbox, coord_ext=a.coord_ext)
import atexit, signal  # noqa: E402
atexit.register(h.stop)
signal.signal(signal.SIGTERM, lambda *_: (h.stop(), sys.exit(1)))
fails = []


def check(name, ok, detail=""):
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"  ({str(detail)[:400]})"), flush=True)
    if not ok:
        fails.append(name)


AGENT = h.agent({"points": ["turn", "settle", "idle"], "insert": "context", "evidence": "none"})
CTX = ["context"]  # senders accept context insertion
transcript = os.path.join(root, "transcript.jsonl")
open(transcript, "w").close()
hook_env = dict(h.env, VQRO_AGENT=AGENT, VQRO_CLAUDE_STATE=os.path.join(root, "hookstate"), CLAUDE_CODE_ENTRYPOINT="cli")
if a.inbox == "fake":
    hook_env["VQRO_CLAUDE_STATUS_COMMAND"] = "agents.terminal_status"
waiters = []


def hook(event, extra=None, mode="hook", wait=True):
    payload = {"session_id": "s1", "transcript_path": transcript, "cwd": root, "hook_event_name": event, **(extra or {})}
    p = subprocess.Popen([a.hook, mode], env=hook_env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    if not wait:
        p.stdin.write(json.dumps(payload)); p.stdin.close()
        waiters.append(p)
        h.procs.append(p)
        return p
    out, err = p.communicate(json.dumps(payload), timeout=20)
    return p.returncode, out, err


def context(out):
    return (json.loads(out).get("hookSpecificOutput") or {}).get("additionalContext", "") if out.strip() else ""


def state(d):
    return h.get(d).get("state")


def transcript_add(*rows):
    with open(transcript, "a") as f:
        for r in rows:
            f.write(json.dumps(r) + "\n")


def hooklog():
    path = os.path.join(root, "hookstate", "hook.log")
    return [json.loads(l) for l in open(path)] if os.path.exists(path) else []


# 1. SessionStart: a new consumer reconciles before anything is taken
rc, out, err = hook("SessionStart", {"source": "startup"})
check("session start exit 0, no output", rc == 0 and out == "", err)
check("session start reconciled", any(e["event"] == "reconciled" for e in hooklog()), [e["event"] for e in hooklog()])
# 2. turn serves boundary only, in order, as one batch
b1 = h.send(AGENT, "boundary", txt("B1 first"), render=CTX)
s1 = h.send(AGENT, "settle", txt("S1 settle"), render=CTX)
st1 = h.send(AGENT, "start", txt("ST1 start"), render=CTX)
b2 = h.send(AGENT, "boundary", txt("B2 second"), render=CTX)
time.sleep(0.3)
hook("PreToolUse", {"tool_name": "Bash", "tool_input": {"command": "true"}})
rc, out, _ = hook("PostToolBatch", {"tool_calls": [{"tool_name": "Bash", "tool_input": {}, "tool_use_id": "t1", "tool_response": "ok"}]})
ctx = context(out)
check("turn take returns the boundary records in order", f"vqro message {b1} " in ctx and ctx.find(f"vqro message {b2} ") > ctx.find(f"vqro message {b1} "), ctx)
check("turn take never returns settle/start", s1 not in ctx and st1 not in ctx and state(s1) in ("held", "recorded") and state(st1) in ("held", "recorded"))
check("hand-off reported supplied (insert: context, evidence: none)", state(b1) == "supplied" and state(b2) == "supplied", h.get(b1))
# 2b. typed content: bare strings are rejected; a block context cannot carry is never flattened or inserted
try:
    h.c.cmd("coord.inbox.send", op=h.op(), submission=h.op() + ":s", to=AGENT, mode="boundary", content="bare string")
    bare = "accepted (BAD)"
except RuntimeError as e:
    bare = str(e)
check("bare-string content is rejected by the inbox", "invalid" in bare, bare)
im = h.send(AGENT, "boundary", [{"type": "text", "text": "IMG"}, {"type": "image", "mime": "image/png", "data": "iVBORw0KGgo="}], render=CTX)
time.sleep(0.3)
rc, out, _ = hook("PostToolBatch", {"tool_calls": []})
check("image record is never handed to a text-only plugin; it stays held", im not in out and "IMG" not in out and state(im) == "held", h.get(im))
h.edit(im, txt("IMG removed"))
time.sleep(0.3)
rc, out, _ = hook("PostToolBatch", {"tool_calls": []})
check("after an edit to text, the record is taken", im in context(out), out)
# 3. subagent hooks are ignored entirely
rc, out, _ = hook("PostToolBatch", {"agent_id": "sub1", "agent_type": "Explore", "tool_calls": []})
check("subagent PostToolBatch takes nothing", out == "" and state(s1) in ("held", "recorded"))
# 4. read only from model-visible evidence: the request payload carrying it, then a finished assistant entry
transcript_add({"type": "attachment", "uuid": "h-out", "attachment": {"type": "hook_success", "hookEvent": "PostToolBatch", "stdout": ctx}},
               {"type": "assistant", "uuid": "a0", "message": {"role": "assistant", "stop_reason": "end_turn", "content": []}})
hook("PreToolUse", {"tool_name": "Bash", "tool_input": {}})
check("hook stdout record + an unrelated answer are not read evidence", state(b1) == "supplied", h.get(b1))
transcript_add({"type": "attachment", "uuid": "u-ctx", "attachment": {"type": "hook_additional_context", "content": [ctx], "hookEvent": "PostToolBatch"}},
               {"type": "api-request-blob", "message": {"role": "user", "content": [{"type": "text", "text": f"<system-reminder>{ctx}</system-reminder>"}]}},
               {"type": "assistant", "uuid": "a1", "message": {"role": "assistant", "stop_reason": "end_turn", "content": [{"type": "text", "text": "ok"}]}})
# 5. Stop serves settle (boundary + settle), continues once
rc, out, _ = hook("Stop", {"stop_hook_active": False, "last_assistant_message": "ok"})
ctx2 = context(out)
check("b1 read after its request and a finished answer", state(b1) == "read", h.get(b1))
check("settle take at Stop returns S1, not start", s1 in ctx2 and st1 not in ctx2, ctx2)
check("Stop output is hook feedback (additionalContext) for Stop", bool(out) and json.loads(out)["hookSpecificOutput"]["hookEventName"] == "Stop")
rc, out, _ = hook("Stop", {"stop_hook_active": True, "last_assistant_message": "done"})
check("second Stop with nothing eligible ends the turn", out == "")
# 6. idle waiter: takes the start message at once (idle), exits 2 with the text on stderr
w = hook("Stop", None, mode="wait", wait=False)
try:
    w.wait(15)
except subprocess.TimeoutExpired:
    pass
werr = w.stderr.read() if w.returncode is not None else ""
check("idle waiter takes start and exits 2 (asyncRewake)", w.returncode == 2 and st1 in werr, f"rc={w.returncode} err={werr[:200]}")
# 7. a waiter armed while idle wakes on a later admission (counts-only wake), not by polling
hook("Notification", {"notification_type": "idle_prompt", "message": "Claude is waiting"})
w2 = hook("Notification", None, mode="wait", wait=False)
time.sleep(1.0)
check("armed waiter with nothing eligible keeps waiting", w2.poll() is None)
t0 = time.time()
st2 = h.send(AGENT, "start", txt("ST2 later"), render=CTX)
try:
    w2.wait(10)
except subprocess.TimeoutExpired:
    pass
check("wake on admission -> exit 2 with the message", w2.returncode == 2 and st2 in w2.stderr.read(), f"rc={w2.returncode}")
print(f"     wake latency {time.time() - t0:.3f}s")
# 8. a new turn retires an armed waiter without taking anything
hook("Notification", {"notification_type": "idle_prompt", "message": "idle"})
w3 = hook("Notification", None, mode="wait", wait=False)
time.sleep(0.8)
hook("UserPromptSubmit", {"prompt": "human typed this"})
st3 = h.send(AGENT, "start", txt("ST3 while busy"), render=CTX)
try:
    w3.wait(40)
except subprocess.TimeoutExpired:
    pass
check("new turn retires the waiter (exit 0) and nothing is taken while busy", w3.returncode == 0 and state(st3) in ("held", "recorded"), f"rc={w3.returncode} state={state(st3)}")
# 9. restart: a claim taken but never reported (crash between take and hand-off) becomes unknown, never retaken
consumer = json.load(open(os.path.join(root, "hookstate", "s1.json")))["consumer"]
b3 = h.send(AGENT, "boundary", txt("B3 lost"), render=CTX)
time.sleep(0.3)
taken = h.c.cmd("coord.inbox.take", op=h.op(), agent=AGENT, consumer=consumer, at="turn", limit=1)  # as the old consumer
claim = (taken.get("records") or [{}])[0].get("claim")
check("simulated crash: b3 claimed by the old consumer", state(b3) == "claimed", h.get(b3))
hook("SessionStart", {"source": "resume"})
check("reconcile: open claim without evidence -> unknown, never resent", state(b3) == "unknown", h.get(b3))
rc, out, _ = hook("PostToolBatch", {"tool_calls": []})
check("unknown is never taken again", b3 not in out)
try:
    h.c.cmd("coord.inbox.report", op=h.op(), delivery=b3, claim=claim, stage="supplied")
    late = "accepted (BAD)"
except RuntimeError as e:
    late = str(e)
check("old consumer's late report after reconcile is rejected", "stale" in late or "conflict" in late, late)
# 10. status reports (fake inbox only: no standard status command exists)
if a.inbox == "fake":
    seen = {(s["source"], s["state"]) for s in h.statuses()}
    check("status: SessionStart unknown, UserPromptSubmit working, Stop idle",
          {("SessionStart", "unknown"), ("UserPromptSubmit", "working"), ("Stop", "idle")} <= seen, seen)
# 11. hook latency without mail: what every tool batch costs
t = time.time()
for _ in range(10):
    hook("PostToolBatch", {"tool_calls": []})
print(f"     PostToolBatch: {(time.time() - t) / 10 * 1000:.1f} ms per hook (incl. process start)")

h.stop()
if not a.keep:
    shutil.rmtree(root, ignore_errors=True)
else:
    print("kept", root)
print(f"{'FAILED' if fails else 'OK'}: {len(fails)} failures ({a.inbox})")
sys.exit(1 if fails else 0)
