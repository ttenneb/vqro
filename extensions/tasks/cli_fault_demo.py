#!/usr/bin/env python3
"""CLI-only socket characterization. Fixture replies are NOT task/host acceptance."""
import argparse
import json
from pathlib import Path
import socket
import subprocess
import tempfile
import threading

SCOPE = {"class": "agent", "terminal_id": "fixture_terminal", "agent": "pi", "execution": "fixture:10:100"}


def response(value):
    return {"id": "cli:task", "result": {"type": "extension_action_invoked", "value": value}}


def success(value):
    return response({"ok": True, "value": value})


def run_case(binary, name, replies, expected_calls, expected_stderr):
    with tempfile.TemporaryDirectory(prefix="vqro-cli-fixture-") as temporary:
        root = Path(temporary)
        for directory in ("home", "config", "state", "run"):
            (root / directory).mkdir(mode=0o700)
        config = root / "config/config.toml"
        config.write_text("[update]\nversion_check = false\nmanifest_check = false\n")
        # The command is contributed by Tasks, not hardcoded in the binary.
        # Link only its local CLI declaration into this private fixture registry;
        # fake replies below remain transport characterization, not admission.
        package = Path(__file__).resolve().parent
        for app in ("vqro", "vqro-dev"):
            directory = root / "config" / app
            directory.mkdir()
            (directory / "plugins.json").write_text(json.dumps([{
                "plugin_id": "vqro.tasks", "name": "Tasks", "version": "0.1.0",
                "manifest_path": str(package / "vqro-extension.toml"),
                "plugin_root": str(package), "enabled": True,
            }]))
        env = {"HOME": str(root / "home"), "XDG_CONFIG_HOME": str(root / "config"),
               "XDG_STATE_HOME": str(root / "state"), "XDG_RUNTIME_DIR": str(root / "run"),
               "VQRO_CONFIG_PATH": str(config), "VQRO_SOCKET_PATH": str(root / "api.sock"),
               "VQRO_CLIENT_SOCKET_PATH": str(root / "client.sock"), "PATH": "/usr/bin:/bin"}
        observed, failures = [], []
        stop = threading.Event()
        with socket.socket(socket.AF_UNIX) as listener:
            listener.bind(str(root / "api.sock"))
            listener.listen()
            listener.settimeout(.1)
            def serve():
                while not stop.is_set():
                    try:
                        stream, _ = listener.accept()
                    except socket.timeout:
                        continue
                    with stream:
                        stream.settimeout(5)
                        try:
                            request = json.loads(stream.makefile("rb").readline())
                            observed.append(request)
                            index = len(observed) - 1
                            reply = replies[index] if index < len(replies) else {"id": "cli:task", "error": {"code": "unexpected_retry", "message": "unexpected retry"}}
                            if reply is not None:
                                stream.sendall((json.dumps(reply) + "\n").encode())
                        except Exception as error:
                            failures.append(str(error))
            thread = threading.Thread(target=serve)
            thread.start()
            try:
                result = subprocess.run([str(binary), "task", "create", "Boundary fixture", "--done-when", "Outcome is recorded.", "--op", "boundary-op", "--caller", json.dumps(SCOPE)], env=env, cwd=root, capture_output=True, text=True, timeout=18)
            finally:
                stop.set()
                thread.join(timeout=6)
            assert not thread.is_alive() and not failures, (name, failures)
        assert result.returncode == 1, (name, result.stdout, result.stderr)
        assert expected_stderr in result.stderr, (name, result.stderr)
        assert len(observed) == expected_calls, (name, observed)
        assert observed[0]["params"]["payload"] == {"action": "whoami"}
        if expected_calls == 2:
            write = observed[1]["params"]
            assert set(write) == {"service_id", "payload"}
            assert write["payload"]["operation_id"] == "boundary-op"
            assert write["payload"]["expected_caller"] == SCOPE
        if "outcome unknown" in expected_stderr:
            assert "original caller:" in result.stderr and "lookup first: vqro task op boundary-op --caller" in result.stderr
        return {"case": name, "passed": True, "observed_requests": expected_calls}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--vqro", required=True, type=Path)
    binary = parser.parse_args().vqro.resolve()
    cases = [
        ("unknown_preflight_never_writes", [success({"caller": {"class": "unknown"}})], 1, "no write attempted"),
        ("lost_preflight_never_writes", [None], 1, "no write attempted"),
        ("replacement_preflight_never_retries_original_create", [success({"caller": {**SCOPE, "execution": "fixture:11:101"}})], 1, "original caller changed; no write attempted"),
        ("preflight_drift_never_rebinds_or_retries", [success({"caller": SCOPE}), response({"ok": False, "error": "caller changed before mutation"})], 2, "caller changed before mutation"),
        ("lost_write_reply_preserves_original_scope_no_retry", [success({"caller": SCOPE}), None], 2, "outcome unknown; op boundary-op"),
    ]
    print(json.dumps({"kind": "CLI-only fixture characterization, not product acceptance", "results": [run_case(binary, *case) for case in cases]}, indent=2))


if __name__ == "__main__":
    main()
