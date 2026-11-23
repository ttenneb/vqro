"""Powerless controls for native-smoke cleanup; never run the Windows fixture here."""
import ast
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path
import unittest
from unittest import mock
from types import SimpleNamespace

import windows_background_smoke as smoke


class FixtureCleanupTests(unittest.TestCase):
    def api(self, statuses):
        return SimpleNamespace(WAIT_OBJECT_0=0, WaitForSingleObject=mock.Mock(side_effect=statuses),
                               CloseHandle=mock.Mock(), OpenProcess=mock.Mock(), TerminateProcess=mock.Mock())

    def test_exited_fixture_handles_are_only_waited_and_closed(self):
        api = self.api([0, 0])
        smoke.close_fixture_handles({11: "first", 22: "second"}, api)
        self.assertEqual(api.WaitForSingleObject.call_args_list, [mock.call("first", 5000), mock.call("second", 5000)])
        self.assertEqual(api.CloseHandle.call_args_list, [mock.call("first"), mock.call("second")])
        api.OpenProcess.assert_not_called()
        api.TerminateProcess.assert_not_called()

    def test_timeout_and_wait_failure_close_every_handle_and_fail_visibly(self):
        api = self.api([258, OSError("wait failed"), 0])
        with self.assertRaisesRegex(RuntimeError, "cleanup unconfirmed.*11.*22"):
            smoke.close_fixture_handles({11: "first", 22: "second", 33: "third"}, api)
        self.assertEqual(api.CloseHandle.call_args_list, [mock.call("first"), mock.call("second"), mock.call("third")])
        api.OpenProcess.assert_not_called()
        api.TerminateProcess.assert_not_called()

    def test_late_reports_are_reported_not_opened_or_terminated(self):
        api = self.api([0])
        with self.assertRaisesRegex(RuntimeError, "cleanup unconfirmed.*unobserved report native-late.json"):
            smoke.close_fixture_handles({11: "first"}, api, unobserved=["native-late.json"])
        api.CloseHandle.assert_called_once_with("first")
        api.OpenProcess.assert_not_called()
        api.TerminateProcess.assert_not_called()

    def test_source_has_no_fixture_termination_or_termination_access(self):
        tree = ast.parse(Path(smoke.__file__).read_text(encoding="utf-8"))
        calls = [node for node in ast.walk(tree) if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)]
        self.assertFalse([node for node in calls if node.func.attr == "TerminateProcess"])
        opened = [node for node in calls if node.func.attr == "OpenProcess"]
        self.assertEqual(len(opened), 1)
        self.assertEqual(ast.unparse(opened[0].args[0]), "_winapi.SYNCHRONIZE")
        killed = [node for node in ast.walk(tree) if isinstance(node, ast.Attribute) and node.attr == "kill"]
        self.assertEqual([ast.unparse(node.value) for node in killed], ["core"])
        collect_calls = [node for node in ast.walk(tree) if isinstance(node, ast.Call)
                         and isinstance(node.func, ast.Name) and node.func.id == "collect"]
        self.assertEqual(len(collect_calls), 1)  # only the live observation/restart path
        self.assertFalse(collect_calls[0].keywords)


class IsolationTests(unittest.TestCase):
    def environment(self, root):
        source = {"SystemRoot": str(root / "operating-system"), "PATH": "caller-tools",
                  "HOME": "real-profile", "USERPROFILE": "real-profile", "APPDATA": "real-profile",
                  "LOCALAPPDATA": "real-profile", "TEMP": "real-temp", "PYTHONPATH": "caller-code",
                  "PYTHONSTARTUP": "caller-startup", "PYTHONHOME": "caller-python",
                  "OPENAI_API_KEY": "secret", "AWS_PROFILE": "real-auth", "CODEX_HOME": "real-auth",
                  "SSH_AUTH_SOCK": "real-auth", "HC_SOCKET": "real-backend", "HERDR_ENV": "real-session"}
        env = smoke.private_environment(root, root / "tools" / "vqro.exe", sys.executable, source)
        (root / "environment.json").write_text(json.dumps(env), encoding="utf-8")
        return env

    def test_private_allowlist_discards_ambient_auth_profiles_and_startup(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            env = self.environment(root)
            self.assertEqual(set(env), set(smoke.PROFILE_DIRS) | {"SYSTEMROOT", "WINDIR", "PATH",
                "VQRO_STATE_DIR", "VQRO_EXTENSIONS_DIR", "VQRO_SOCKET"})
            for key in smoke.PROFILE_DIRS:
                self.assertTrue(Path(env[key]).is_relative_to(root), key)
                self.assertTrue(Path(env[key]).is_dir(), key)
            self.assertNotIn("caller-tools", env["PATH"])
            self.assertTrue(env["VQRO_SOCKET"].startswith("\\\\.\\pipe\\vqro-background-"))
            self.assertFalse(any(key.startswith(("HC_", "HERDR_", "PYTHON")) for key in env))

    def test_controller_refuses_tainted_env_cwd_or_nonisolated_python_before_native(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binary = root / "tools" / "vqro.exe"
            env = self.environment(root)
            self.assertEqual(smoke.require_private_controller(root, binary, environ=env, cwd=root, isolated=True), env)
            for bad in [dict(env, OPENAI_API_KEY="secret"), dict(env, PYTHONSTARTUP="caller-code"),
                        dict(env, HOME="real-profile"), dict(env, HC_SOCKET="real-backend")]:
                with self.assertRaisesRegex(RuntimeError, "private environment"):
                    smoke.require_private_controller(root, binary, environ=bad, cwd=root, isolated=True)
            with self.assertRaisesRegex(RuntimeError, "private cwd"):
                smoke.require_private_controller(root, binary, environ=env, cwd=root.parent, isolated=True)
            with self.assertRaisesRegex(RuntimeError, "Python -I"):
                smoke.require_private_controller(root, binary, environ=env, cwd=root, isolated=False)
            self.assertTrue((root / "environment.json").is_file(), "failure evidence must survive")

    def test_bootstrap_passes_explicit_environment_cwd_and_isolated_python(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            env = self.environment(root)
            run = mock.Mock(return_value=SimpleNamespace(returncode=1))
            with mock.patch.object(subprocess, "CREATE_NO_WINDOW", 1234, create=True):
                self.assertEqual(smoke.launch_controller(root, root / "vqro.exe", env, run=run), 1)
            command = run.call_args.args[0]
            self.assertEqual(command[1:3], ["-I", "-B"])
            self.assertEqual(run.call_args.kwargs["env"], env)
            self.assertEqual(run.call_args.kwargs["cwd"], root)
            self.assertEqual(run.call_args.kwargs["creationflags"], 1234)
            self.assertTrue((root / "controller.log").is_file(), "failed controller evidence retained")

    def test_every_python_fixture_is_isolated_and_has_private_cwd(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            declared = smoke.declarations(root, root / "vqro-herdr.exe", "python.exe")["extensions"]
            self.assertTrue(all(row["cwd"] == str(root) for row in declared))
            self.assertEqual(declared[0]["argv"][1:3], ["-I", "-B"])
            connector = declared[1]["argv"]
            self.assertEqual(connector[connector.index("--hc-arg") + 1], "-I")
            tree = ast.parse(smoke.FIXTURE)
            native = next(i for i, node in enumerate(tree.body) if isinstance(node, ast.Import)
                          and any(alias.name == "ctypes" for alias in node.names))
            restores = [i for i, node in enumerate(tree.body) if isinstance(node, ast.Expr)
                        and isinstance(node.value, ast.Call) and ast.unparse(node.value.func) == "os.environ.update"]
            self.assertTrue(restores and restores[0] < native)
            spawned = next(node for node in ast.walk(tree) if isinstance(node, ast.Call)
                           and ast.unparse(node.func) == "subprocess.run")
            self.assertIn("-I", [item.value for item in spawned.args[0].elts if isinstance(item, ast.Constant)])
            self.assertTrue({"env", "cwd"}.issubset({keyword.arg for keyword in spawned.keywords}))


class IndependentCleanupTests(unittest.TestCase):
    def test_client_and_core_failures_never_skip_fixture_cleanup(self):
        api = FixtureCleanupTests().api([0])
        client = mock.Mock()
        client.call.side_effect = OSError("shutdown failed")
        client.close.side_effect = ValueError("client close failed")
        core = mock.Mock()
        core.poll.return_value = None
        core.wait.side_effect = [OSError("wait failed"), OSError("reap failed")]
        core.kill.side_effect = OSError("kill failed")
        reports = mock.Mock()
        reports.glob.side_effect = OSError("inventory failed")
        errors = smoke.cleanup_owned(client, core, {1: "fixture"}, api, reports, {})
        for stage in ["protocol shutdown", "client close", "core wait", "core kill", "core reap", "inventory"]:
            self.assertTrue(any(stage in error for error in errors), (stage, errors))
        client.close.assert_called_once()
        core.kill.assert_called_once()
        self.assertEqual(core.wait.call_count, 2)
        api.CloseHandle.assert_called_once_with("fixture")
        api.TerminateProcess.assert_not_called()

    def test_interrupt_during_close_still_waits_core_and_closes_fixtures(self):
        api = FixtureCleanupTests().api([KeyboardInterrupt(), 0])
        client, core, reports = mock.Mock(), mock.Mock(), mock.Mock()
        core.poll.return_value = 0
        client.close.side_effect = KeyboardInterrupt()
        reports.glob.return_value = []
        errors = smoke.cleanup_owned(client, core, {1: "first", 2: "second"}, api, reports, {})
        self.assertTrue(any("KeyboardInterrupt" in error for error in errors))
        core.wait.assert_called_once_with(timeout=10)
        self.assertEqual(api.CloseHandle.call_args_list, [mock.call("first"), mock.call("second")])

    def test_client_file_close_failure_still_closes_pipe(self):
        client = smoke.Client.__new__(smoke.Client)
        client.file, client.pipe = mock.Mock(), mock.Mock()
        client.file.close.side_effect = OSError("file close failed")
        with self.assertRaisesRegex(RuntimeError, "file close failed"):
            client.close()
        client.pipe.close.assert_called_once()

    def test_actual_finally_retains_fail_evidence_after_client_close_error(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            env = IsolationTests().environment(root)
            for generation in [1, 2]:
                for index, mode in enumerate(["extension", "helper", "native"]):
                    row = {"mode": mode, "pid": generation * 10 + index, "console": 0}
                    (root / "reports" / f"{mode}-{generation}.json").write_text(json.dumps(row), encoding="utf-8")
            (root / "state" / "ext-probe.log").write_text("helper stdout: stdio works\nextension stderr", encoding="utf-8")
            api = FixtureCleanupTests().api([0] * 20)
            api.SYNCHRONIZE = 1
            api.OpenProcess.side_effect = lambda access, inherit, pid: pid
            core = mock.Mock(returncode=0)
            core.poll.return_value = 0
            core.wait.return_value = 0
            client = mock.Mock()
            client.call.side_effect = lambda method, params=None: {"resources": {}} if method == "workspace.snapshot" else {}
            client.close.side_effect = OSError("close failure")
            with mock.patch.dict(sys.modules, {"_winapi": api}), \
                    mock.patch.object(smoke, "require_private_controller", return_value=env), \
                    mock.patch.object(smoke.subprocess, "Popen", return_value=core) as spawn, \
                    mock.patch.object(smoke.subprocess, "DETACHED_PROCESS", 8, create=True), \
                    mock.patch.object(smoke.subprocess, "CREATE_NEW_PROCESS_GROUP", 512, create=True), \
                    mock.patch.object(smoke, "Client", return_value=client), mock.patch("builtins.print"):
                self.assertEqual(smoke.run_controller(root, root / "vqro.exe"), 1)
            self.assertEqual(api.CloseHandle.call_count, 4)
            self.assertGreaterEqual(core.wait.call_count, 2)
            self.assertEqual(spawn.call_args.kwargs["env"], env)
            self.assertEqual(spawn.call_args.kwargs["cwd"], root)
            result = json.loads((root / "result.json").read_text(encoding="utf-8"))
            self.assertEqual(result["result"], "fail")
            self.assertTrue(any("close failure" in error for error in result["errors"]))
            self.assertTrue((root / "core.log").is_file(), "failure evidence retained")


if __name__ == "__main__":
    unittest.main()
