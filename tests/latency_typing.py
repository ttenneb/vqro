#!/usr/bin/python3
"""Opt-in typing-latency harness: how long until a typed token is visible on screen.

The real `vqro tui` runs in an outer tmux harness against a private core with a private herdr-core (vqro-herdr)
and the private tmux provider. A token is typed with `send-keys`; the screen is polled with `capture-pane`
until the token shows; the line is then cleared (Ctrl-U). Scenarios:
- plain:       bash directly in a harness pane (the floor of this method: tmux in, tmux out, polling);
- herdr-idle:  vqro, one bash pane on herdr-core;
- tmux-idle:   vqro, one bash pane on the vqro-owned tmux provider;
- herdr-agent: vqro, focused bash beside a pane streaming like an agent (about 200 short lines a second);
- herdr-flood: vqro, focused bash beside a pane flooding output as fast as it can (`yes`).

Usage: tests/latency_typing.py TARGET_DIR [--samples N] [--receipt FILE] [--only a,b]
(debug or release binaries: TARGET_DIR/debug unless --release). Prints one line per scenario and writes a JSON
receipt with every sample and the load average before and after. Private /tmp root; nothing live is touched.
"""
import argparse, json, os, random, shutil, socket, statistics, subprocess, sys, tempfile, time

ap = argparse.ArgumentParser()
ap.add_argument("target")
ap.add_argument("--samples", type=int, default=30)
ap.add_argument("--receipt")
ap.add_argument("--release", action="store_true")
ap.add_argument("--only", default="")
args = ap.parse_args()
BIN = os.path.join(args.target, "release" if args.release else "debug")
VQRO, PHERDR, PTMUX = (os.path.join(BIN, b) for b in ("vqro", "vqro-herdr", "vqro-tmux"))
HC = os.path.expanduser("~/.local/bin/herdr-core")
R = tempfile.mkdtemp(prefix="plt", dir="/tmp")
for d in ("home", "st", "xst", "run", "cfg", "ext"):
    os.makedirs(os.path.join(R, d))
os.chmod(os.path.join(R, "run"), 0o700)
with open(os.path.join(R, "ext", "defaults.json"), "w") as f:
    json.dump({"version": 2, "extensions": []}, f)
ENV = {"HOME": f"{R}/home", "PATH": "/usr/bin:/bin", "TERM": "xterm-256color", "SHELL": "/bin/sh", "VQRO_STATE_DIR": f"{R}/st",
       "VQRO_SOCKET": f"{R}/p.sock", "VQRO_EXTENSIONS_DIR": f"{R}/ext", "XDG_STATE_HOME": f"{R}/xst",
       "XDG_RUNTIME_DIR": f"{R}/run", "XDG_CONFIG_HOME": f"{R}/cfg", "HC_SESSION": "latency", "HC_SOCKET": f"{R}/hc.sock",
       "LANG": "C.UTF-8", **({"VQRO_TUI_PROBE": os.environ["VQRO_TUI_PROBE"]} if os.environ.get("VQRO_TUI_PROBE") else {})}
HARNESS = ["/usr/bin/tmux", "-S", f"{R}/harness.sock", "-f", "/dev/null"]
SHELL = ["/bin/bash", "--norc", "--noprofile"]
SHELL_ENV = {"PATH": "/usr/bin:/bin", "HOME": f"{R}/home", "TERM": "xterm-256color", "LANG": "C.UTF-8", "PS1": "$ "}
AGENT = ["/bin/bash", "-c", "i=0; while :; do i=$((i+1)); echo \"assistant line $i: some streamed text of a typical width\"; sleep 0.005; done"]
FLOOD = ["/usr/bin/yes", "flood 0123456789 0123456789 0123456789 0123456789 0123456789 0123456789 0123456789"]


def call(method, params=None):
    s = socket.socket(socket.AF_UNIX)
    s.connect(ENV["VQRO_SOCKET"])
    f = s.makefile("rwb", buffering=0)
    for i, (m, p) in enumerate([("hello", {"client": "latency"}), (method, params or {})]):
        f.write((json.dumps({"jsonrpc": "2.0", "id": i, "method": m, "params": p}) + "\n").encode())
        while True:
            msg = json.loads(f.readline())
            if msg.get("id") == i:
                break
    s.close()
    if "error" in msg:
        raise RuntimeError(f"{method}: {msg['error']}")
    return msg["result"]


def wait(pred, timeout=20):
    end = time.time() + timeout
    while time.time() < end:
        try:
            v = pred()
            if v:
                return v
        except Exception:
            pass
        time.sleep(0.1)
    return None


def tmux(*a):
    return subprocess.run([*HARNESS, *a], capture_output=True, text=True).stdout


def measure(target, n):
    """Typed token -> visible on the harness screen, in ms; the line is cleared between samples."""
    out = []
    for i in range(n):
        token = "".join(random.choice("qwxzjk") for _ in range(6)) + str(i)
        t0 = time.monotonic()
        tmux("send-keys", "-t", target, "-l", token)
        while token not in tmux("capture-pane", "-p", "-t", target):
            if time.monotonic() - t0 > 3:
                break
        out.append((time.monotonic() - t0) * 1000)
        tmux("send-keys", "-t", target, "C-u")
        time.sleep(0.15)
    return out


def processes():
    """Our processes (their environment or command line names the private root): pid -> short name."""
    out = {}
    for pid in filter(str.isdigit, os.listdir("/proc")):
        try:
            cmd = open(f"/proc/{pid}/cmdline", "rb").read().replace(b"\0", b" ").decode(errors="replace")
            if R in cmd or R.encode() in open(f"/proc/{pid}/environ", "rb").read():
                name = os.path.basename(cmd.split()[0]) + (" " + cmd.split()[1] if len(cmd.split()) > 1 and not cmd.split()[1].startswith("-") else "")
                out[pid] = name[:40]
        except (OSError, IndexError):
            pass
    return out


def cpu_ticks(pid):
    try:
        f = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
        return int(f[11]) + int(f[12])
    except (OSError, IndexError):
        return 0


def cpu_during(fn):
    """Run fn; return its result and each of our processes' CPU use meanwhile (% of one core)."""
    procs = processes()
    before, t0 = {p: cpu_ticks(p) for p in procs}, time.monotonic()
    result = fn()
    elapsed, hz = time.monotonic() - t0, os.sysconf("SC_CLK_TCK")
    use = {f"{name} ({pid})": round(100 * (cpu_ticks(pid) - before[pid]) / hz / elapsed, 1) for pid, name in procs.items()}
    return result, {k: v for k, v in sorted(use.items(), key=lambda kv: -kv[1]) if v >= 1}


def summary(samples):
    s = sorted(samples)
    return {"median_ms": round(statistics.median(s), 1), "p90_ms": round(s[int(len(s) * .9)], 1), "max_ms": round(s[-1], 1)}


def spawn(provider, argv, env=None):
    return call("resource.spawn", {"provider": provider, "argv": argv, "cwd": f"{R}/home", "env": env or SHELL_ENV,
                                   "cols": 100, "rows": 40})["resource"]["id"]


def show(*rids):
    """One tab: the first resource focused (left), the others beside it."""
    ops = [{"op": "set", "key": f"view/term-{rid}", "value": {"kind": "terminal", "resource": rid}} for rid in rids]
    node = {"view": f"term-{rids[0]}"} if len(rids) == 1 else {"split": "h", "children": [{"view": f"term-{r}"} for r in rids], "sizes": [1] * len(rids)}
    snap = call("workspace.snapshot")
    ops.append({"op": "set", "key": "layout/main", "value": {"tabs": [{"title": "latency", "node": node}], "active": 0},
                "expect": snap["entries"].get("layout/main", {}).get("version", 0)})
    ops.append({"op": "set", "key": "workspace/active", "value": {"layout": "main"}, "expect": snap["entries"].get("workspace/active", {}).get("version", 0)})
    call("workspace.apply", {"note": "latency layout", "ops": ops})


def ready_shell(target):
    """The focused pane is a shell that answers: a fresh marker each time, so old screen text can't pass."""
    for _ in range(5):
        n = random.randint(1000, 9999)
        tmux("send-keys", "-t", target, "-l", f"echo ready-$(({n}+1))"); tmux("send-keys", "-t", target, "Enter")
        if wait(lambda: f"ready-{n+1}" in tmux("capture-pane", "-p", "-t", target), 6):
            tmux("send-keys", "-t", target, "-l", "clear"); tmux("send-keys", "-t", target, "Enter")
            time.sleep(0.5)
            return True
    return False


def loadavg():
    return open("/proc/loadavg").read().split()[:3]


receipt = {"binaries": BIN, "samples": args.samples, "load_before": loadavg(), "scenarios": {}}
wanted = set(filter(None, args.only.split(",")))
core = None
try:
    tmux("new-session", "-d", "-s", "h", "-x", "205", "-y", "45", "/bin/sh")
    def run(name, fn):
        if wanted and name not in wanted:
            return
        samples, cpu = cpu_during(fn)
        receipt["scenarios"][name] = dict(summary(samples), load=loadavg(), cpu_percent=cpu, samples=[round(x, 1) for x in samples])
        print(f"{name:12} median {receipt['scenarios'][name]['median_ms']:6.1f} ms  p90 {receipt['scenarios'][name]['p90_ms']:6.1f}  max {receipt['scenarios'][name]['max_ms']:6.1f}  load {receipt['scenarios'][name]['load'][0]}  cpu {cpu}", flush=True)

    def plain():
        tmux("new-window", "-t", "h", "-n", "plain", "env -i PATH=/usr/bin:/bin HOME=" + R + "/home TERM=xterm-256color PS1='$ ' /bin/bash --norc --noprofile")
        ready_shell("h:plain")
        return measure("h:plain", args.samples)
    run("plain", plain)

    core = subprocess.Popen([VQRO, "serve"], env=ENV, stdout=open(f"{R}/serve.log", "w"), stderr=subprocess.STDOUT)
    wait(lambda: os.path.exists(ENV["VQRO_SOCKET"]), 10)
    call("workspace.apply", {"note": "providers", "ops": [
        {"op": "set", "key": "extension/herdr", "value": {"argv": ["/usr/bin/env", "PATH=", PHERDR, "--hc", HC, "--name", "herdr"], "kind": "provider", "default": True}},
        {"op": "set", "key": "extension/tmux", "value": {"argv": ["/usr/bin/env", "PATH=", PTMUX, "--tmux", "/usr/bin/tmux", "--name", "tmux"], "kind": "provider"}}]})
    assert wait(lambda: all(call("workspace.snapshot")["providers"].get(n, {}).get("state") == "connected" for n in ("herdr", "tmux")), 30), "providers"
    shells = {"herdr": spawn("herdr", SHELL), "tmux": spawn("tmux", SHELL)}
    show(shells["herdr"])
    with open(f"{R}/tui.sh", "w") as f:
        f.write("".join(f"export {k}='{v}'\n" for k, v in ENV.items()) + f"exec {VQRO} tui\n")
    tmux("new-window", "-t", "h", "-n", "vqro", f"/bin/sh {R}/tui.sh")
    assert ready_shell("h:vqro"), "vqro shell ready"

    run("herdr-idle", lambda: measure("h:vqro", args.samples))

    def provider(name):
        show(shells[name]); time.sleep(1.5)
        assert ready_shell("h:vqro"), name
        return measure("h:vqro", args.samples)
    run("tmux-idle", lambda: provider("tmux"))

    def beside(argv):
        def go():
            other = spawn("herdr", argv)
            show(shells["herdr"], other); time.sleep(2)
            # Focus the shell (left half) by clicking it: typing must reach the shell, never the stream.
            tmux("send-keys", "-t", "h:vqro", "-l", "\x1b[<0;20;10M\x1b[<0;20;10m"); time.sleep(0.5)
            assert ready_shell("h:vqro"), "shell beside a stream"
            try:
                return measure("h:vqro", args.samples)
            finally:
                call("resource.kill", {"resource": other})
        return go
    run("herdr-agent", beside(AGENT))
    run("herdr-flood", beside(FLOOD))
finally:
    receipt["load_after"] = loadavg()
    tmux("kill-server")
    if core:
        core.terminate()
        try:
            core.wait(10)
        except subprocess.TimeoutExpired:
            core.kill()
    subprocess.run([HC, "server", "stop"], env={**ENV, "PATH": ""}, capture_output=True)
    subprocess.run(["/usr/bin/tmux", "-N", "-S", f"{R}/st/tmux.sock", "kill-server"], env={"HOME": f"{R}/home", "PATH": "/usr/bin:/bin"}, capture_output=True)
    if args.receipt:
        with open(args.receipt, "w") as f:
            json.dump(receipt, f, indent=1)
    time.sleep(1)
    def ours(pid):
        try:
            return R.encode() in open(f"/proc/{pid}/environ", "rb").read() or R.encode() in open(f"/proc/{pid}/cmdline", "rb").read()
        except OSError:
            return False
    left = [p for p in os.listdir("/proc") if p.isdigit() and int(p) != os.getpid() and ours(p)]
    if left:  # e.g. a core the closing TUI started again while reconnecting: ours only (private root)
        for pid in left:
            try:
                os.kill(int(pid), 15)
            except OSError:
                pass
        time.sleep(1)
        left = [p for p in left if os.path.exists(f"/proc/{p}") and ours(p)]
    if not left:
        shutil.rmtree(R, ignore_errors=True)
    print(f"load before {receipt['load_before']} after {receipt['load_after']}; root {R}{' (leftover processes: ' + ' '.join(left) + ')' if left else ''}")
