#!/usr/bin/env python3
"""Linux-only opt-in CPU-pressure runner for the standalone tmux flood test.

Constrain only reader/test threads AFTER native clients attach, leaving native producer
children on their normal CPUs. Pinning the entire process tree throttles the producer and
can hide the backlog failure. All environments and cleanup are private to this invocation.
"""

import sys
if __name__ == "__main__" and sys.platform != "linux":
    print("SKIP tmux_flood_load: requires Linux /proc and namespace/process probes")
    raise SystemExit(77)

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import time


def main():
    if sys.platform != "linux":
        print("SKIP tmux_flood_load: requires Linux /proc and namespace/process probes")
        raise SystemExit(77)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path, help="compiled tests/tmux.rs executable")
    parser.add_argument("output", type=Path, help="new private evidence directory (must not exist)")
    parser.add_argument("--runs", type=int, default=20)
    parser.add_argument("--workers", type=int, default=4)
    args = parser.parse_args()
    if args.runs < 1 or args.workers < 1:
        parser.error("positive run/worker counts required")
    args.output.mkdir(parents=True, exist_ok=False)
    env = {"HOME": os.environ["HOME"], "PATH": os.environ["PATH"]}
    cpu = min(os.sched_getaffinity(0))
    workers, results = [], []
    try:
        for _ in range(args.workers):
            workers.append(subprocess.Popen(["/usr/bin/taskset", "-c", str(cpu), sys.executable, "-c",
                "import time; end=time.monotonic()+600\nwhile time.monotonic()<end: pass"], env=env))
        for index in range(args.runs):
            assert all(worker.poll() is None for worker in workers), "load worker exited; trial invalid"
            marker = args.output / f"trial-{index}.ready"
            started = time.monotonic()
            with (args.output / f"trial-{index}.log").open("wb") as log:
                child = subprocess.Popen([str(args.binary.resolve()), "--ignored", "read_only_timeouts", "--nocapture"],
                    env={**env, "TMUX_FLOOD_LOAD_READY": str(marker.resolve())}, stdout=log, stderr=subprocess.STDOUT)
                try:
                    deadline = time.monotonic() + 15
                    while not marker.exists() and child.poll() is None and time.monotonic() < deadline:
                        time.sleep(.002)
                    if not marker.exists():
                        raise RuntimeError("test did not reach the private load rendezvous")
                    for thread in Path(f"/proc/{child.pid}/task").iterdir():
                        os.sched_setaffinity(int(thread.name), {cpu})
                    marker.with_suffix(".go").write_text("go")
                    code = child.wait(timeout=90)
                finally:
                    if child.poll() is None:
                        child.kill()
                        child.wait(timeout=3)
                        # Timeout cleanup only: the rendezvous contains THIS test's private socket.
                        if marker.exists():
                            socket = Path(marker.read_text())
                            if socket.name == "s" and socket.parent.name.startswith("ptmx") and socket.parent.parent == Path("/tmp"):
                                subprocess.run(["/usr/bin/tmux", "-N", "-S", str(socket), "kill-server"],
                                               env=env, capture_output=True, timeout=3)
            assert all(worker.poll() is None for worker in workers), "load worker exited; trial invalid"
            result = {"trial": index, "code": code, "seconds": time.monotonic()-started, "reader_cpu": cpu,
                      "workers": args.workers}
            results.append(result)
            print(json.dumps(result), flush=True)
    finally:
        for worker in workers:
            worker.terminate()
        for worker in workers:
            worker.wait(timeout=3)
        (args.output / "results.json").write_text(json.dumps(results, indent=2))
    return int(any(result["code"] for result in results))


if __name__ == "__main__":
    sys.exit(main())
