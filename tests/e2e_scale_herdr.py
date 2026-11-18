#!/usr/bin/python3
"""Opt-in load probe through a wire provider: private vqro core + vqro-herdr + private herdr-core.

Usage: tests/e2e_scale_herdr.py TARGET_DIR [TERMINALS=150] [AGENTS=50] [FLOODS=2] [SECONDS=30]
Real terminals in herdr-core (agents report OSC 7501 every 2 s; floods are `yes` drained by an observer),
an echo probe on an idle terminal during the floods. Prints a JSON receipt. Stops only what it started.
Not the in-process scale harness (tests/scale.rs): numbers here include herdr-core and real shells.
"""
import base64, json, os, queue, shutil, socket, subprocess, sys, tempfile, threading, time

TARGET = sys.argv[1]
N, AGENTS, FLOODS, SECONDS = (int(x) for x in (sys.argv[2:] + ["150", "50", "2", "30"][len(sys.argv) - 2:])[:4])
VQRO, PHERDR = (os.path.join(TARGET, "debug", b) for b in ("vqro", "vqro-herdr"))
HC = os.path.expanduser("~/.local/bin/herdr-core")
R = tempfile.mkdtemp(prefix="pe2s", dir="/tmp")
for d in ("home", "st", "xst", "run", "cfg", "ext"):
    os.makedirs(os.path.join(R, d))
os.chmod(os.path.join(R, "run"), 0o700)
ENV = {"HOME": f"{R}/home", "PATH": "", "TERM": "xterm-256color", "SHELL": "/bin/sh", "VQRO_STATE_DIR": f"{R}/st",
       "VQRO_SOCKET": f"{R}/p.sock", "VQRO_EXTENSIONS_DIR": f"{R}/ext", "XDG_STATE_HOME": f"{R}/xst",
       "XDG_RUNTIME_DIR": f"{R}/run", "XDG_CONFIG_HOME": f"{R}/cfg", "HC_SESSION": "scale"}


class Client:
    def __init__(self, name):
        self.s = socket.socket(socket.AF_UNIX)
        self.s.connect(ENV["VQRO_SOCKET"])
        self.f = self.s.makefile("wb", buffering=0)
        self.r = self.s.makefile("rb")  # buffered: line reads must not be byte-at-a-time
        self.n, self.replies, self.cv, self.lock = 0, {}, threading.Condition(), threading.Lock()
        self.handlers = []
        threading.Thread(target=self.read, daemon=True).start()
        self.call("hello", {"client": name})

    def read(self):
        for line in self.r:
            msg = json.loads(line)
            if "method" in msg:
                for h in self.handlers:
                    h(msg)
            else:
                with self.cv:
                    self.replies[msg["id"]] = msg
                    self.cv.notify_all()

    def call(self, method, params=None, timeout=60):
        with self.lock:
            self.n += 1
            ident = self.n
            self.f.write((json.dumps({"jsonrpc": "2.0", "id": ident, "method": method, "params": params or {}}) + "\n").encode())
        with self.cv:
            if not self.cv.wait_for(lambda: ident in self.replies, timeout):
                raise TimeoutError(method)
            msg = self.replies.pop(ident)
        if "error" in msg:
            raise RuntimeError(f"{method}: {msg['error']}")
        return msg["result"]

    def notify(self, method, params):
        with self.lock:
            self.f.write((json.dumps({"jsonrpc": "2.0", "method": method, "params": params}) + "\n").encode())


def rss_mb(pid):
    try:
        for line in open(f"/proc/{pid}/status"):
            if line.startswith("VmRSS:"):
                return int(line.split()[1]) / 1024
    except OSError:
        return None


def pct(v, q):
    return round(sorted(v)[round((len(v) - 1) * q)], 1) if v else None


def pids_with(word):
    out = []
    for pid in os.listdir("/proc"):
        if pid.isdigit():
            try:
                if R.encode() in open(f"/proc/{pid}/environ", "rb").read() and word.encode() in open(f"/proc/{pid}/cmdline", "rb").read():
                    out.append(int(pid))
            except OSError:
                pass
    return out


core, receipt = None, {"profile": {"terminals": N, "agents": AGENTS, "floods": FLOODS, "seconds": SECONDS}}
try:
    core = subprocess.Popen([VQRO, "serve", "--fake"], env=ENV, stdout=open(f"{R}/serve.log", "w"), stderr=subprocess.STDOUT)
    for _ in range(50):
        if os.path.exists(ENV["VQRO_SOCKET"]):
            break
        time.sleep(0.2)
    c = Client("scale-writer")
    c.call("workspace.apply", {"ops": [{"op": "set", "key": "extension/herdr", "value": {"argv": [PHERDR, "--hc", HC, "--name", "herdr"]}}]})
    for _ in range(100):
        if c.call("workspace.snapshot")["providers"].get("herdr", {}).get("synced"):
            break
        time.sleep(0.2)
    spawn_ms, ids = [], []
    agent = "i=0; while :; do printf '\\033]7501;state=working:app=a%s\\007' $i; i=$((i+1)); /usr/bin/sleep 2; done"
    t0 = time.time()
    for i in range(N):
        # Floods start only after the observer attached (see the note below).
        argv = ["/bin/sh", "-c", agent] if i < AGENTS else ["/bin/sh"]
        t = time.time()
        ids.append(c.call("resource.spawn", {"provider": "herdr", "argv": argv})["resource"]["id"])
        spawn_ms.append((time.time() - t) * 1000)
    receipt["spawn"] = {"seconds": round(time.time() - t0, 1), "p50_ms": pct(spawn_ms, .5), "p99_ms": pct(spawn_ms, .99)}

    obs = Client("scale-observer")
    stats = {"status": 0, "missed": 0, "flood_bytes": 0, "events": 0, "stream_ends": []}
    echo = {"buf": b"", "token": None, "t0": 0, "done": threading.Event()}
    flood_sids, idle_sid = set(), None

    def on(msg):
        m, p = msg["method"], msg.get("params", {})
        if m == "event":
            stats["events"] += 1
            if p.get("type") == "resource" and isinstance(p.get("data", {}).get("status"), dict):
                stats["status"] += 1
        elif m == "missed":
            stats["missed"] += 1
        elif m in ("stream.gap", "stream.closed"):
            stats["stream_ends"].append({"method": m, "stream": p.get("stream"), "reason": p.get("reason")})
        elif m == "stream.data":
            if p["stream"] == idle_sid:
                echo["buf"] = (echo["buf"] + base64.b64decode(p["data"]))[-4096:]
                if echo["token"] and echo["token"].encode() in echo["buf"]:
                    echo["done"].set()
            else:
                stats["flood_bytes"] += len(p["data"]) * 3 // 4
    obs.handlers.append(on)
    obs.call("events.subscribe", {"partial_resources": True})
    for rid in ids[AGENTS:AGENTS + FLOODS]:
        flood_sids.add(obs.call("resource.attach", {"resource": rid, "cols": 80, "rows": 24})["stream"])
    idle_sid = obs.call("resource.attach", {"resource": ids[-1], "cols": 80, "rows": 24})["stream"]
    # Floods start after attaching, at a bounded rate.
    for sid in flood_sids:
        # About 1.3 MB/s each (64 KiB every 50 ms): an unthrottled `yes` outruns any viewer slower than the
        # producer and is closed slow_consumer by design (credit on writer acceptance; viewer credit is OPEN).
        obs.notify("stream.input", {"stream": sid, "text": "while :; do /usr/bin/head -c 65536 /dev/zero | /usr/bin/tr '\\0' x; echo; /usr/bin/sleep 0.05; done\r"})
    core_rss, prov_rss, hc_rss, echo_ms, lost = [], [], [], [], 0
    # Harness-local process measurement, not a process-id-dependent public contract.
    provider_pids = pids_with("vqro-herdr")
    assert len(provider_pids) == 1, provider_pids
    prov_pid = provider_pids[0]
    hc_pids = pids_with("herdr-core")
    end, k = time.time() + SECONDS, 0
    while time.time() < end:
        k += 1
        echo["token"], echo["buf"] = f"<p{k}>", b""
        echo["done"].clear()
        echo["t0"] = time.time()
        obs.notify("stream.input", {"stream": idle_sid, "text": f"echo '<p{k}>'\r"})
        if echo["done"].wait(5):
            echo_ms.append((time.time() - echo["t0"]) * 1000)
        else:
            lost += 1
        core_rss.append(rss_mb(core.pid))
        prov_rss.append(rss_mb(prov_pid))
        hc_rss.append(sum(filter(None, (rss_mb(p) for p in hc_pids))))
        time.sleep(0.2)
    receipt.update({
        "echo_idle_during_floods": {"n": len(echo_ms), "p50_ms": pct(echo_ms, .5), "p99_ms": pct(echo_ms, .99), "max_ms": pct(echo_ms, 1), "lost_after_5s": lost},
        "status_events_per_s": round(stats["status"] / SECONDS, 1),
        "status_expected_per_s": round(AGENTS / 2, 1), "missed": stats["missed"],
        "flood_mb_per_s_to_observer": round(stats["flood_bytes"] / SECONDS / 1e6, 2),
        "stream_ends": stats["stream_ends"][:10],
        "rss_mb": {"core_peak": round(max(filter(None, core_rss)), 1), "vqro_herdr_peak": round(max(filter(None, prov_rss)), 1),
                   "herdr_core_peak": round(max(hc_rss), 1)},
    })
finally:
    if core:
        core.terminate()
        try:
            core.wait(15)
        except subprocess.TimeoutExpired:
            core.kill()
    subprocess.run([HC, "server", "stop"], env=ENV, capture_output=True)
    time.sleep(2)
    left = pids_with("")
    receipt["left_running"] = left
    for pid in left:
        try:
            os.kill(pid, 9)
        except OSError:
            pass
    print(json.dumps(receipt, indent=1))
    shutil.rmtree(R, ignore_errors=True)
