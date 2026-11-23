"""A3 interactive scenario: real Claude Code TUI in a PTY, fake Anthropic endpoint, private core + fake inbox.

  env -i HOME=$HOME PATH=$PATH unshare -rn sh -c 'ip link set lo up && exec python3 tests/claude_code_tui.py \
      --vqro target/debug/vqro --hook target/debug/vqro-claude-hook --claude /path/to/claude --root ROOT'

  I1  after a turn ends, the Stop asyncRewake waiter is armed; a `start` message wakes the idle session (exit 2) and
      reaches the model in the next request; evidence comes from the transcript entry the rewake creates;
  I2  the human's unsent draft in the editor is untouched by the wake and is later submitted byte-identical;
  I3  a waiter armed while idle is retired by a new prompt (nothing taken while busy).
"""

import sys
if sys.platform != "linux":
    print("SKIP claude_code_tui: requires Linux /proc and namespace/process probes")
    raise SystemExit(77)

import argparse, fcntl, glob, json, os, pty, re, select, socket, struct, subprocess, sys, termios, time
from pathlib import Path

ap = argparse.ArgumentParser()
for k in ("vqro", "hook", "claude", "root"):
    ap.add_argument("--" + k, required=True)
ap.add_argument("--inbox", choices=["fake", "k2", "k2core"], default="fake")
ap.add_argument("--coord-ext")
ap.add_argument("--framing-only", action="store_true", help="native rewake and human-prompt race witnesses only")
a = ap.parse_args()
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
from coord_harness import Harness, txt, finish_framing  # noqa: E402
from textfiles import read_json, read_lines, read_text, write_json, write_text  # noqa: E402

root = a.root
P = {k: os.path.join(root, k) for k in ("home", "cfg", "work", "hookstate")}
for d in P.values():
    os.makedirs(d, exist_ok=True)
model_log = os.path.join(root, "model.jsonl")
port = 18810
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


h.spawn([sys.executable, os.path.join(HERE, "fixtures", "fake_anthropic.py"), "--port", str(port), "--log", model_log])
AGENT = h.agent(json.loads(subprocess.run([a.hook, "declaration"], capture_output=True, text=True, check=True).stdout)["pull"])  # the launcher writes what the plugin declares
CTX = ["context"]
base = dict(h.env, HOME=P["home"], PATH=os.environ["PATH"])
settings = json.loads(subprocess.run([a.hook, "settings", os.path.abspath(a.hook)], capture_output=True, text=True).stdout)
write_json(os.path.join(P["cfg"], "settings.json"), settings, indent=1)
key = "sk-ant-fake-key-for-a3-tests-0123456789"
write_json(os.path.join(P["cfg"], ".claude.json"), {"hasCompletedOnboarding": True, "theme": "dark", "numStartups": 5,
           "customApiKeyResponses": {"approved": [key[-20:]], "rejected": []},
           "projects": {P["work"]: {"hasTrustDialogAccepted": True, "hasCompletedProjectOnboarding": True}}})
cenv = dict(base, CLAUDE_CONFIG_DIR=P["cfg"], VQRO_AGENT=AGENT, VQRO_PRINCIPAL=h.PLUGIN, VQRO_CLAUDE_STATE=P["hookstate"], DISABLE_TELEMETRY="1",
            DISABLE_ERROR_REPORTING="1", CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC="1", DISABLE_AUTOUPDATER="1",
            ANTHROPIC_BASE_URL=f"http://127.0.0.1:{port}", ANTHROPIC_API_KEY=key, TERM="xterm-256color")

pid, fd = pty.fork()
if pid == 0:
    os.chdir(P["work"])
    os.execve(a.claude, [a.claude, "--model", "claude-sonnet-4-5", "--allowedTools", "Bash"], cenv)
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
    return re.sub(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b\][^\x07]*\x07|\x1b[=>()][0-9A-Za-z]?", "", screen.decode("utf-8", "replace"))


def typed(text):
    for ch in text:
        os.write(fd, ch.encode()); pump(0.04)


def requests():
    return [json.loads(l) for l in read_lines(model_log)] if os.path.exists(model_log) else []


def first_request_with(text):
    return next((r["n"] for r in requests() if any(text in b["text"] for m in r["messages"] for b in m["blocks"])), None)


def hooklog():
    path = os.path.join(P["hookstate"], "hook.log")
    return [json.loads(l) for l in read_lines(path)] if os.path.exists(path) else []


def wait_for(pred, timeout=30):
    end = time.time() + timeout
    while time.time() < end:
        if pred():
            return True
        pump(0.1)
    return False


def last_user_text(r):
    users = [b["text"] for m in r["messages"] if m["role"] == "user" for b in m["blocks"] if b["type"] == "text"]
    return users[-1] if users else None


pump(6)
results["startup_screen_tail"] = plain()[-600:]
typed("steps:1 sleep:1 first task"); os.write(fd, b"\r")
started = wait_for(lambda: len(requests()) >= 2, 60)
check("TUI ran a first turn against the fake endpoint", started, plain()[-800:])
armed = wait_for(lambda: any(e["event"] == "waiter_armed" for e in hooklog()), 30)
check("Stop armed the asyncRewake waiter (interactive)", armed, [e["event"] for e in hooklog()][-10:])
pump(1.5)
# I2: the human starts a draft but does not submit it
DRAFT = "HUMAN DRAFT keep me 42"
typed(DRAFT)
pump(0.5)
n_before = len(requests())
wake_mode = "boundary" if a.framing_only else "settle"
d1 = h.send(AGENT, wake_mode, txt("MSG-WAKE-I1: a teammate says the build is green"), render=CTX)
woke = wait_for(lambda: first_request_with("MSG-WAKE-I1") is not None, 40)
nreq = first_request_with("MSG-WAKE-I1")
check("I1 a safe message wakes approximate idle without serving start", woke and nreq == n_before + 1, {"n_before": n_before, "first": nreq})
r = next((x for x in requests() if x["n"] == nreq), None)
blk = r and next((b["text"] for m in r["messages"] for b in m["blocks"] if "MSG-WAKE-I1" in b["text"]), None)
results["I1_insert_block"] = blk and blk[:400]
results["I1_draft_in_wake_request"] = bool(r and any(DRAFT in b["text"] for m in r["messages"] for b in m["blocks"]))
check("I1 wake request does not contain the human draft", r is not None and not results["I1_draft_in_wake_request"])
wait_for(lambda: len([e for e in hooklog() if e["event"] == "waiter_armed"]) >= 2, 30)
pump(1.0)
tx = sorted(glob.glob(os.path.join(P["cfg"], "projects", "*", "*.jsonl")), key=os.path.getmtime)
rows = [json.loads(l) for l in read_lines(tx[-1])] if tx else []
kinds = [(x.get("type"), (x.get("attachment") or {}).get("type")) for x in rows if "MSG-WAKE-I1" in json.dumps(x)]
results["I1_transcript_kinds"] = kinds
# I2: submit the draft now; it must arrive unchanged
n_before2 = len(requests())
os.write(fd, b"\r")
wait_for(lambda: len(requests()) > n_before2, 30)
sub = next((x for x in requests() if x["n"] == n_before2 + 1), None)
results["I2_submitted"] = sub and last_user_text(sub)
check("I2 the human draft is submitted byte-identical after the wake", results["I2_submitted"] == DRAFT, results["I2_submitted"])
# Evidence is advanced when a later hook fires (the transcript is written asynchronously).
results["I1_record"] = h.get(d1)
check("I1 evidence: the woken message reaches read from the transcript (by the next hooks)", results["I1_record"].get("state") == "read", results["I1_record"])
wait_for(lambda: len([e for e in hooklog() if e["event"] == "waiter_armed"]) >= 3, 30)
pump(1.0)
# I3: an armed waiter is retired by a new prompt; a message sent while busy is not taken by the waiter
prompts = sum(1 for e in hooklog() if e["event"] == "hook" and e["detail"].get("hook") == "UserPromptSubmit")
typed("steps:1 sleep:3 second task"); os.write(fd, b"\r")
# The idle point is approximate with hooks: a take can race a prompt being submitted (reported). Send only after
# Claude Code ran UserPromptSubmit for this prompt.
wait_for(lambda: sum(1 for e in hooklog() if e["event"] == "hook" and e["detail"].get("hook") == "UserPromptSubmit") > prompts, 15)
wait_for(lambda: any(e["event"] == "waiter_retired" for e in hooklog()), 15)
d2 = h.send(AGENT, "start" if a.framing_only else "settle", txt("MSG-BUSY-I3"), render=CTX)
pump(1.0)
state_busy = h.get(d2)["state"]
check("I3 a new prompt retires the waiter; a settle message stays held while busy", state_busy in ("held", "recorded")
      and any(e["event"] == "waiter_retired" for e in hooklog()), {"state": state_busy, "events": [e["event"] for e in hooklog()][-8:]})
if not a.framing_only:
    wait_for(lambda: first_request_with("MSG-BUSY-I3") is not None, 40)
    check("I3 the held settle message is delivered at the next settle or idle point", first_request_with("MSG-BUSY-I3") is not None)
else:
    wait_for(lambda: len([e for e in hooklog() if e["event"] == "waiter_armed"]) >= 4, 40)
    unsafe = h.get(d2)
    check("I3 approximate idle never claims an assumed-idle start", first_request_with("MSG-BUSY-I3") is None
          and unsafe.get("state") in ("held", "recorded") and not unsafe.get("attempt"), unsafe)

if a.framing_only:
    # Actually race a pending boundary hint with submission of a prepared human
    # prompt. Typing happens before the barrier; Enter and the public send race.
    import threading
    barrier = threading.Barrier(2)
    raced = {}
    def send_during_prompt():
        try:
            barrier.wait(timeout=5)
            raced["delivery"] = h.send(AGENT, "boundary", txt("MSG-RACE-I3"), render=CTX)
        except BaseException as error:
            raced["error"] = repr(error)
    typed("steps:1 sleep:2 T15 human prompt race")
    thread = threading.Thread(target=send_during_prompt)
    thread.start(); barrier.wait(timeout=5)
    os.write(fd, b"\r")
    thread.join(10)
    check("I3 boundary send actually races human Enter", not thread.is_alive() and "delivery" in raced, raced)
    arrived = wait_for(lambda: first_request_with("MSG-RACE-I3") is not None, 40)
    race_record = h.get(raced["delivery"]) if "delivery" in raced else {}
    unsafe = h.get(d2)
    check("I3 race preserves boundary mode; approximate idle never claims start",
          arrived and race_record.get("mode") == "boundary" and not unsafe.get("attempt")
          and first_request_with("MSG-BUSY-I3") is None,
          {"boundary": race_record, "unsafe_start": unsafe})
    results["approximate_idle_race"] = {"boundary": race_record, "unsafe_start": unsafe,
        "model_request": first_request_with("MSG-RACE-I3"), "send": raced,
        "human_prompt": "T15 human prompt race"}
    results["screen_tail"] = plain()[-8192:]
    with open(os.path.join(root, "framing-screen.txt"), "w") as output:
        output.write(plain())
    # pty.fork made this exact child the session/process-group leader. Stop only
    # that private group (including its hook waiters), and reap the direct child.
    try: os.killpg(pid, signal.SIGKILL)
    except ProcessLookupError: pass
    try: os.waitpid(pid, 0)
    except ChildProcessError: pass
    os.close(fd)
    finish_framing(h, results, fails)

# --- I4/I5: Claude Code's native per-session inbox socket as an `admit` route (wire format from the 2.1.329 binary:
# an optional auth line, then {"type":"user","message":{...}}). vqro cannot edit or withdraw after posting. ---
wait_for(lambda: len([e for e in hooklog() if e["event"] == "waiter_armed"]) >= 4, 40)
pump(1.0)
socks = [e["detail"].get("messaging_socket") for e in hooklog() if e["event"] == "session" and e["detail"].get("messaging_socket")]
results["native_socket"] = socks[-1] if socks else None


def post(text):
    s = socket.socket(socket.AF_UNIX); s.connect(socks[-1]); s.settimeout(3)
    s.sendall((json.dumps({"type": "user", "message": {"role": "user", "content": text}}) + "\n").encode())
    try:
        reply = s.recv(4096).decode()
    except OSError as e:
        reply = f"<{e}>"
    s.close()
    return reply


if socks:
    n_idle = len(requests())
    results["I4_reply"] = post("MSG-NATIVE-IDLE: posted to the session socket")
    wait_for(lambda: first_request_with("MSG-NATIVE-IDLE") is not None, 30)
    results["I4_first_request"] = first_request_with("MSG-NATIVE-IDLE")
    r = next((x for x in requests() if x["n"] == results["I4_first_request"]), None)
    results["I4_block"] = r and next((b["text"] for m in r["messages"] for b in m["blocks"] if "MSG-NATIVE-IDLE" in b["text"]), None)[:500]
    check("I4 native socket while idle starts a turn (admit route)", results["I4_first_request"] == n_idle + 1, results.get("I4_reply"))
    pump(3)
    typed("steps:2 sleep:3 third task"); os.write(fd, b"\r")
    wait_for(lambda: any("third task" in b["text"] for r in requests() for m in r["messages"] for b in m["blocks"]), 30)
    n_busy = len(requests())
    results["I5_reply"] = post("MSG-NATIVE-BUSY: posted while a tool runs")
    wait_for(lambda: first_request_with("MSG-NATIVE-BUSY") is not None, 40)
    results["I5_first_request"] = first_request_with("MSG-NATIVE-BUSY")
    results["I5_requests_when_posted"] = n_busy
    r = next((x for x in requests() if x["n"] == results["I5_first_request"]), None)
    results["I5_block"] = r and next((b["type"] + ": " + b["text"] for m in r["messages"] for b in m["blocks"] if "MSG-NATIVE-BUSY" in b["text"]), None)[:500]
    check("I5 native socket while busy is read at the next request (between tool calls)", results["I5_first_request"] == n_busy + 1, results)
    tx = sorted(glob.glob(os.path.join(P["cfg"], "projects", "*", "*.jsonl")), key=os.path.getmtime)
    rows = [json.loads(l) for l in read_lines(tx[-1])] if tx else []
    results["I4_I5_transcript_kinds"] = [(x.get("type"), (x.get("attachment") or {}).get("type"), x.get("operation")) for x in rows if "MSG-NATIVE" in json.dumps(x)]

# I6: /clear. Does Claude Code keep the session id? Either way the hook keeps its consumer (not a restart).
starts = lambda: [e for e in hooklog() if e["event"] == "hook" and e["detail"].get("hook") == "SessionStart"]
n_starts = len(starts())
consumers_before = {e["detail"].get("consumer") for e in hooklog() if e["event"] == "session"}
pump(2)
typed("/clear"); pump(0.5); os.write(fd, b"\r")
cleared = wait_for(lambda: len(starts()) > n_starts, 30)
pump(2)
first_session = starts()[0]["session"] if starts() else None
after = starts()[-1]["session"] if cleared else None
results["I6_clear_session"] = {"before": first_session, "after": after, "same": first_session == after}
print(f"     /clear: session id {'kept' if first_session == after else 'changed'} ({first_session} -> {after})")
new_consumers = {e["detail"].get("consumer") for e in hooklog() if e["event"] == "session"} - consumers_before
check("I6 /clear runs SessionStart and keeps the consumer (no reconcile as a restart)",
      cleared and not new_consumers and any(e["event"] == "session_continued" for e in hooklog()),
      {"clear": results["I6_clear_session"], "new_consumers": list(new_consumers), "tail": [e["event"] for e in hooklog()][-8:]})
# ...and mail still flows into the cleared session (the next prompt's tool batch takes a boundary message)
after_clear = h.send(AGENT, "boundary", txt("MSG-AFTER-CLEAR"), render=["context"])
typed("steps:1 sleep:1 after clear"); os.write(fd, b"\r")
check("I6 after /clear a boundary message is handed over in the new session", wait_for(lambda: first_request_with("MSG-AFTER-CLEAR") is not None, 40),
      {"record": h.get(after_clear), "tail": [e["event"] for e in hooklog()][-8:]})
results["I6_after_clear"] = h.get(after_clear)

results["hook_log"] = [f"{e['event']} {json.dumps(e['detail'])[:120]}" for e in hooklog()][-40:]
results["screen_tail"] = plain()[-1500:]
write_json(os.path.join(root, "tui-results.json"), results, indent=1)
try:
    os.kill(pid, 9); os.waitpid(pid, 0)
except OSError:
    pass


def waiters():
    out = subprocess.run(["pgrep", "-f", a.hook + " wait"], capture_output=True, text=True).stdout.split()
    mine = []
    for w in out:
        try:
            if P["hookstate"] in Path(f"/proc/{w}/environ").read_bytes().decode("utf-8", "replace"):
                mine.append(w)
        except OSError:
            pass
    return mine


end = time.time() + 15
while waiters() and time.time() < end:
    time.sleep(0.5)
left = waiters()
check("no idle waiter outlives a killed Claude Code", not left, left)
for w in left:
    os.kill(int(w), 9)
h.stop()
print(f"{'FAILED' if fails else 'OK'}: {len(fails)} failures; root {root}")
sys.exit(1 if fails else 0)
