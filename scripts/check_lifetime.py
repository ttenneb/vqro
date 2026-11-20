#!/usr/bin/env python3
"""Own one transient gate service; systemd, not a shell EXIT trap, owns descendant teardown.

Scopes outlive their launcher while any member survives. A service with ExitType=main and
KillMode=control-group instead tears down the whole job when this supervisor exits, even by
SIGKILL. Blocking pidfds also bind it to the original launcher and launcher's parent lifetime.
"""
import os
from pathlib import Path
import selectors
import signal
import subprocess
import sys
import time

# Never serialize arbitrary caller/manager credentials or HERDR_* into ExecStart arguments.
FORWARDED = ("HOME", "PATH", "LANG", "LC_ALL", "TMPDIR", "CARGO_TARGET_DIR", "CHECK_JOBS",
             "CHECK_RECEIPT", "VQRO_CHECK_INJECT_LEAK", "XDG_RUNTIME_DIR", "DBUS_SESSION_BUS_ADDRESS")


def birth(pid):
    # proc_pid_stat(5), field22; parsing bytes also handles arbitrary process comm bytes.
    return Path(f"/proc/{pid}/stat").read_bytes().rsplit(b")", 1)[1].split()[19].decode("ascii")


def exit_code(status):
    return status if status >= 0 else 128 - status


def supervise(owners, command):
    """Called only as the service MainPID. Main exit lets native KillMode stop every descendant."""
    with selectors.DefaultSelector() as poll:
        fds = []
        try:
            for pid, started in owners:
                fd = os.pidfd_open(pid)
                fds.append(fd)
                if birth(pid) != started:
                    return 143  # Parent already died/reused during service startup: do not launch.
                poll.register(fd, selectors.EVENT_READ, "owner")
            if poll.select(0):
                return 143
            env = dict(os.environ, VQRO_CHECK_SUPERVISED="1", VQRO_CHECK_SCOPED="1")
            child = subprocess.Popen(command, env=env, close_fds=True)
            fd = os.pidfd_open(child.pid)
            fds.append(fd)
            poll.register(fd, selectors.EVENT_READ, "gate")
            events = poll.select()
            if any(key.data == "owner" for key, _ in events):
                print("check: launcher/parent exited; stopping owned gate service", file=sys.stderr, flush=True)
                # Deliberately leave running children to systemd's cgroup teardown, not Popen.__del__.
                os._exit(143)
            return exit_code(child.wait())
        except (ProcessLookupError, FileNotFoundError):
            return 143
        finally:
            for fd in fds:
                os.close(fd)


def launch(command, owner=None):
    parent = owner[0] if owner else os.getppid()
    if parent <= 1 or (owner and not owner[1]):
        print("check: no launcher-parent identity; refusing an unowned gate", file=sys.stderr)
        return 143
    try:
        owners = [(os.getpid(), birth(os.getpid())), owner or (parent, birth(parent))]
    except FileNotFoundError:
        return 143
    if not hasattr(os, "pidfd_open"):
        print("check: scoped lifetime supervision requires Linux pidfds", file=sys.stderr)
        return 2
    unit = f"vqro-check-{os.getpid()}-{time.monotonic_ns()}.service"
    env = {name: os.environ[name] for name in FORWARDED if name in os.environ}
    env["VQRO_CHECK_UNIT"] = unit
    inside = [sys.executable, "-I", str(Path(__file__).resolve()), "--inside"]
    for pid, started in owners:
        inside.extend([str(pid), started])
    inside.extend(["--", *command])
    args = ["systemd-run", "--user", "--quiet", "--wait", "--pipe", "--collect", "--expand-environment=no",
            "--service-type=exec", "--property=ExitType=main", "--property=KillMode=control-group",
            "--property=TimeoutStopSec=2s", "--property=CPUWeight=50",
            f"--property=Nice={max(10, os.getpriority(os.PRIO_PROCESS, 0))}",
            f"--property=WorkingDirectory={os.getcwd()}", f"--unit={unit}", "--", "/usr/bin/env", "-i"]
    args.extend(f"{name}={value}" for name, value in env.items())
    print(f"check: owned service {unit}", file=sys.stderr, flush=True)
    return exit_code(subprocess.call(args + inside))


def main(args):
    # Reset inherited nohup/background dispositions as well: interruption must end the owner.
    for sig in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(sig, signal.SIG_DFL)
    if args and args[0] == "--inside":
        split = args.index("--")
        values = args[1:split]
        owners = [(int(values[i]), values[i + 1]) for i in range(0, len(values), 2)]
        return supervise(owners, args[split + 1:])
    if args and args[0] == "--owner":
        return launch(args[4:], (int(args[1]), args[2]))
    return launch(args)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
