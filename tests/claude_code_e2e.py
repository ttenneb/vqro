"""A3 end-to-end with the real Claude Code CLI and a fake Anthropic endpoint (no network, no spend).

Run inside a private network namespace (loopback only):
  env -i HOME=$HOME PATH=$PATH unshare -rn sh -c 'ip link set lo up && exec python3 tests/claude_code_e2e.py \
      --vqro target/debug/vqro --hook target/debug/vqro-claude-hook --claude /path/to/claude'

The fake model's request log is the oracle for *when* a message reached the model (coordination-v0 §7, T2/T10):
  A  boundary sent during tool 1 of a 3-step run: in exactly the request after that tool batch, as a system reminder;
  B  settle sent at the same time: only in the request after the final answer (Stop continuation);
  D  resume: a claim handed over but absent from the transcript becomes unknown, never resent;
  S  status reports from hooks.
"""
import argparse, glob, json, os, shutil, socket, subprocess, sys, tempfile, time

ap = argparse.ArgumentParser()
ap.add_argument("--vqro", required=True)
ap.add_argument("--hook", required=True)
ap.add_argument("--claude", required=True)
ap.add_argument("--root")
ap.add_argument("--framing-only", action="store_true", help="only native tool/Stop model-envelope witnesses")
ap.add_argument("--model", default="claude-sonnet-4-5")
ap.add_argument("--inbox", choices=["fake", "k2", "k2core"], default="fake")
ap.add_argument("--coord-ext")
a = ap.parse_args()
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from coord_harness import Harness, txt, finish_framing  # noqa: E402

root = a.root or tempfile.mkdtemp(prefix="a3-cc-")
os.makedirs(root, exist_ok=True)
P = {k: os.path.join(root, k) for k in ("home", "cfg", "work", "hookstate")}
for d in P.values():
    os.makedirs(d, exist_ok=True)
model_log = os.path.join(root, "model.jsonl")
port = 18800
results, fails = {"inbox": a.inbox}, []
h = Harness(root, a.vqro, inbox=a.inbox, coord_ext=a.coord_ext)
procs = h.procs
import atexit, signal  # noqa: E402
atexit.register(h.stop)
signal.signal(signal.SIGTERM, lambda *_: (h.stop(), sys.exit(1)))


def check(name, ok, detail=""):
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"  ({str(detail)[:600]})"), flush=True)
    results[name] = {"ok": bool(ok), "detail": None if ok else str(detail)[:2000]}
    if not ok:
        fails.append(name)


def spawn(argv, **kw):
    return h.spawn(argv, **kw)


spawn([sys.executable, os.path.join(HERE, "fixtures", "fake_anthropic.py"), "--port", str(port), "--log", model_log])
AGENT = h.agent(json.loads(subprocess.run([a.hook, "declaration"], capture_output=True, text=True, check=True).stdout)["pull"])  # the launcher writes what the plugin declares
CTX = ["context"]
base = dict(h.env, HOME=P["home"])
settings = json.loads(subprocess.run([a.hook, "settings", os.path.abspath(a.hook)], capture_output=True, text=True).stdout)
json.dump(settings, open(os.path.join(P["cfg"], "settings.json"), "w"), indent=1)
cenv = dict(base, PATH=os.environ["PATH"], CLAUDE_CONFIG_DIR=P["cfg"], VQRO_AGENT=AGENT, VQRO_PRINCIPAL=h.PLUGIN, VQRO_CLAUDE_STATE=P["hookstate"], DISABLE_TELEMETRY="1",
            DISABLE_ERROR_REPORTING="1", CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC="1", DISABLE_AUTOUPDATER="1",
            ANTHROPIC_BASE_URL=f"http://127.0.0.1:{port}", ANTHROPIC_API_KEY="sk-fake-not-a-key")
version = subprocess.run([a.claude, "--version"], capture_output=True, text=True, env=cenv).stdout.strip()
print("claude", version, flush=True)
results["claude_version"] = version


def requests():
    return [json.loads(l) for l in open(model_log)] if os.path.exists(model_log) else []


def first_request_with(text):
    for r in requests():
        if any(text in b["text"] for m in r["messages"] for b in m["blocks"]):
            return r["n"]
    return None


def hook_events(name):
    path = os.path.join(P["hookstate"], "hook.log")
    return [e for e in (json.loads(l) for l in open(path)) if e["event"] == "hook" and e["detail"].get("hook") == name] if os.path.exists(path) else []


def get(d):
    return h.get(d)


def wait_for(pred, timeout=30, step=0.05):
    end = time.time() + timeout
    while time.time() < end:
        if pred():
            return True
        time.sleep(step)
    return False


base_argv = [a.claude, "--model", a.model, "--allowedTools", "Bash", "--output-format", "stream-json", "--verbose"]

# --- A + B: boundary and settle during a 3-step run (-p) ---
n0 = len(requests())
run = spawn(base_argv + ["-p", "steps:3 sleep:2 begin"], cwd=P["work"], env=cenv, stdout=open(os.path.join(root, "runAB.jsonl"), "w"),
            stderr=open(os.path.join(root, "runAB.err"), "w"))
wait_for(lambda: hook_events("PreToolUse"), 60)
sent_after = len(requests()) - n0
b1 = h.send(AGENT, "boundary", txt("MSG-BOUNDARY-A: the linux build is green"), render=CTX)
s1 = h.send(AGENT, "settle", txt("MSG-SETTLE-B: also summarise the result"), render=CTX)
run.wait(180)
reqs = requests()[n0:]
nb, ns = first_request_with("MSG-BOUNDARY-A"), first_request_with("MSG-SETTLE-B")
results["A_B"] = {"requests_made_before_send": sent_after, "boundary_first_request": nb, "settle_first_request": ns,
                  "requests": [r["n"] for r in reqs], "boundary": get(b1), "settle": get(s1)}
check("A boundary lands in the request right after the first tool batch", nb == n0 + 2 and sent_after == 1, results["A_B"])
check("B settle lands only after the final answer (Stop continuation)", ns is not None and ns == reqs[-1]["n"] and ns > (nb or 0) + 2, results["A_B"])
# where/how it was inserted: role and block of the request
blk = next((m for r in reqs if r["n"] == nb for m in r["messages"] if any("MSG-BOUNDARY-A" in b["text"] for b in m["blocks"])), None)
results["A_B"]["boundary_insert_role"] = blk and blk["role"]
results["A_B"]["boundary_insert_block"] = blk and next(b for b in blk["blocks"] if "MSG-BOUNDARY-A" in b["text"])["text"][:300]
check("A inserted as context (system reminder), not as the user's prompt text", blk is not None and "system-reminder" in results["A_B"]["boundary_insert_block"], results["A_B"]["boundary_insert_block"])
check("A evidence: supplied, then read from the transcript (request payload + finished answer)", get(b1).get("state") == "read", get(b1))
check("B evidence by SessionEnd: read", get(s1).get("state") == "read", get(s1))

if a.framing_only:
    finish_framing(h, results, fails)

# --- D: resume; an open claim with no transcript entry becomes unknown, never resent ---
sessions = sorted(glob.glob(os.path.join(P["cfg"], "projects", "*", "*.jsonl")), key=os.path.getmtime)
session_id = os.path.basename(sessions[0])[:-6] if sessions else None
# a claim taken by an earlier consumer that never reached the transcript (simulated crash after take)
d1 = h.send(AGENT, "boundary", txt("MSG-LOST-D"), render=CTX)
time.sleep(0.3)
cons = json.load(open(glob.glob(os.path.join(P["hookstate"], f"{session_id}.json"))[0]))["consumer"] if session_id else None
h.c.cmd("coord.inbox.take", op=h.op(), agent=AGENT, consumer=cons, at="turn", limit=1)  # as the old consumer
n3 = len(requests())
spawn(base_argv + ["-p", "--resume", session_id, "resumed, no task"], cwd=P["work"], env=cenv,
      stdout=open(os.path.join(root, "runD.jsonl"), "w"), stderr=open(os.path.join(root, "runD.err"), "w")).wait(120)
results["D_resume"] = {"session": session_id, "record": get(d1), "ever_sent": first_request_with("MSG-LOST-D"),
                       "A_after_resume": get(b1).get("state")}
check("D resume: claim without transcript evidence -> unknown, never sent", get(d1)["state"] == "unknown" and first_request_with("MSG-LOST-D") is None, results["D_resume"])
check("D resume: read records keep their evidence", get(b1)["state"] == "read")

# --- S: status from hooks (fake inbox only: no standard status command yet) ---
if a.inbox == "fake":
    st_events = h.statuses()
    results["S_status"] = [s["state"] for s in st_events][-30:]
    check("S status (agents.status) running on prompt/tool, idle on Stop", {"running", "idle"} <= {s["state"] for s in st_events})

results["hook_log"] = open(os.path.join(P["hookstate"], "hook.log")).read().splitlines()[-40:]
json.dump(results, open(os.path.join(root, "results.json"), "w"), indent=1)
h.stop()
print(f"{'FAILED' if fails else 'OK'}: {len(fails)} failures; root {root}")
sys.exit(1 if fails else 0)
