"""A3 end-to-end with the real Claude Code CLI and a fake Anthropic endpoint (no network, no spend).

Run inside a private network namespace (loopback only):
  env -i HOME=$HOME PATH=$PATH unshare -rn sh -c 'ip link set lo up && exec python3 tests/claude_code_e2e.py \
      --vqro target/debug/vqro --hook target/debug/vqro-claude-hook --claude /path/to/claude'

The fake model's request log is the oracle for *when* a message reached the model (coordination-v0 §7, T2/T10):
  A  boundary sent during tool 1 of a 3-step run: in exactly the request after that tool batch, as a system reminder;
  B  settle sent at the same time: only in the request after the final answer (Stop continuation);
  C  stream-json session left open and idle: a start message wakes it through the asyncRewake waiter;
  D  resume: a claim handed over but absent from the transcript becomes unknown, never resent;
  S  status reports from hooks.
"""
import argparse, glob, json, os, shutil, socket, subprocess, sys, tempfile, time

ap = argparse.ArgumentParser()
ap.add_argument("--vqro", required=True)
ap.add_argument("--hook", required=True)
ap.add_argument("--claude", required=True)
ap.add_argument("--root")
ap.add_argument("--model", default="claude-sonnet-4-5")
ap.add_argument("--real", action="store_true", help="use the real Anthropic API (smoke only; not in a netns)")
ap.add_argument("--skip-idle", action="store_true", help="skip C (the idle wake needs an interactive session)")
a = ap.parse_args()
HERE = os.path.dirname(os.path.abspath(__file__))
root = a.root or tempfile.mkdtemp(prefix="a3-cc-")
os.makedirs(root, exist_ok=True)
P = {k: os.path.join(root, k) for k in ("home", "cfg", "work", "xrun", "st", "noext", "hookstate")}
for d in P.values():
    os.makedirs(d, exist_ok=True)
sock, model_log, inbox_log = (os.path.join(root, n) for n in ("v.sock", "model.jsonl", "inbox.jsonl"))
port = 18800
base = {"HOME": P["home"], "PATH": os.environ["PATH"], "XDG_RUNTIME_DIR": P["xrun"], "VQRO_STATE_DIR": P["st"],
        "VQRO_SOCKET": sock, "VQRO_EXTENSIONS_DIR": P["noext"], "VQRO_HERDR": '["/bin/false"]',
        "HC_SOCKET": os.path.join(root, "h.sock"), "HC_SESSION": "a3"}
def txt(s):
    return [{"type": "text", "text": s}]


procs, results, fails = [], {}, []


def _cleanup(*_):
    for p in procs:
        if p.poll() is None:
            p.kill()
    sys.exit(1)


import atexit, signal  # noqa: E402
atexit.register(lambda: [p.kill() for p in procs if p.poll() is None])
signal.signal(signal.SIGTERM, _cleanup)


def check(name, ok, detail=""):
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"  ({detail})"), flush=True)
    results[name] = {"ok": bool(ok), "detail": detail if not ok else None}
    if not ok:
        fails.append(name)


def spawn(argv, **kw):
    p = subprocess.Popen(argv, **kw)
    procs.append(p)
    return p


spawn([a.vqro, "serve", "--fake"], env=base, stdout=open(os.path.join(root, "core.log"), "w"), stderr=subprocess.STDOUT)
while not os.path.exists(sock):
    time.sleep(0.05)
spawn([sys.executable, os.path.join(HERE, "fixtures", "fake_inbox.py"), inbox_log],
      env=dict(base, VQRO_EXT_PATH=os.path.join(HERE, "..", "extensions")), stderr=open(os.path.join(root, "inbox.err"), "w"))
if not a.real:
    spawn([sys.executable, os.path.join(HERE, "fixtures", "fake_anthropic.py"), "--port", str(port), "--log", model_log])


class Client:
    def __init__(self):
        self.s = socket.socket(socket.AF_UNIX); self.s.connect(sock); self.f = self.s.makefile("rw"); self.n = 0
        self.call("hello", {"client": "a3-e2e"})

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
settings = json.loads(subprocess.run([a.hook, "settings", os.path.abspath(a.hook)], capture_output=True, text=True).stdout)
json.dump(settings, open(os.path.join(P["cfg"], "settings.json"), "w"), indent=1)
cenv = dict(base, CLAUDE_CONFIG_DIR=P["cfg"], VQRO_AGENT="r7", VQRO_CLAUDE_STATE=P["hookstate"], DISABLE_TELEMETRY="1",
            DISABLE_ERROR_REPORTING="1", CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC="1", DISABLE_AUTOUPDATER="1")
if a.real:
    for k in ("ANTHROPIC_API_KEY", "CLAUDE_CODE_OAUTH_TOKEN"):
        if os.environ.get(k):
            cenv[k] = os.environ[k]
else:
    cenv.update(ANTHROPIC_BASE_URL=f"http://127.0.0.1:{port}", ANTHROPIC_API_KEY="sk-fake-not-a-key")
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


def inbox_events(kind):
    return [json.loads(l) for l in open(inbox_log) if f'"event": "{kind}"' in l] if os.path.exists(inbox_log) else []


def get(d):
    return c.cmd("inbox.get", delivery=d)


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
wait_for(lambda: any(s.get("source") == "PreToolUse" for s in inbox_events("status")), 60)
sent_after = len(requests()) - n0
b1 = c.cmd("inbox.send", to="r7", mode="boundary", content=txt("MSG-BOUNDARY-A: the linux build is green"))["delivery"]
s1 = c.cmd("inbox.send", to="r7", mode="settle", content=txt("MSG-SETTLE-B: also summarise the result"))["delivery"]
run.wait(180)
reqs = requests()[n0:]
nb, ns = first_request_with("MSG-BOUNDARY-A"), first_request_with("MSG-SETTLE-B")
final_ack = next((r["n"] for r in reqs if r["n"] > (nb or 0) and not any(m for m in [])), None)
results["A_B"] = {"requests_made_before_send": sent_after, "boundary_first_request": nb, "settle_first_request": ns,
                  "requests": [r["n"] for r in reqs], "boundary": get(b1), "settle": get(s1)}
check("A boundary lands in the request right after the first tool batch", nb == n0 + 2 and sent_after == 1, results["A_B"])
check("B settle lands only after the final answer (Stop continuation)", ns is not None and ns == reqs[-1]["n"] and ns > (nb or 0) + 2, results["A_B"])
# where/how it was inserted: role and block of the request
blk = next((m for r in reqs if r["n"] == nb for m in r["messages"] if any("MSG-BOUNDARY-A" in b["text"] for b in m["blocks"])), None)
results["A_B"]["boundary_insert_role"] = blk and blk["role"]
results["A_B"]["boundary_insert_block"] = blk and next(b for b in blk["blocks"] if "MSG-BOUNDARY-A" in b["text"])["text"][:300]
check("A inserted as context (system reminder), not as the user's prompt text", blk is not None and "system-reminder" in results["A_B"]["boundary_insert_block"], results["A_B"]["boundary_insert_block"])
check("A evidence: inserted then read from the transcript", get(b1).get("stage") == "read", get(b1))
check("B evidence at SessionEnd: inserted (read needs a later assistant entry)", get(s1).get("stage") in ("inserted", "read"), get(s1))

if not a.skip_idle:
    # --- C: idle session (stream-json input kept open), a start message wakes it via asyncRewake ---
    n1 = len(requests())
    sj = spawn(base_argv + ["-p", "--input-format", "stream-json"], cwd=P["work"], env=cenv, stdin=subprocess.PIPE,
               stdout=open(os.path.join(root, "runC.jsonl"), "w"), stderr=open(os.path.join(root, "runC.err"), "w"), text=True)
    sj.stdin.write(json.dumps({"type": "user", "message": {"role": "user", "content": "first prompt, no task"}}) + "\n"); sj.stdin.flush()
    wait_for(lambda: len(requests()) > n1, 60)
    wait_for(lambda: any(s.get("source") == "Stop" for s in inbox_events("status")[-3:]), 30)
    time.sleep(1.5)  # idle; the waiter is armed
    n2 = len(requests())
    st = c.cmd("inbox.send", to="r7", mode="start", content=txt("MSG-START-C: please say hello"))["delivery"]
    woke = wait_for(lambda: first_request_with("MSG-START-C") is not None, 30)
    results["C_idle"] = {"requests_before": n2, "first_request_with_msg": first_request_with("MSG-START-C"), "record": get(st),
                         "hook_log_tail": open(os.path.join(P["hookstate"], "hook.log")).read().splitlines()[-6:]}
    check("C idle start wakes the session (asyncRewake) and reaches the model", woke, results["C_idle"])
    time.sleep(2)
    sj.stdin.close()
    try:
        sj.wait(30)
    except subprocess.TimeoutExpired:
        sj.kill()


# --- D: resume; an open claim with no transcript entry becomes unknown, never resent ---
sessions = sorted(glob.glob(os.path.join(P["cfg"], "projects", "*", "*.jsonl")), key=os.path.getmtime)
session_id = os.path.basename(sessions[0])[:-6] if sessions else None
# a claim taken by an earlier consumer that never reached the transcript (simulated crash after take)
d1 = c.cmd("inbox.send", to="r7", mode="boundary", content=txt("MSG-LOST-D"))["delivery"]
cons = json.load(open(glob.glob(os.path.join(P["hookstate"], f"{session_id}.json"))[0]))["consumer"] if session_id else None
c.cmd("inbox.take", op="sim-crash-take", agent="r7", consumer=cons, at="turn", limit=1)
n3 = len(requests())
spawn(base_argv + ["-p", "--resume", session_id, "resumed, no task"], cwd=P["work"], env=cenv,
      stdout=open(os.path.join(root, "runD.jsonl"), "w"), stderr=open(os.path.join(root, "runD.err"), "w")).wait(120)
results["D_resume"] = {"session": session_id, "record": get(d1), "ever_sent": first_request_with("MSG-LOST-D"),
                       "A_after_resume": get(b1).get("state")}
check("D resume: claim without transcript evidence -> unknown, never sent", get(d1)["state"] == "unknown" and first_request_with("MSG-LOST-D") is None, results["D_resume"])
check("D resume: delivered records keep their evidence", get(b1)["state"] == "delivered")

# --- S: status from hooks ---
st_events = inbox_events("status")
results["S_status"] = [(s["source"], s["state"]) for s in st_events][-30:]
check("S status working on UserPromptSubmit/PreToolUse, idle on Stop", {"working", "idle"} <= {s["state"] for s in st_events})

results["hook_log"] = open(os.path.join(P["hookstate"], "hook.log")).read().splitlines()[-40:]
json.dump(results, open(os.path.join(root, "results.json"), "w"), indent=1)
for p in procs:
    if p.poll() is None:
        p.terminate()
        try:
            p.wait(5)
        except subprocess.TimeoutExpired:
            p.kill()
print(f"{'FAILED' if fails else 'OK'}: {len(fails)} failures; root {root}")
sys.exit(1 if fails else 0)
