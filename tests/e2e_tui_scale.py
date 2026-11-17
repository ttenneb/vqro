#!/usr/bin/env python3
"""Opt-in real-TUI scale probe, entirely private: --fake core and explicit private tmux socket.
Usage: python3 tests/e2e_tui_scale.py TARGET [--duration 15] [--receipt FILE]
CPU is percent of one core; latency is tmux send-keys completion to first capture-pane observation.
Performance budgets are enforced, as are correctness and owned-process cleanup. BUDGETS are set from measured
results (docs/test-plan.md, "Scale budgets"): twice the worst of the current check.sh full runs, rounded up to 10;
frame-rate limits are the renderer's design caps (background redraw is capped near 60 fps).
"""
import argparse
import json
import os
from pathlib import Path
import queue
import shutil
import socket
import subprocess
import tempfile
import threading
import time

# measure -> (limit, worst observed in 7 runs on 2026-11-17)
BUDGETS = {"cpu_pct_one_core": (40, 18.0), "rss_peak_mib": (80, 38.6), "key_p99_ms": (170, 81.8),
           "background_fps_peak_complete_second": (64, 55), "fps_peak_complete_second": (120, 94)}


def exceeded(measured):
    """Budget names whose measured value is above its limit (a missing measurement counts as exceeded)."""
    return sorted(name for name, (limit, _) in BUDGETS.items() if not measured.get(name) or measured[name] > limit)


class Peer:
    def __init__(self, path, note=lambda _: None):
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.settimeout(15)
        self.sock.connect(str(path))
        self.sock.settimeout(None)
        self.file = self.sock.makefile("rwb", buffering=0)
        self.pending, self.lock, self.next, self.note = {}, threading.Lock(), 1, note
        self.reader = threading.Thread(target=self.read, daemon=True)
        self.reader.start()
        self.call("hello", {"client": "tui-scale-probe"})

    def read(self):
        try:
            while True:
                line = self.file.readline(64 * 1024 * 1024 + 1)
                if not line:
                    break
                message = json.loads(line)
                if "id" in message:
                    with self.lock:
                        waiter = self.pending.get(message["id"])
                    if waiter:
                        waiter.put(message)
                else:
                    self.note(message)
        except (OSError, ValueError):
            pass

    def call(self, method, params=None):
        waiter = queue.Queue(maxsize=1)
        with self.lock:
            ident, self.next = self.next, self.next + 1
            self.pending[ident] = waiter
            self.file.write((json.dumps({"jsonrpc": "2.0", "id": ident, "method": method, "params": params or {}}) + "\n").encode())
        try:
            result = waiter.get(timeout=15)
            if "error" in result:
                raise RuntimeError(f"{method}: {result['error']}")
            return result["result"]
        finally:
            with self.lock:
                self.pending.pop(ident, None)

    def close(self):
        try:
            self.sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        self.sock.close()


def wait(predicate, seconds=10):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        result = predicate()
        if result:
            return result
        time.sleep(.02)
    raise TimeoutError("private probe condition")


def process(pid):
    fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    cpu = (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")
    rss = int(Path(f"/proc/{pid}/status").read_text().split("VmRSS:")[1].split()[0]) / 1024
    return cpu, rss, fields[19]


def owned(root):
    """Exact private root, including socket inode ownership when tmux scrubs/renames itself."""
    sockets = {}
    for line in Path("/proc/net/unix").read_text().splitlines()[1:]:
        fields = line.split()
        if len(fields) >= 8 and fields[7].startswith(str(root) + "/"):
            sockets[fields[6]] = fields[7]
    result = []
    for entry in Path("/proc").iterdir():
        if not entry.name.isdigit() or int(entry.name) == os.getpid():
            continue
        try:
            hit = str(root).encode() in (entry / "environ").read_bytes()
            if not hit:
                for fd in (entry / "fd").iterdir():
                    link = os.readlink(fd)
                    if link.startswith("socket:[") and link[8:-1] in sockets:
                        hit = True
                        break
            if hit:
                result.append((int(entry.name), (entry / "stat").read_text().rsplit(")", 1)[1].split()[19]))
        except OSError:
            pass
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target", type=Path)
    parser.add_argument("--duration", type=float, default=15)
    parser.add_argument("--terminals", type=int, default=600)
    parser.add_argument("--agents", type=int, default=200)
    parser.add_argument("--receipt", type=Path)
    args = parser.parse_args()
    binary = args.target.resolve() / "release/prim"
    if not binary.exists():
        parser.error(f"build release binary first: {binary}")
    repo = Path(__file__).resolve().parent.parent
    root = Path(tempfile.mkdtemp(prefix="ptus", dir="/tmp"))
    for name in ("home", "st", "ext", "cfg", "run"):
        (root / name).mkdir()
    # Only sidebar: no unrelated policies/workspace scanners or real backend extensions.
    for source in (repo / "extensions").glob("*.py"):
        shutil.copy(source, root / "ext" / source.name)
    (root / "run").chmod(0o700)
    env = {"HOME": str(root / "home"), "PATH": "/usr/bin:/bin", "TERM": "xterm-256color", "LANG": "C.UTF-8",
           "SHELL": "/bin/sh", "PRIM_STATE_DIR": str(root / "st"), "PRIM_SOCKET": str(root / "p.sock"),
           "PRIM_EXTENSIONS_DIR": str(root / "ext"), "PRIM_HERDR": "/bin/false", "HC_SOCKET": str(root / "unused-hc.sock"),
           "HC_SESSION": "private-tui-scale", "XDG_CONFIG_HOME": str(root / "cfg"), "XDG_RUNTIME_DIR": str(root / "run"),
           "PRIM_TUI_PROBE": str(root / "tui-metrics.json")}
    harness = ["/usr/bin/tmux", "-S", str(root / "tmux.sock"), "-f", "/dev/null"]
    def tmux(*command):
        return subprocess.run([*harness, *([] if command[0] == "new-session" else ["-N"]), *command], env=env, capture_output=True, timeout=5, check=True).stdout.decode()
    client, observer, core = None, None, None
    receipt = {"kind": "prim.tui-scale", "version": 1, "profile": {"terminals": args.terminals, "agents": args.agents,
               "status_per_second": args.agents / 2, "flood_bytes_per_second": 1_000_000, "views": 3, "sidebar": True, "duration_s": args.duration},
               "budgets": {"enforced": True, **{name: limit for name, (limit, _) in BUDGETS.items()}}}
    events = {"resource": 0, "extension": 0, "missed": 0}
    try:
        log = (root / "serve.log").open("wb")
        core = subprocess.Popen([str(binary), "serve", "--fake"], env=env, stdout=log, stderr=subprocess.STDOUT)
        wait(lambda: (root / "p.sock").exists())
        client = Peer(root / "p.sock")
        wait(lambda: client.call("workspace.snapshot")["providers"]["fake"]["synced"])
        receipt["stage"] = "spawn-resources"
        resources = []
        for index in range(args.terminals):
            argv = ["agent", "--status-every-ms", "2000", "--status-phase-ms", str(index * 2000 // max(1, args.agents) + 1)] if index < args.agents else ["sh"]
            if index == args.agents:
                argv = ["flood", "--output-bytes-per-sec", "1000000"]
            resources.append(client.call("resource.spawn", {"argv": argv, "name": f"probe-{index}"})["resource"]["id"])
        client.call("workspace.apply", {"ops": [{"op": "set", "key": "extension/sidebar", "value": {"argv": ["/usr/bin/python3", str(root / "ext/sidebar.py")]}}]})
        wait(lambda: client.call("workspace.snapshot")["live"].get("sidebar"))
        views = {"probe-input": resources[-1], "probe-flood": resources[args.agents], "probe-other": resources[-2]}
        ops = [{"op": "set", "key": f"view/{name}", "value": {"kind": "terminal", "resource": rid, "title": name}} for name, rid in views.items()]
        layout = {"split": "h", "sizes": [1, 3], "children": [{"view": "sidebar"}, {"split": "v", "children": [
            {"view": "probe-input"}, {"split": "h", "children": [{"view": "probe-flood"}, {"view": "probe-other"}]}]}]}
        ops += [{"op": "set", "key": "layout/main", "value": layout}, {"op": "set", "key": "workspace/active", "value": {"layout": "main"}}]
        client.call("workspace.apply", {"note": "private TUI scale layout", "ops": ops})
        wait(lambda: client.call("workspace.snapshot")["live"].get("sidebar"))
        receipt["stage"] = "start-tui"
        tmux("new-session", "-d", "-x", "160", "-y", "44", str(binary), "tui")
        pid = int(tmux("display-message", "-p", "#{pane_pid}").strip())
        wait(lambda: "probe-input" in tmux("capture-pane", "-p"))
        time.sleep(1)
        # SGR click: focus the idle top-right terminal, never the sidebar or flood.
        tmux("send-keys", "-H", *[f"{b:02x}" for b in b"\x1b[<0;80;5M\x1b[<0;80;5m"])
        def note(message):
            kind = message.get("params", {}).get("type", message.get("method"))
            if kind in events:
                events[kind] += 1
        observer = Peer(root / "p.sock", note)
        start_snapshot = client.call("workspace.snapshot")
        observer.call("events.subscribe", {"from": start_snapshot["seq"], "epoch": start_snapshot["epoch"], "types": list(events), "partial_resources": True})
        receipt["stage"] = "measurement"
        cpu_start, rss_start, birth = process(pid)
        start = time.monotonic()
        latencies, lost, samples = [], [], []
        index = 0
        while time.monotonic() - start < args.duration:
            token = f"K{index:05d}Z"
            before = time.monotonic()
            tmux("send-keys", "-l", "\r" + token)  # Start at column zero; capture must not mistake wrapping for loss.
            try:
                wait(lambda: token in tmux("capture-pane", "-p"), 2)
                latencies.append((time.monotonic() - before) * 1000)
            except TimeoutError:
                lost.append(token)
            cpu, rss, _ = process(pid)
            samples.append((time.monotonic() - start, cpu - cpu_start, rss))
            index += 1
            time.sleep(.2)
        elapsed = time.monotonic() - start
        cpu_end, rss_end, _ = process(pid)
        end_snapshot = client.call("workspace.snapshot")
        receipt["stage"] = "quit"
        tmux("send-keys", "C-g")
        time.sleep(.1)
        tmux("send-keys", "q")
        wait(lambda: not Path(f"/proc/{pid}").exists(), 5)
        metrics = json.loads((root / "tui-metrics.json").read_text())
        complete_buckets = [row for row in metrics["frames_by_second"] if 1 < row[0] < int(metrics["elapsed_s"])]
        complete = [row[1] for row in complete_buckets]
        background = [row[2] for row in complete_buckets]
        received_bps = metrics["received_stream_bytes"] / metrics["elapsed_s"]
        workload_valid = received_bps >= 900_000 and events["resource"] / elapsed >= args.agents / 2 * .9
        latencies.sort()
        receipt.update({"tui": {"cpu_pct_one_core": ((cpu_end - cpu_start) / elapsed) * 100, "rss_start_mib": rss_start,
            "rss_peak_mib": max(row[2] for row in samples), "frames": metrics["frames"], "fps_mean_complete_seconds": sum(complete) / max(1, len(complete)),
            "fps_peak_complete_second": max(complete, default=0), "background_fps_peak_complete_second": max(background, default=0), "draw_total_ms": metrics["draw_total_ms"], "draw_max_ms": metrics["draw_max_ms"]},
            "keys": {"samples": len(latencies), "lost": lost, "p50_ms": latencies[len(latencies)//2] if latencies else None,
            "p99_ms": latencies[min(len(latencies)-1, int(len(latencies)*.99))] if latencies else None, "max_ms": max(latencies, default=None)},
            "events": events, "extensions": end_snapshot["extensions"], "status_per_second_observed": events["resource"] / elapsed,
            "sidebar_publishes_estimated_per_second": (end_snapshot["seq"] - start_snapshot["seq"] - events["resource"] - events["extension"]) / elapsed,
            "received_stream_bytes_per_s": received_bps, "workload_valid": workload_valid,
            "metrics": metrics})
        over = exceeded({**receipt["tui"], "key_p99_ms": receipt["keys"]["p99_ms"]})
        receipt["budgets_exceeded"] = over
        receipt["result"] = "pass" if workload_valid and not lost and not events["missed"] and latencies and not over and end_snapshot["extensions"]["sidebar"]["status"] == "running" and end_snapshot["extensions"]["sidebar"]["crashes"] == 0 else "fail"
    except Exception as error:
        receipt.update(result="fail", error=f"{type(error).__name__}: {error}")
        try:
            receipt["screen_on_failure"] = tmux("capture-pane", "-p")
        except Exception:
            pass
    finally:
        subprocess.run([*harness, "-N", "kill-server"], env=env, capture_output=True, timeout=5)
        if observer:
            observer.close()
        if client:
            try:
                client.call("core.shutdown")
            except Exception:
                pass
            client.close()
        if core:
            try:
                core.wait(timeout=5)
            except subprocess.TimeoutExpired:
                core.kill(); core.wait()
        time.sleep(.1)
        left = owned(root)
        receipt["leftover_processes"] = left
        if left:
            receipt["result"] = "fail"
            # Exact private-root membership and PID birth are checked again before any signal.
            for pid, birth in left:
                try:
                    if process(pid)[2] == birth:
                        os.kill(pid, 9)
                except (OSError, IndexError):
                    pass
        if receipt.get("result") != "pass":
            receipt["serve_log"] = (root / "serve.log").read_text(errors="replace")[-4000:]
        shutil.rmtree(root)
    wire = json.dumps(receipt, indent=2) + "\n"
    if args.receipt:
        args.receipt.parent.mkdir(parents=True, exist_ok=True)
        args.receipt.write_text(wire)
    print(wire)
    return 0 if receipt["result"] == "pass" else 1


if __name__ == "__main__":
    raise SystemExit(main())
