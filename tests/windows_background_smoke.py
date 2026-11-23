r"""Private native Windows no-console fixture (no real backend, agent or terminal).

  python -I -B tests/windows_background_smoke.py --vqro target/debug/vqro.exe

The -I bootstrap starts a second -I controller with an explicit private profile/env/cwd.
All evidence is retained, including on failure. This is not a sandbox for arbitrary code.
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
PROFILE_DIRS = {"HOME": "home", "USERPROFILE": "home", "APPDATA": "appdata",
                "LOCALAPPDATA": "localappdata", "TEMP": "tmp", "TMP": "tmp",
                "XDG_RUNTIME_DIR": "runtime", "XDG_STATE_HOME": "xdg-state", "XDG_CONFIG_HOME": "config"}


def private_environment(root, binary, python, source, *, create=True):
    """Allowlist tool/OS lookup only; never copy caller auth, profile or Python startup settings."""
    root = Path(root).resolve()
    system = next((value for key, value in source.items() if key.upper() == "SYSTEMROOT"), None)
    if not system or not Path(system).is_absolute():
        raise RuntimeError("an absolute SystemRoot is required for the native Windows tools")
    env = {"SYSTEMROOT": system, "WINDIR": system}
    env["PATH"] = os.pathsep.join([str(Path(binary).parent), str(Path(python).parent), str(Path(system) / "System32")])
    for key, name in PROFILE_DIRS.items():
        path = root / name
        if create:
            path.mkdir(exist_ok=True)
        env[key] = str(path)
    if create:
        for name in ("state", "extensions", "reports"):
            (root / name).mkdir(exist_ok=True)
    env.update(VQRO_STATE_DIR=str(root / "state"), VQRO_EXTENSIONS_DIR=str(root / "extensions"),
               VQRO_SOCKET=r"\\.\pipe\vqro-background-" + uuid.uuid5(uuid.NAMESPACE_URL, str(root)).hex)
    return env


def require_private_controller(root, binary, *, environ=None, cwd=None, isolated=None):
    actual = dict(os.environ if environ is None else environ)
    if not (sys.flags.isolated if isolated is None else isolated):
        raise RuntimeError("controller requires Python -I")
    if Path(Path.cwd() if cwd is None else cwd).resolve() != Path(root).resolve():
        raise RuntimeError("controller requires its private cwd")
    expected = private_environment(root, binary, sys.executable, actual, create=False)
    if {key.upper(): value for key, value in actual.items()} != expected:
        raise RuntimeError("controller requires the explicit private environment; inherited settings refused")
    saved = json.loads((Path(root) / "environment.json").read_text(encoding="utf-8"))
    if saved != expected:
        raise RuntimeError("private child environment does not match controller configuration")
    return expected


# Restore the known private configuration even after the connector deliberately clears VQRO_*.
# Do this before importing ctypes/the repository helper or querying any native console state.
FIXTURE = r'''
import json, os, pathlib, subprocess, sys
mode, reports = sys.argv[2], pathlib.Path(sys.argv[3])
root = reports.parent
private = json.loads((root / 'environment.json').read_text(encoding='utf-8'))
os.environ.clear()
os.environ.update(private)
os.chdir(root)
if not sys.flags.isolated:
    raise RuntimeError('fixture requires Python -I')
sys.path.insert(0, sys.argv[1])
from vqro_process import background_kwargs
import ctypes
get_console = ctypes.WinDLL('kernel32').GetConsoleWindow
get_console.restype = ctypes.c_void_p
report = {'mode': mode, 'pid': os.getpid(), 'console': get_console() or 0}
(reports / (mode + '-' + str(os.getpid()) + '.json')).write_text(json.dumps(report), encoding='utf-8')
if mode == 'extension':
    result = subprocess.run([sys.executable, '-I', '-B', __file__, sys.argv[1], 'helper', str(reports)],
                            env=private, cwd=root, stdin=subprocess.DEVNULL, capture_output=True,
                            timeout=10, check=True, **background_kwargs())
    print('helper stdout: ' + result.stdout.decode('utf-8').strip(), flush=True)
    print('extension stderr', file=sys.stderr, flush=True)
if mode == 'helper':
    print('stdio works', flush=True)
else:
    sys.stdin.buffer.read()
'''


class Client:
    def __init__(self, endpoint):
        # Imported only after require_private_controller, never by the ambient bootstrap/importing tests.
        sys.path.insert(0, str(ROOT / "extensions"))
        from vqro_transport import connect_endpoint
        self.pipe = connect_endpoint(endpoint, timeout=2)
        self.file = None
        try:
            self.file = self.pipe.makefile("rwb", buffering=0)
            self.identifier = 0
            self.call("hello", {"client": "windows-background-smoke"})
        except BaseException:
            self.close()
            raise

    def call(self, method, params=None):
        self.identifier += 1
        request = {"jsonrpc": "2.0", "id": self.identifier, "method": method, "params": params or {}}
        self.pipe.sendall((json.dumps(request) + "\n").encode())
        row = json.loads(self.file.readline(64 * 1024 * 1024 + 1))
        if row.get("id") != self.identifier or "error" in row:
            raise RuntimeError(row)
        return row["result"]

    def close(self):
        errors = []
        if self.file is not None:
            attempt(errors, "client file close", self.file.close)
        attempt(errors, "client pipe close", self.pipe.close)
        if errors:
            raise RuntimeError("; ".join(errors))


def attempt(errors, label, action):
    try:
        return action()
    except BaseException as error:
        errors.append(f"{label}: {type(error).__name__}: {error}")


def wait_for(check, description, seconds=15):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(.05)
    raise TimeoutError(description)


def close_fixture_handles(handles, api, *, unobserved=()):
    """Observation only; a PID report never authorizes termination of a later process."""
    incomplete = [f"unobserved report {name}" for name in unobserved]
    for pid, handle in handles.items():
        try:
            if api.WaitForSingleObject(handle, 5000) != api.WAIT_OBJECT_0:
                incomplete.append(f"captured fixture {pid} did not confirm exit")
        except BaseException as error:
            incomplete.append(f"captured fixture {pid}: {error}")
        finally:
            attempt(incomplete, f"close fixture {pid}", lambda: api.CloseHandle(handle))
    if incomplete:
        raise RuntimeError("fixture cleanup unconfirmed; no PID-based termination attempted: " + "; ".join(incomplete))


def cleanup_owned(client, core, handles, api, reports, rows):
    """Every resource gets its own attempt; one failure cannot bypass later cleanup."""
    errors = []
    if client is not None:
        if core is not None:
            def shutdown():
                if core.poll() is None:
                    client.call("core.shutdown")
            attempt(errors, "core protocol shutdown", shutdown)
        attempt(errors, "client close", client.close)
    if core is not None:
        try:
            core.wait(timeout=10)
        except BaseException as error:
            errors.append(f"core wait: {type(error).__name__}: {error}")
            attempt(errors, "owned core kill", core.kill)
            attempt(errors, "owned core reap", lambda: core.wait(timeout=5))
    unobserved = []
    def remaining_reports():
        unobserved.extend(sorted(path.name for path in reports.glob("*.json") if path.name not in rows))
    attempt(errors, "report inventory", remaining_reports)
    attempt(errors, "fixture handles", lambda: close_fixture_handles(handles, api, unobserved=unobserved))
    return errors


def declarations(root, provider, python):
    common = [str(python), "-I", "-B", "-u", str(root / "probe.py"), str(ROOT / "extensions")]
    native = ["-I", "-B", "-u", str(root / "probe.py"), str(ROOT / "extensions"), "native", str(root / "reports")]
    connector = [str(provider), "--name", "console-fixture", "--hc", str(python)]
    for arg in native:
        connector.extend(["--hc-arg", arg])
    return {"version": 1, "extensions": [
        {"name": "probe", "argv": common + ["extension", str(root / "reports")], "cwd": str(root)},
        {"name": "connector", "kind": "provider", "argv": connector, "cwd": str(root)},
    ]}


def run_controller(root, binary):
    env = require_private_controller(root, binary)  # before SDK/native API imports or a child launch
    import _winapi
    provider = binary.with_name("vqro-herdr.exe")
    reports = root / "reports"
    (root / "probe.py").write_text(FIXTURE, encoding="utf-8")
    (root / "extensions" / "defaults.json").write_text(json.dumps(declarations(root, provider, sys.executable)), encoding="utf-8")
    handles, rows = {}, {}
    client = core = summary = None
    failure = []

    def collect():
        for path in reports.glob("*.json"):
            if path.name in rows:
                continue
            try:
                row = json.loads(path.read_text(encoding="utf-8"))
            except (OSError, json.JSONDecodeError):
                continue
            rows[path.name] = row
            if row["mode"] == "helper":
                continue  # subprocess.run already waited for this short-lived helper
            handles[row["pid"]] = _winapi.OpenProcess(_winapi.SYNCHRONIZE, False, row["pid"])
        assert all(row["console"] == 0 for row in rows.values()), rows
        return rows

    try:
        with (root / "core.log").open("wb") as log:
            core = subprocess.Popen([str(binary), "serve"], env=env, cwd=root, stdin=subprocess.DEVNULL,
                                    stdout=log, stderr=log,
                                    creationflags=subprocess.DETACHED_PROCESS | subprocess.CREATE_NEW_PROCESS_GROUP)
        def connect():
            try:
                return Client(env["VQRO_SOCKET"])
            except OSError:
                if core.poll() is not None:
                    raise RuntimeError((root / "core.log").read_text(encoding="utf-8", errors="replace"))
                return None
        client = wait_for(connect, "private core did not start")
        def enough(count):
            collect()
            return all(sum(row["mode"] == mode for row in rows.values()) >= count
                       for mode in ("extension", "helper", "native"))
        wait_for(lambda: enough(1), "first background generation did not report")
        for name in ("probe", "connector"):
            client.call("command.invoke", {"name": "vqro.extension.restart", "args": {"name": name}})
        wait_for(lambda: enough(2), "replacement background generation did not report")
        log = root / "state" / "ext-probe.log"
        wait_for(lambda: log.exists() and "helper stdout: stdio works" in log.read_text(encoding="utf-8")
                 and "extension stderr" in log.read_text(encoding="utf-8"), "supervisor stopped draining stdio")
        snapshot = client.call("workspace.snapshot")
        assert not snapshot["resources"], "fixture must not create workspace terminals"
        client.call("core.shutdown")
        core.wait(timeout=10)
        assert core.returncode == 0, core.returncode
        wait_for(lambda: all(_winapi.WaitForSingleObject(h, 0) == _winapi.WAIT_OBJECT_0
                             for h in handles.values()), "fixture children survived shutdown")
        summary = {"result": "pass", "fixture_reports": list(rows.values()), "restart": "pass", "stdio": "pass"}
    except BaseException as error:
        failure.append(f"smoke: {type(error).__name__}: {error}")
    finally:
        failure.extend(cleanup_owned(client, core, handles, _winapi, reports, rows))
    result = {"result": "fail", "errors": failure} if failure else dict(summary, cleanup="pass")
    (root / "result.json").write_text(json.dumps(result, indent=2), encoding="utf-8")
    print(json.dumps(result, indent=2))
    return 1 if failure else 0


def launch_controller(root, binary, env, *, run=None):
    """Bootstrap has no endpoint/native SDK use; actual controller starts private, not inherited."""
    command = [sys.executable, "-I", "-B", str(Path(__file__).resolve()),
               "--vqro", str(binary), "--controller-root", str(root)]
    with (root / "controller.log").open("wb") as log:
        completed = (subprocess.run if run is None else run)(command, env=env, cwd=root,
            stdin=subprocess.DEVNULL, stdout=log, stderr=subprocess.STDOUT,
            creationflags=subprocess.CREATE_NO_WINDOW)
    return completed.returncode


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--vqro", type=Path, required=True)
    parser.add_argument("--controller-root", type=Path, help=argparse.SUPPRESS)
    args = parser.parse_args()
    if not sys.flags.isolated:
        parser.exit(77, "requires Python -I; inherited Python startup/user-site configuration is not supported\n")
    if os.name != "nt":
        parser.error("requires native Windows CPython 3.11+")
    binary = args.vqro.resolve()
    if not binary.is_file() or not binary.with_name("vqro-herdr.exe").is_file():
        parser.error("build vqro.exe and sibling vqro-herdr.exe first")
    if args.controller_root is not None:
        root = args.controller_root.resolve()
        try:
            return run_controller(root, binary)
        except Exception as error:
            result = {"result": "fail", "errors": [f"controller: {type(error).__name__}: {error}"]}
            (root / "result.json").write_text(json.dumps(result, indent=2), encoding="utf-8")
            print(json.dumps(result))
            return 1
    # Never auto-delete evidence, including when controller setup or cleanup fails.
    root = Path(tempfile.mkdtemp(prefix="vqro-win-background-")).resolve()
    print(f"Private smoke evidence retained: {root}", flush=True)
    env = private_environment(root, binary, sys.executable, os.environ)
    (root / "environment.json").write_text(json.dumps(env, indent=2), encoding="utf-8")
    code = launch_controller(root, binary, env)
    result = root / "result.json"
    if result.is_file():
        print(result.read_text(encoding="utf-8"))
    else:
        print(json.dumps({"result": "fail", "errors": ["controller produced no result; evidence retained"]}))
        return code or 1
    return code


if __name__ == "__main__":
    raise SystemExit(main())
