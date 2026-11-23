r"""Native Windows regression: detached core -> extension/provider -> stdio child, no consoles.

  python tests/windows_background_smoke.py --vqro target/debug/vqro.exe

Uses two private fixture declarations, no real backend, no agent, no terminal spawn, and
no Vqro SDK controller (that SDK exits its process on disconnect). Exercises extension
restart and native connector replacement, captures process handles, then shuts down and
requires all observed fixture processes to exit. Never starts a daily core or backend.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import uuid

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "extensions"))
from vqro_transport import connect_endpoint

# Each fixture reports its own console handle, rather than trusting redirected stdio
# or the absence of workspace resources. The native fixture is launched by vqro-herdr.
FIXTURE = r'''
import ctypes, json, os, pathlib, subprocess, sys
sys.path.insert(0, sys.argv[1])
from vqro_process import background_kwargs
mode, reports = sys.argv[2], pathlib.Path(sys.argv[3])
get_console = ctypes.WinDLL('kernel32').GetConsoleWindow
get_console.restype = ctypes.c_void_p
report = {'mode': mode, 'pid': os.getpid(), 'console': get_console() or 0}
(reports / (mode + '-' + str(os.getpid()) + '.json')).write_text(json.dumps(report))
if mode == 'extension':
    result = subprocess.run([sys.executable, __file__, sys.argv[1], 'helper', str(reports)],
                            stdin=subprocess.DEVNULL, capture_output=True, timeout=10,
                            check=True, **background_kwargs())
    print('helper stdout: ' + result.stdout.decode().strip(), flush=True)
    print('extension stderr', file=sys.stderr, flush=True)
if mode == 'helper':
    print('stdio works', flush=True)
else:
    sys.stdin.buffer.read()  # Extension lifetime EOF / connector stdin close ends it.
'''


class Client:
    def __init__(self, endpoint):
        self.pipe = connect_endpoint(endpoint, timeout=2)
        self.file = self.pipe.makefile("rwb", buffering=0)
        self.identifier = 0
        self.call("hello", {"client": "windows-background-smoke"})

    def call(self, method, params=None):
        self.identifier += 1
        request = {"jsonrpc": "2.0", "id": self.identifier, "method": method, "params": params or {}}
        self.pipe.sendall((json.dumps(request) + "\n").encode())
        row = json.loads(self.file.readline(64 * 1024 * 1024 + 1))
        if row.get("id") != self.identifier or "error" in row:
            raise RuntimeError(row)
        return row["result"]

    def close(self):
        self.file.close()
        self.pipe.close()


def wait_for(check, description, seconds=15):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(.05)
    raise TimeoutError(description)


def close_fixture_handles(handles, api, *, unobserved=()):
    """Wait/close only: a PID report does not authorize terminating a later process with that PID."""
    incomplete = [f"unobserved report {name}" for name in unobserved]
    for pid, handle in handles.items():
        try:
            if api.WaitForSingleObject(handle, 5000) != api.WAIT_OBJECT_0:
                incomplete.append(f"captured fixture {pid} did not confirm exit")
        except OSError as error:
            incomplete.append(f"captured fixture {pid}: {error}")
        finally:
            try:
                api.CloseHandle(handle)
            except OSError as error:
                incomplete.append(f"close fixture {pid}: {error}")
    if incomplete:
        raise RuntimeError("fixture cleanup unconfirmed; no PID-based termination attempted: " + "; ".join(incomplete))


def main():
    # Held by review: the controller and its children still inherit the real profile, credentials, Python startup
    # and cwd, and a failed client close can skip cleanup. Refuse until the isolated version lands.
    print("windows_background_smoke: not isolated yet; use the desktop checklist in docs/windows.md", file=sys.stderr)
    raise SystemExit(77)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--vqro", type=Path, required=True)
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("requires native Windows CPython 3.11+")
    import _winapi
    binary = args.vqro.resolve()
    provider = binary.with_name("vqro-herdr.exe")
    if not binary.is_file() or not provider.is_file():
        parser.error("build vqro.exe and its sibling vqro-herdr.exe first")
    with tempfile.TemporaryDirectory(prefix="vqro-win-background-") as temporary:
        root = Path(temporary)
        declarations = root / "extensions"
        declarations.mkdir()
        reports = root / "reports"
        reports.mkdir()
        fixture = root / "probe.py"
        fixture.write_text(FIXTURE, encoding="utf-8")
        common = [sys.executable, "-u", str(fixture), str(ROOT / "extensions")]
        declarations.joinpath("defaults.json").write_text(json.dumps({"version": 1, "extensions": [
            {"name": "probe", "argv": common + ["extension", str(reports)]},
            {"name": "connector", "kind": "provider", "argv": [str(provider), "--name", "console-fixture",
                "--hc", sys.executable, "--hc-arg", "-u", "--hc-arg", str(fixture),
                "--hc-arg", str(ROOT / "extensions"), "--hc-arg", "native", "--hc-arg", str(reports)]}
        ]}), encoding="utf-8")
        env = {k: v for k, v in os.environ.items() if not k.startswith(("VQRO_", "HC_", "HERDR_"))}
        # No real provider is declared; nevertheless fence inherited backend reachability.
        for key in ("XDG_RUNTIME_DIR", "XDG_STATE_HOME", "XDG_CONFIG_HOME"):
            path = root / key
            path.mkdir()
            env[key] = str(path)
        endpoint = r"\\.\pipe\vqro-background-" + uuid.uuid4().hex
        env.update(VQRO_STATE_DIR=str(root / "state"), VQRO_EXTENSIONS_DIR=str(declarations),
                   VQRO_SOCKET=endpoint, HC_SESSION="windows-background-fixture",
                   HC_SOCKET=r"\\.\pipe\hc-background-" + uuid.uuid4().hex)
        handles = {}
        rows = {}
        client = None
        core = None
        summary = None

        def collect():
            for path in reports.glob("*.json"):
                if path.name in rows:
                    continue
                try:
                    row = json.loads(path.read_text())
                except (OSError, json.JSONDecodeError):
                    continue  # Fixture may still be writing; no effects are repeated.
                rows[path.name] = row
                # The extension waits for its short-lived helper and verifies its exit code.
                # Never open/terminate that helper by a potentially recycled, already-dead PID.
                if row["mode"] == "helper":
                    continue
                # Observation only; never acquire termination access based on a reported PID.
                handle = _winapi.OpenProcess(_winapi.SYNCHRONIZE, False, row["pid"])
                handles[row["pid"]] = handle
            assert all(row["console"] == 0 for row in rows.values()), rows
            return rows

        try:
            with (root / "core.log").open("wb") as log:
                core = subprocess.Popen([str(binary), "serve"], env=env, stdin=subprocess.DEVNULL,
                                        stdout=log, stderr=log,
                                        creationflags=subprocess.DETACHED_PROCESS | subprocess.CREATE_NEW_PROCESS_GROUP)
            def connect():
                try:
                    return Client(endpoint)
                except OSError:
                    if core.poll() is not None:
                        raise RuntimeError((root / "core.log").read_text(errors="replace"))
                    return None
            client = wait_for(connect, "private core did not start")
            def enough(count):
                collect()
                return all(sum(row["mode"] == mode for row in rows.values()) >= count
                           for mode in ("extension", "helper", "native"))
            wait_for(lambda: enough(1), "first background generation did not report")
            # Restart only these private declarations, not OS consoles or backend terminals.
            for name in ("probe", "connector"):
                client.call("command.invoke", {"name": "vqro.extension.restart", "args": {"name": name}})
            wait_for(lambda: enough(2), "replacement background generation did not report")
            log = root / "state" / "ext-probe.log"
            wait_for(lambda: log.exists() and "helper stdout: stdio works" in log.read_text()
                     and "extension stderr" in log.read_text(), "supervisor stopped draining stdio")
            snapshot = client.call("workspace.snapshot")
            assert not snapshot["resources"], "fixture must not create workspace terminals"
            client.call("core.shutdown")
            core.wait(timeout=10)
            assert core.returncode == 0, core.returncode
            wait_for(lambda: all(_winapi.WaitForSingleObject(h, 0) == _winapi.WAIT_OBJECT_0
                                 for h in handles.values()), "fixture children survived shutdown")
            summary = {"result": "pass", "fixture_reports": list(rows.values()),
                       "restart": "pass", "stdio": "pass", "cleanup": "pass"}
        finally:
            if client:
                if core and core.poll() is None:
                    try:
                        client.call("core.shutdown")
                    except (OSError, RuntimeError, ValueError):
                        pass
                client.close()
            if core:
                try:
                    core.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    core.kill()
                    core.wait(timeout=5)
            # Never open late PID reports or terminate fixture handles. Only the Popen-owned core
            # above can be killed; uncertain fixture cleanup is a visible failure, not a guessed effect.
            unobserved = sorted(path.name for path in reports.glob("*.json") if path.name not in rows)
            close_fixture_handles(handles, _winapi, unobserved=unobserved)
        # Do not emit a passing result until failure-path cleanup also confirmed all observed exits.
        print(json.dumps(summary, indent=2))


if __name__ == "__main__":
    main()
