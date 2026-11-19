"""Tier-1 Pi pull plugin (integrations/pi/vqro-pull.ts) end to end: real Pi in RPC mode, a fake OpenAI-compatible
model, a private vqro core and the fake inbox served through command.invoke (coordination-v0 §7). No network, no
spend. The fake model's request log is the oracle for *when* a message reached the model.

  env -i HOME=$HOME PATH=$PATH unshare -rn sh -c 'ip link set lo up && exec python3 tests/pi_pull_e2e.py \
      --vqro target/debug/vqro --pi /path/to/pi/dist/bundle/cli.js --root ROOT'

A boundary -> exactly the request after the tool batch; B settle -> only the continuation after the final answer;
D edit while held reaches the model, edit after take is too_late; C idle start wakes Pi; E crash after take ->
unknown, never resent; F lost report -> reconciled inserted from the session; G abort after take -> inserted, read
only by the next answered request; I image block inserted as an image; U a block Pi cannot insert stays held.
"""
import argparse, json, os, socket, subprocess, sys, tempfile, time

ap = argparse.ArgumentParser()
ap.add_argument("--vqro", required=True)
ap.add_argument("--pi", required=True, help="Pi CLI entry (dist/bundle/cli.js)")
ap.add_argument("--root")
a = ap.parse_args()
HERE = os.path.dirname(os.path.abspath(__file__))
PLUGIN = os.path.join(HERE, "..", "integrations", "pi", "vqro-pull.ts")
root = a.root or tempfile.mkdtemp(prefix="pi-pull-")
P = {k: os.path.join(root, k) for k in ("home", "agent", "sessions", "work", "xrun", "st", "noext")}
for d in P.values():
    os.makedirs(d, exist_ok=True)
sock, model_log, inbox_log, plugin_log = (os.path.join(root, n) for n in ("v.sock", "model.jsonl", "inbox.jsonl", "plugin.jsonl"))
port = 18820
base = {"HOME": P["home"], "PATH": os.environ["PATH"], "XDG_RUNTIME_DIR": P["xrun"], "VQRO_STATE_DIR": P["st"],
        "VQRO_SOCKET": sock, "VQRO_EXTENSIONS_DIR": P["noext"], "VQRO_HERDR": '["/bin/false"]',
        "HC_SOCKET": os.path.join(root, "h.sock"), "HC_SESSION": "pi-pull"}
results, fails = {}, []


def txt(s):
    return [{"type": "text", "text": s}]


procs = []


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
    results[name] = {"ok": bool(ok), "detail": None if ok else detail}
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
spawn([sys.executable, os.path.join(HERE, "fixtures", "fake_openai.py"), "--port", str(port), "--log", model_log])
json.dump({"providers": {"fake": {"baseUrl": f"http://127.0.0.1:{port}/v1", "api": "openai-completions", "apiKey": "fake",
                                  "models": [{"id": "fake-1", "input": ["text", "image"]}]}}},
          open(os.path.join(P["agent"], "models.json"), "w"))
json.dump({"defaultProvider": "fake", "defaultModel": "fake-1", "retry": {"enabled": False}},
          open(os.path.join(P["agent"], "settings.json"), "w"))
HELP = subprocess.run(["node", a.pi, "--help"], capture_output=True, text=True).stdout
OPTIONAL = [f for f in ("--no-mcp",) if f in HELP]
results["pi_version"] = subprocess.run(["node", a.pi, "--version"], capture_output=True, text=True).stdout.strip()
print("pi", results["pi_version"], flush=True)


class Client:
    def __init__(self):
        self.s = socket.socket(socket.AF_UNIX); self.s.connect(sock); self.f = self.s.makefile("rw"); self.n = 0
        self.call("hello", {"client": "pi-pull-e2e"})

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


class Pi:
    def __init__(self, fault="", resume=False):
        env = dict(base, PI_CODING_AGENT_DIR=P["agent"], PI_OFFLINE="1", PI_SKIP_VERSION_CHECK="1", PI_TELEMETRY="0",
                   VQRO_AGENT="r42", VQRO_PULL_LOG=plugin_log, VQRO_PULL_FAULT=fault)
        argv = ["node", a.pi, "--mode", "rpc", "-ne", "-e", PLUGIN, "--no-skills", "--no-context-files",
                "--no-prompt-templates", *OPTIONAL, "--session-dir", P["sessions"], "--tools", "bash"] + (["-c"] if resume else [])
        self.p = spawn(argv, cwd=P["work"], env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                       stderr=open(os.path.join(root, "pi-stderr.log"), "a"), text=True, bufsize=1)

    def send(self, record):
        self.p.stdin.write(json.dumps(record) + "\n"); self.p.stdin.flush()

    def until(self, pred, timeout=60):
        end = time.time() + timeout
        while time.time() < end:
            line = self.p.stdout.readline()
            if not line:
                raise RuntimeError(f"pi exited ({self.p.poll()}); see pi-stderr.log")
            ev = json.loads(line)
            if pred(ev):
                return ev
        raise TimeoutError("event not seen")

    def close(self):
        try:
            self.p.stdin.close(); self.p.wait(5)
        except Exception:
            self.p.kill(); self.p.wait()


def requests():
    return [json.loads(l) for l in open(model_log)] if os.path.exists(model_log) else []


def first_request_with(text):
    return next((r["n"] for r in requests() if any(text in m["text"] for m in r["messages"])), None)


def get(d):
    return c.cmd("inbox.get", delivery=d)


def plugin_events(kind):
    return [json.loads(l) for l in open(plugin_log) if f'"event":"{kind}"' in l] if os.path.exists(plugin_log) else []


def wait_for(pred, timeout=20):
    end = time.time() + timeout
    while time.time() < end:
        if pred():
            return True
        time.sleep(0.05)
    return False


settled = lambda e: e.get("type") == "agent_settled"

# --- A, B, D: boundary, settle, edit while held; sent during tool 1 of a 3-step run ---
pi = Pi()
wait_for(lambda: plugin_events("reconciled"), 30)
pi.send({"id": "p1", "type": "prompt", "message": "steps:3 sleep:1.5 begin"})
pi.until(lambda e: e.get("type") == "tool_execution_start")
n_at = len(requests())
a1 = c.cmd("inbox.send", to="r42", mode="boundary", content=txt("MSG-BOUNDARY-A"))["delivery"]
b1 = c.cmd("inbox.send", to="r42", mode="settle", content=txt("MSG-SETTLE-B"))["delivery"]
d1 = c.cmd("inbox.send", to="r42", mode="settle", content=txt("MSG-EDIT-D original"))["delivery"]
c.cmd("inbox.edit", delivery=d1, content=txt("MSG-EDIT-D edited"))
pi.until(settled)
wait_for(lambda: get(b1).get("stage") == "read", 5)
na, nb = first_request_with("MSG-BOUNDARY-A"), first_request_with("MSG-SETTLE-B")
results["A_B"] = {"requests_at_send": n_at, "boundary": na, "settle": nb, "total": len(requests()), "a": get(a1), "b": get(b1)}
check("A boundary in exactly the request after the tool batch", n_at == 1 and na == 2, results["A_B"])
check("A evidence inserted -> read", get(a1).get("stage") == "read" and (get(a1).get("evidence") or {}) is not None, get(a1))
check("B settle only in the continuation after the final answer", nb == len(requests()) and nb > 4, results["A_B"])
check("B evidence read", get(b1).get("stage") == "read", get(b1))
check("D the model saw only the edited text", first_request_with("MSG-EDIT-D original") is None and first_request_with("MSG-EDIT-D edited") == nb)
try:
    c.cmd("inbox.edit", delivery=a1, content=txt("late"))
    late = "accepted (BAD)"
except RuntimeError as e:
    late = str(e)
check("D edit after take is too_late", "too_late" in late, late)

# --- C: idle start; I: image block; U: a block Pi cannot insert stays held ---
time.sleep(0.5)
n_before = len(requests())
c1 = c.cmd("inbox.send", to="r42", mode="start", content=txt("MSG-START-C") + [{"type": "image", "mime": "image/png",
           "data": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg=="}])["delivery"]
pi.until(settled)
wait_for(lambda: get(c1).get("stage") == "read", 5)
nc = first_request_with("MSG-START-C")
r = next((x for x in requests() if x["n"] == nc), None)
kinds = next((m.get("kinds") for m in (r or {}).get("messages", []) if "MSG-START-C" in m["text"]), None)
results["C_I"] = {"requests_before": n_before, "first": nc, "kinds": kinds, "record": get(c1)}
check("C idle start wakes Pi and reaches the next request", nc == n_before + 1, results["C_I"])
check("I image block reaches the model as an image", kinds is not None and "image_url" in kinds, kinds)
u1 = c.cmd("inbox.send", to="r42", mode="start", content=[{"type": "audio", "mime": "audio/wav", "data": "UklGRg=="}])["delivery"]
time.sleep(1.5)
check("U audio stays held (unsupported_content), never flattened", get(u1)["state"] == "held" and get(u1).get("reason") == "unsupported_content", get(u1))
c.cmd("inbox.edit", delivery=u1, content=txt("MSG-U now text"))
pi.until(settled)
pi.close()

# --- E: crash after a durable take -> unknown on restart, never resent ---
pi = Pi(fault="crash_after_take", resume=True)
wait_for(lambda: len(plugin_events("reconciled")) >= 2, 30)
e1 = c.cmd("inbox.send", to="r42", mode="start", content=txt("MSG-CRASH-E"))["delivery"]
pi.p.wait(20)
crashed = get(e1)["state"]
pi = Pi(resume=True)
wait_for(lambda: len(plugin_events("reconciled")) >= 4, 30)
time.sleep(1)
results["E"] = {"exit": procs[-2].returncode, "after_crash": crashed, "after_restart": get(e1)}
check("E crash after take -> unknown, never sent", crashed == "claimed" and get(e1)["state"] == "unknown" and first_request_with("MSG-CRASH-E") is None, results["E"])
pi.close()

# --- F: inserted but the report is lost -> reconciled inserted from the session ---
pi = Pi(fault="drop_report", resume=True)
wait_for(lambda: len(plugin_events("reconciled")) >= 5, 30)
f1 = c.cmd("inbox.send", to="r42", mode="start", content=txt("MSG-LOSTREPORT-F"))["delivery"]
pi.until(settled)
before = get(f1)["state"]
pi.close()
pi = Pi(resume=True)
wait_for(lambda: len(plugin_events("reconciled")) >= 6, 30)
time.sleep(1)
results["F"] = {"before": before, "after": get(f1), "seen": first_request_with("MSG-LOSTREPORT-F")}
check("F lost report -> reconciled inserted from the session entry", before == "claimed" and get(f1).get("stage") == "inserted"
      and (get(f1).get("evidence") or {}).get("entry"), results["F"])
pi.close()

# --- G: abort right after a boundary take -> inserted, unread until the next answered request ---
pi = Pi(fault="abort_after_take", resume=True)
wait_for(lambda: len(plugin_events("reconciled")) >= 7, 30)
pi.send({"id": "g1", "type": "prompt", "message": "steps:3 sleep:1.5 run-G"})
pi.until(lambda e: e.get("type") == "tool_execution_start")
g1 = c.cmd("inbox.send", to="r42", mode="boundary", content=txt("MSG-ABORT-G"))["delivery"]
pi.until(settled)
time.sleep(0.5)
after_abort = (get(g1).get("stage"), first_request_with("MSG-ABORT-G"))
pi.send({"id": "g2", "type": "prompt", "message": "next prompt after abort"})
pi.until(settled)
wait_for(lambda: get(g1).get("stage") == "read", 5)
results["G"] = {"after_abort": after_abort, "after_next": get(g1).get("stage"), "first_request": first_request_with("MSG-ABORT-G")}
check("G abort after take: inserted but unread, never sent", after_abort == ("inserted", None), results["G"])
check("G read only after the next answered request", get(g1).get("stage") == "read" and first_request_with("MSG-ABORT-G") is not None, results["G"])
pi.close()

calls = [json.loads(l) for l in open(inbox_log) if '"event": "take"' in l]
results["takes"] = len(calls)
json.dump(results, open(os.path.join(root, "results.json"), "w"), indent=1)
for p in procs:
    if p.poll() is None:
        p.terminate()
        try:
            p.wait(5)
        except subprocess.TimeoutExpired:
            p.kill()
print(f"{'FAILED' if fails else 'OK'}: {len(fails)} failures; {len(calls)} take calls; root {root}")
sys.exit(1 if fails else 0)
