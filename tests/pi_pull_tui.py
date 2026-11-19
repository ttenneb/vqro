"""Tier-1 Pi pull plugin in an interactive Pi (PTY): the human has an unsent draft while a boundary message is pulled
and an idle start message wakes Pi. Pass = each message reaches the model at its point, rendered in the TUI, and the
draft, submitted later, arrives byte-identical. Fake model, private core and fake inbox, private netns.

  env -i HOME=$HOME PATH=$PATH unshare -rn sh -c 'ip link set lo up && exec python3 tests/pi_pull_tui.py \
      --vqro target/debug/vqro --pi /path/to/pi/dist/bundle/cli.js --root ROOT'
"""
import argparse, fcntl, json, os, pty, re, select, socket, struct, subprocess, sys, termios, time

ap = argparse.ArgumentParser()
for k in ("vqro", "pi", "root"):
    ap.add_argument("--" + k, required=True)
a = ap.parse_args()
HERE = os.path.dirname(os.path.abspath(__file__))
PLUGIN = os.path.join(HERE, "..", "integrations", "pi", "vqro-pull.ts")
root = a.root
P = {k: os.path.join(root, k) for k in ("home", "agent", "sessions", "work", "xrun", "st", "noext")}
for d in P.values():
    os.makedirs(d, exist_ok=True)
sock, model_log, inbox_log = (os.path.join(root, n) for n in ("v.sock", "model.jsonl", "inbox.jsonl"))
port = 18830
base = {"HOME": P["home"], "PATH": os.environ["PATH"], "XDG_RUNTIME_DIR": P["xrun"], "VQRO_STATE_DIR": P["st"],
        "VQRO_SOCKET": sock, "VQRO_EXTENSIONS_DIR": P["noext"], "VQRO_HERDR": '["/bin/false"]',
        "HC_SOCKET": os.path.join(root, "h.sock"), "HC_SESSION": "pi-tui"}
procs, results, fails = [], {}, []


def txt(s):
    return [{"type": "text", "text": s}]


def check(name, ok, detail=""):
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"  ({detail})"), flush=True)
    results[name] = {"ok": bool(ok), "detail": None if ok else detail}
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
spawn([sys.executable, os.path.join(HERE, "fixtures", "fake_openai.py"), "--port", str(port), "--log", model_log])
json.dump({"providers": {"fake": {"baseUrl": f"http://127.0.0.1:{port}/v1", "api": "openai-completions", "apiKey": "fake",
                                  "models": [{"id": "fake-1", "input": ["text"]}]}}}, open(os.path.join(P["agent"], "models.json"), "w"))
json.dump({"defaultProvider": "fake", "defaultModel": "fake-1", "retry": {"enabled": False}, "tuiMode": "regular"},
          open(os.path.join(P["agent"], "settings.json"), "w"))
OPTIONAL = [f for f in ("--no-mcp",) if f in subprocess.run(["node", a.pi, "--help"], capture_output=True, text=True).stdout]
results["pi_version"] = subprocess.run(["node", a.pi, "--version"], capture_output=True, text=True).stdout.strip()


class Client:
    def __init__(self):
        self.s = socket.socket(socket.AF_UNIX); self.s.connect(sock); self.f = self.s.makefile("rw"); self.n = 0
        self.call("hello", {"client": "pi-tui"})

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
env = dict(base, TERM="xterm-256color", PI_CODING_AGENT_DIR=P["agent"], PI_OFFLINE="1", PI_SKIP_VERSION_CHECK="1",
           PI_TELEMETRY="0", VQRO_AGENT="r7", VQRO_PULL_LOG=os.path.join(root, "plugin.jsonl"))
pid, fd = pty.fork()
if pid == 0:
    os.chdir(P["work"])
    os.execvpe("node", ["node", a.pi, "-ne", "-e", PLUGIN, "--no-skills", "--no-context-files", "--no-prompt-templates",
                        *OPTIONAL, "--session-dir", P["sessions"], "--tools", "bash"], env)
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 40, 120, 0, 0))
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


def typed(text):
    for ch in text:
        os.write(fd, ch.encode()); pump(0.03)


def requests():
    return [json.loads(l) for l in open(model_log)] if os.path.exists(model_log) else []


def first_request_with(text):
    return next((r["n"] for r in requests() if any(text in m["text"] for m in r["messages"])), None)


def wait_for(pred, timeout=30):
    end = time.time() + timeout
    while time.time() < end:
        if pred():
            return True
        pump(0.1)
    return False


pump(4)
typed("steps:2 sleep:2.5 tui-task"); os.write(fd, b"\r")
wait_for(lambda: len(requests()) >= 1)
pump(0.5)
DRAFT = "HUMAN-DRAFT keep me intact 123"
typed(DRAFT)
b = c.cmd("inbox.send", to="r7", mode="boundary", content=txt("MSG-TUI-BOUNDARY"))["delivery"]
wait_for(lambda: len(requests()) >= 3, 40)
pump(2)
n_idle = len(requests())
s = c.cmd("inbox.send", to="r7", mode="start", content=txt("MSG-TUI-START"))["delivery"]
wait_for(lambda: first_request_with("MSG-TUI-START") is not None, 30)
pump(2)
n_before_submit = len(requests())
os.write(fd, b"\r")
wait_for(lambda: len(requests()) > n_before_submit, 20)
pump(1)
reqs = requests()
last_user = [m["text"] for m in reqs[-1]["messages"] if m["role"] == "user"][-1] if reqs else None
plain = re.sub(r"\x1b\[[0-9;?]*[ -/]*[@-~]", "", screen.decode("utf-8", "replace"))
results.update(boundary_first=first_request_with("MSG-TUI-BOUNDARY"), start_first=first_request_with("MSG-TUI-START"),
               n_idle=n_idle, submitted=last_user, b=c.cmd("inbox.get", delivery=b), s=c.cmd("inbox.get", delivery=s))
check("boundary in request 2 (after tool 1)", results["boundary_first"] == 2, results)
check("idle start wakes the TUI into the next request", results["start_first"] == n_idle + 1, results)
check("human draft submitted byte-identical and never mixed into earlier requests",
      last_user == DRAFT and not any(DRAFT in m["text"] for r in reqs[:-1] for m in r["messages"]), last_user)
check("messages rendered in the TUI with their delivery id", f"vqro message {b}" in plain and f"vqro message {s}" in plain)
check("both read", results["b"].get("stage") == "read" and results["s"].get("stage") == "read", (results["b"], results["s"]))
json.dump(results, open(os.path.join(root, "tui-results.json"), "w"), indent=1, default=str)
os.kill(pid, 9); os.waitpid(pid, 0)
for p in procs:
    p.kill(); p.wait()
print(f"{'FAILED' if fails else 'OK'}: {len(fails)} failures ({results['pi_version']}); root {root}")
sys.exit(1 if fails else 0)
