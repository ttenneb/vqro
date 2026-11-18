"""Spike: drive Rex through its CLI (`rex ... --json`) and event stream."""
import json, os, subprocess, threading, time

REX = os.path.expanduser("~/Projects/bin/rex")
AGENT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "fake-agent.sh")
S = "primspike"


def log(*a):
    print(f"[{time.strftime('%H:%M:%S')}]", *a, flush=True)


def rex(*args, j=False):
    r = subprocess.run([REX, *args], capture_output=True, text=True, timeout=30)
    if r.returncode != 0:
        log("ERR", args, r.stderr.strip()[:300])
    return json.loads(r.stdout) if j and r.stdout.strip() else r.stdout


def main():
    subprocess.run([REX, "session", "kill", S], capture_output=True)
    created = rex("new", S, "--shell=none", "--json", "--", "bash", "--norc", j=True)
    sid = created["session_id"]
    shell = created["initial_windows"][0]["block_ids"][0]
    log("session", sid, "shell block", shell)

    events = []
    ev = subprocess.Popen([REX, "events", "-s", sid, "--json"], stdout=subprocess.PIPE, text=True)

    def listen():
        for line in ev.stdout:
            e = json.loads(line)
            events.append(e)
            inner = e.get("payload", {}).get("event", {}); log("EVENT", inner.get("type"), json.dumps(inner.get("payload", {}))[:200]) if inner.get("type") == "block_event" else None

    threading.Thread(target=listen, daemon=True).start()
    time.sleep(0.8)

    rex("send", "-s", sid, "-b", shell, "echo hi-from-vqro\n")
    time.sleep(1)
    out = rex("capture", "-s", sid, "-b", shell, "--trim")
    log("capture saw hi:", "hi-from-vqro" in out)

    sp = rex("split", "-s", sid, "-b", shell, "--shell=none", "--label", "fake-agent",
             "--focus=false", "--keep-open", "--json", "--", AGENT, j=True)
    agent = sp["block_ids"][0]
    log("agent block", agent)
    for _ in range(3):
        time.sleep(1.6)
        st = rex("block", "call", "-s", sid, "-b", agent, "program_status")
        log("program_status:", st.strip().replace("\n", " ")[:200])

    view = rex("api", "call", "-s", sid, "session.view")
    log("session.view (layout) excerpt:", view.strip().replace("\n", " ")[:300])

    time.sleep(1)
    rex("session", "kill", sid)
    time.sleep(0.5)
    ev.terminate()
    log("event types:", sorted({e.get("type") for e in events}))


if __name__ == "__main__":
    main()
