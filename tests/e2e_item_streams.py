#!/usr/bin/env python3
"""Item streams end to end (agent-v0 §4; P3 regression gate): a private vqro core (--fake, no extensions), the
fake item runtime (src/bin/vqro-fake-items.rs) and 3 checking viewers (+1 stalled), on CPUs `--cpus`.
Timeline: steady; a stalled viewer holding every stream; a slow viewer (SIGSTOP); a session switch on 50 agents;
a runtime SIGKILL + restart; then pause and compare every viewer's reconstruction with the runtime's digests.
Exits 1 unless every viewer is identical to the runtime for every agent with zero integrity errors.
Usage: e2e_item_streams.py OUT [--agents 200] [--rate 2] [--timescale 1] [--cpus 8-15]"""
import json, os, shutil, signal, socket, subprocess, sys, tempfile, threading, time
from pathlib import Path

OUT = Path(sys.argv[1]); OUT.mkdir(parents=True, exist_ok=True)
def arg(n, d): return sys.argv[sys.argv.index(n) + 1] if n in sys.argv else d
AGENTS, RATE, TEXT = int(arg("--agents", "200")), arg("--rate", "2"), arg("--text", "160")
SCALE = float(arg("--timescale", "1"))  # shrink the timeline for smoke runs
BIN = Path(os.environ.get("CARGO_TARGET_DIR", str(Path(__file__).resolve().parents[2] / "target"))) / "release"
CPUS = arg("--cpus", "")  # pin with taskset only when asked (measurements: 8-15); the gate inherits its own set
R = Path(tempfile.mkdtemp(prefix="p3", dir="/tmp"))
for d in ("home", "st", "ext", "run"): (R / d).mkdir()
(R / "run").chmod(0o700)
(R / "ext/defaults.json").write_text('{"version":4,"extensions":[]}')
ENV = {"HOME": str(R / "home"), "PATH": "/usr/bin:/bin", "VQRO_STATE_DIR": str(R / "st"), "VQRO_SOCKET": str(R / "v.sock"),
       "VQRO_EXTENSIONS_DIR": str(R / "ext"), "XDG_RUNTIME_DIR": str(R / "run"), "HC_SOCKET": str(R / "h.sock"),
       "HC_SESSION": "p3", "VQRO_HERDR": '["/bin/false"]'}
LOG = open(OUT / "timeline.log", "a", buffering=1)
T0 = time.monotonic()
def log(*a): LOG.write(f"{time.monotonic()-T0:7.1f}s {' '.join(map(str, a))}\n")
procs = {}
RESULT = [["did not finish"]]
def start(name, argv, out):
    procs[name] = subprocess.Popen([*(["taskset", "-c", CPUS] if CPUS else []), *map(str, argv)], env=ENV, stdout=open(out, "ab"), stderr=subprocess.STDOUT)
    log("start", name, procs[name].pid)

def rpc(method, params):
    s = socket.socket(socket.AF_UNIX); s.connect(str(R / "v.sock")); f = s.makefile("rb")
    out = None
    for i, (m, p) in enumerate([("hello", {"client": "p3-control"}), (method, params)], 1):
        s.sendall((json.dumps({"jsonrpc": "2.0", "id": i, "method": m, "params": p}) + "\n").encode())
        while True:
            msg = json.loads(f.readline())
            if msg.get("id") == i: out = msg; break
    s.close()
    return out

def pinfo(pid):
    try:
        st = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
        rss = int(open(f"/proc/{pid}/statm").read().split()[1]) * os.sysconf("SC_PAGE_SIZE") / 2**20
        return (int(st[11]) + int(st[12])) / os.sysconf("SC_CLK_TCK"), rss, len(os.listdir(f"/proc/{pid}/task"))
    except OSError:
        return None

PHASE = ["setup"]
def sampler():
    prev = {}
    with open(OUT / "samples.jsonl", "a") as f:
        while not STOP.is_set():
            row = {"t": round(time.monotonic() - T0, 1), "phase": PHASE[0]}
            for n, p in list(procs.items()):
                i = pinfo(p.pid)
                if i is None: continue
                cpu, rss, thr = i
                d = cpu - prev.get(n, (cpu, 0))[0]; dt = time.monotonic() - prev.get(n, (cpu, time.monotonic() - 5))[1]
                prev[n] = (cpu, time.monotonic())
                row[n] = {"cpu_pct": round(100 * d / max(dt, 0.1), 1), "rss_mb": round(rss, 1), "threads": thr}
            f.write(json.dumps(row) + "\n"); f.flush()
            STOP.wait(5)
STOP = threading.Event()

def at(t):
    t *= SCALE
    while time.monotonic() - T0 < t: time.sleep(0.2)

def provider_argv():
    return [BIN / "vqro-fake-items", "--agents", AGENTS, "--rate", RATE, "--text", TEXT]

try:
    start("core", [BIN / "vqro", "serve", "--fake"], OUT / "core.log")
    while not (R / "v.sock").exists(): time.sleep(0.05)
    start("provider", provider_argv(), OUT / "provider.log")
    while True:
        r = rpc("resource.list", {})
        if sum(1 for x in r["result"]["resources"] if x["kind"] == "agent") >= AGENTS: break
        time.sleep(0.2)
    total = int(400 * SCALE)
    for v in ("v1", "v2", "v3"):
        start(v, [BIN / "vqro-items-viewer", "--name", v, "--seconds", total, "--out", OUT / f"{v}.json"], OUT / f"{v}.out")
    threading.Thread(target=sampler, daemon=True).start()
    PHASE[0] = "steady"; at(90)
    if "--no-stall" not in sys.argv:
        PHASE[0] = "stall"; start("stalled", [BIN / "vqro-items-viewer", "--name", "stalled", "--stall", "--seconds", total], OUT / "stalled.out")
    at(150)
    if "--no-slow" not in sys.argv:
        # A slow viewer that comes back: v3 stops reading for 20 s (SIGSTOP), then reattaches from its cursor.
        PHASE[0] = "slow_v3"; procs["v3"].send_signal(signal.SIGSTOP); log("v3 SIGSTOP")
        time.sleep(float(arg("--slow-seconds", "20")) * SCALE); procs["v3"].send_signal(signal.SIGCONT); log("v3 SIGCONT"); PHASE[0] = "stall"
    at(210)
    PHASE[0] = "switch"
    agents = [x["id"] for x in rpc("resource.list", {})["result"]["resources"] if x["kind"] == "agent"]
    for a in agents[:50]:
        r = rpc("resource.invoke", {"resource": a, "op": "agent.session.switch", "args": {}})
        if "error" in r: log("switch error", a, r["error"])
    log("session switch on 50 agents")
    at(270)
    PHASE[0] = "restart"
    procs["provider"].send_signal(signal.SIGKILL); procs["provider"].wait(); log("provider SIGKILL")
    time.sleep(3)
    start("provider", provider_argv(), OUT / "provider.log")
    at(360)
    PHASE[0] = "settle"
    if "stalled" in procs: procs["stalled"].kill(); procs["stalled"].wait(); del procs["stalled"]; log("stalled viewer ended")
    at(390)
    PHASE[0] = "final"
    a0 = [x for x in rpc("resource.list", {})["result"]["resources"] if x["kind"] == "agent"]
    rpc("resource.invoke", {"resource": a0[0]["id"], "op": "items.pause", "args": {"paused": True}}); log("generation paused")
    time.sleep(3)
    truth = {}
    for x in a0:
        r = rpc("resource.invoke", {"resource": x["id"], "op": "items.digest", "args": {}})
        truth[x["id"]] = r.get("result", {}).get("data")
    (OUT / "truth.json").write_text(json.dumps(truth, indent=1))
    for v in ("v1", "v2", "v3"): procs[v].wait(timeout=int(60 * SCALE) + 60)
    STOP.set()
    # Compare each viewer's reconstruction with the runtime at the paused cut.
    summary = {}
    for v in ("v1", "v2", "v3"):
        d = json.loads((OUT / f"{v}.json").read_text())
        same = sum(1 for k, t in truth.items() if t and d["agents"].get(k, {}).get("digest") == t["digest"] and d["agents"][k]["seq"] == t["seq"] and d["agents"][k]["epoch"] == t["epoch"])
        summary[v] = {"agents": len(truth), "identical_to_runtime": same, "counters": d["counters"]}
    stalled = (OUT / "stalled.out").read_text() if (OUT / "stalled.out").exists() else ""
    summary["stalled_attached"] = next((int(l.split("attached ")[1].split()[0]) for l in stalled.splitlines() if "attached " in l), None)
    (OUT / "summary.json").write_text(json.dumps(summary, indent=1)); log("summary", json.dumps({k: v["identical_to_runtime"] for k, v in summary.items() if isinstance(v, dict)}))
    BAD = ("duplicate", "missing", "stale_base", "unknown_entity", "bad_utf8", "bad_json", "bad_record", "out_of_snapshot", "oversize", "over_bound", "rpc_bad_json")
    failures = [f"{v}: {s['identical_to_runtime']}/{s['agents']} identical" for v, s in summary.items() if isinstance(s, dict) and s["identical_to_runtime"] != s["agents"]]
    failures += [f"{v}: {k}={s['counters'][k]}" for v, s in summary.items() if isinstance(s, dict) for k in BAD if s["counters"].get(k)]
    if "--no-stall" not in sys.argv and summary["stalled_attached"] != AGENTS: failures.append(f"stalled viewer attached {summary['stalled_attached']} of {AGENTS}")
    RESULT[0] = failures
finally:
    STOP.set()
    try: rpc("core.shutdown", {})
    except Exception: pass
    time.sleep(1)
    for p in procs.values():
        if p.poll() is None: p.kill()
    for p in os.listdir("/proc"):
        if p.isdigit():
            try:
                if str(R).encode() in open(f"/proc/{p}/environ", "rb").read(): os.kill(int(p), 9); log("leftover killed", p)
            except OSError: pass
    shutil.rmtree(R, ignore_errors=True)
    log("done")
print(json.dumps({"result": "pass" if not RESULT[0] else "fail", "failures": RESULT[0], "out": str(OUT)}))
sys.exit(1 if RESULT[0] else 0)
