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
ap.add_argument("--inbox", choices=["fake", "k2", "k2core"], default="fake")
ap.add_argument("--coord-ext")
ap.add_argument("--framing-only", action="store_true", help="retain T15 native wire and actual renderer witnesses")
a = ap.parse_args()
HERE = os.path.dirname(os.path.abspath(__file__))
PLUGIN = os.path.join(HERE, "..", "integrations", "pi", "vqro-pull.ts")
root = a.root
sys.path.insert(0, HERE)
from coord_harness import Harness, txt, finish_framing  # noqa: E402
from textfiles import read_json, read_lines, read_text, write_json, write_text  # noqa: E402

P = {k: os.path.join(root, k) for k in ("home", "agent", "sessions", "work")}
for d in P.values():
    os.makedirs(d, exist_ok=True)
model_log = os.path.join(root, "model.jsonl")
port = 18830
results, fails = {"inbox": a.inbox}, []
h = Harness(root, a.vqro, inbox=a.inbox, coord_ext=a.coord_ext)
base, procs = h.env, h.procs
import atexit  # noqa: E402
atexit.register(h.stop)


def check(name, ok, detail=""):
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"  ({str(detail)[:500]})"), flush=True)
    results[name] = {"ok": bool(ok), "detail": None if ok else str(detail)[:1500]}
    if not ok:
        fails.append(name)


h.spawn([sys.executable, os.path.join(HERE, "fixtures", "fake_openai.py"), "--port", str(port), "--log", model_log])
AGENT = h.agent(json.loads(re.search(r"^export const PULL_DECLARATION = (.*);$", read_text(PLUGIN), re.M).group(1))["pull"])  # as the plugin declares
write_json(os.path.join(P["agent"], "models.json"), {"providers": {"fake": {"baseUrl": f"http://127.0.0.1:{port}/v1", "api": "openai-completions", "apiKey": "fake",
                                  "models": [{"id": "fake-1", "input": ["text"]}]}}})
write_json(os.path.join(P["agent"], "settings.json"), {"defaultProvider": "fake", "defaultModel": "fake-1", "retry": {"enabled": False}, "tuiMode": "regular"})
OPTIONAL = [f for f in ("--no-mcp",) if f in subprocess.run(["node", a.pi, "--help"], capture_output=True, text=True).stdout]
results["pi_version"] = subprocess.run(["node", a.pi, "--version"], capture_output=True, text=True).stdout.strip()


env = dict(base, TERM="xterm-256color", PI_CODING_AGENT_DIR=P["agent"], PI_OFFLINE="1", PI_SKIP_VERSION_CHECK="1",
           PI_TELEMETRY="0", VQRO_AGENT=AGENT, VQRO_PRINCIPAL=h.PLUGIN, VQRO_PULL_LOG=os.path.join(root, "plugin.jsonl"))
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
    return [json.loads(l) for l in read_lines(model_log)] if os.path.exists(model_log) else []


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
b = h.send(AGENT, "boundary", txt("MSG-TUI-BOUNDARY"))  # while tool 1 runs
DRAFT = "HUMAN-DRAFT keep me intact 123"
typed(DRAFT)
wait_for(lambda: len(requests()) >= 3, 40)
pump(2)
n_idle = len(requests())
s = h.send(AGENT, "start", txt("MSG-TUI-START"))
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
               n_idle=n_idle, submitted=last_user, b=h.get(b), s=h.get(s))
check("boundary in request 2 (after tool 1)", results["boundary_first"] == 2, results)
check("idle start wakes the TUI into the next request", results["start_first"] == n_idle + 1, results)
check("human draft submitted byte-identical and never mixed into earlier requests",
      last_user == DRAFT and not any(DRAFT in m["text"] for r in reqs[:-1] for m in r["messages"]), last_user)
check("messages rendered in the TUI with their delivery id", f"vqro inbox message {b}" in plain and f"vqro inbox message {s}" in plain)
check("both read", results["b"].get("state") == "read" and results["s"].get("state") == "read", (results["b"], results["s"]))
write_json(os.path.join(root, "tui-results.json"), results, indent=1, default=str)
if a.framing_only:
    with open(os.path.join(root, "framing-screen.txt"), "w") as output:
        output.write(plain)
    try: os.killpg(pid, 9)
    except ProcessLookupError: pass
    try: os.waitpid(pid, 0)
    except ChildProcessError: pass
    os.close(fd)
    finish_framing(h, results, fails)
os.kill(pid, 9); os.waitpid(pid, 0)
h.stop()
print(f"{'FAILED' if fails else 'OK'}: {len(fails)} failures ({results['pi_version']}); root {root}")
sys.exit(1 if fails else 0)
