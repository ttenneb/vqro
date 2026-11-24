"""RPC admission/negotiation tests. Only private fake executables; never a model."""
import contextlib
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import time
import unittest
from unittest.mock import Mock, patch
import subprocess

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
from pi_bridge.testing import Plugin, controlled
from pi_bridge.program import Bridge
from pi_bridge import launch
from testing_setup import rpc_rule_notice, rpc_launches, RPC_RULE_NOTICE
from pi_testing import rpc_testing_args
from testing_unit import setup_status


def crt_split(command):
    """Independent MSVCRT whitespace/quote/backslash reader for encoded-boundary controls."""
    result, index = [], 0
    while index < len(command):
        while index < len(command) and command[index] in " \t":
            index += 1
        if index == len(command):
            break
        value, quoted = [], False
        while index < len(command) and (quoted or command[index] not in " \t"):
            slashes = 0
            while index < len(command) and command[index] == chr(92):
                slashes += 1
                index += 1
            if index < len(command) and command[index] == '"':
                value.extend(chr(92) * (slashes // 2))
                if slashes % 2:
                    value.append('"')
                else:
                    quoted = not quoted
                index += 1
            else:
                value.extend(chr(92) * slashes)
                if index < len(command) and (quoted or command[index] not in " \t"):
                    value.append(command[index])
                    index += 1
        result.append("".join(value))
    return result


class RpcTestingTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="rpc-testing-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.plugin = self.root / "tier1/vqro-testing.ts"
        self.skill = self.root / "tier1/skills/vqro-setup-testing/SKILL.md"
        self.skill.parent.mkdir(parents=True)
        self.skill.write_text("skill")
        self.plugin.write_text("plugin")
        launch._HELP.clear()
        launch._HELP_REASONS.clear()
        launch._WARNED.clear()

    def rows(self):
        return [{"name": "vqro-testing-status", "source": "extension", "sourceInfo": {"path": str(self.plugin)}},
                {"name": "skill:vqro-setup-testing", "source": "skill", "sourceInfo": {"path": str(self.skill)}}]

    def test_only_installed_regular_plugin_and_derived_skill(self):
        self.assertEqual(rpc_testing_args(self.root), ["--testing-plugin", str(self.plugin)])
        allowed = Plugin(self.plugin, self.root, "p:one")
        self.assertEqual(allowed.arguments(), ["-e", str(self.plugin), "--skill", str(self.skill)])
        for path in (self.root / "other.ts", self.root, self.skill):
            with self.subTest(path=path), self.assertRaises((ValueError, OSError)):
                Plugin(path, self.root)
        self.skill.unlink()
        with self.assertRaises(OSError):
            Plugin(self.plugin, self.root)
        self.assertEqual(rpc_testing_args(self.root), [])

    @unittest.skipIf(os.name == "nt", "unprivileged symlink creation is not portable")
    def test_plugin_symlink_cannot_escape_installed_root(self):
        outside = self.root / "outside.ts"
        outside.write_text("not the installed plugin")
        self.plugin.unlink()
        self.plugin.symlink_to(outside)
        with self.assertRaises(ValueError):
            Plugin(self.plugin, self.root)
        self.assertEqual(rpc_testing_args(self.root), [])

    def test_native_argv_never_accepts_explicit_interceptors(self):
        for flag in ("-e", "--extension=x", "--prompt-template=y", "--print", "-p"):
            with self.subTest(flag=flag), self.assertRaises(ValueError):
                controlled(["pi", flag])
        self.assertEqual(controlled(["pi", "--mode", "rpc"]), ["pi", "--mode", "rpc"])

    def test_legacy_native_environment_adds_exactly_two_names(self):
        names = ["VQRO_TESTING_SOCKET", "VQRO_TESTING_STATE_DIR", "VQRO_TESTING_EXTENSIONS_DIR", "VQRO_EXTENSION_TOKEN",
                 "VQRO_BRIDGE_TOKEN", "VQRO_STATE_DIR", "HERDR_SOCKET", "PATH"]
        bridge = Bridge(["fixture"], cwd=self.root, env=dict.fromkeys(names, "fixture"), token="fake-bootstrap")
        self.assertEqual(set(bridge.env), {"VQRO_TESTING_SOCKET", "VQRO_TESTING_STATE_DIR", "PATH"})

    def test_inventory_evidence_must_match_command_and_skill_paths(self):
        plugin = Plugin(self.plugin, self.root, "p:one")
        rows = self.rows()
        plugin.observe(rows[:1])
        self.assertEqual(plugin.fact()["state"], "unverified")
        plugin.observe(rows)
        self.assertEqual(plugin.fact()["state"], "loaded")
        rows[0]["sourceInfo"]["path"] = str(self.root / "other.ts")
        plugin.observe(rows)
        self.assertEqual(plugin.fact()["state"], "unverified")
        plugin.observe(None)
        self.assertEqual(plugin.fact()["state"], "unverified")

    def test_scope_is_per_launch_and_old_or_unavailable_bridges_never_clear_it(self):
        plugin = Plugin(self.plugin, self.root, "p:one")
        plugin.observe(self.rows())
        resource = {"id": "r1", "kind": "agent", "name": "Pi", "state": "live", "available": True,
                    "data": {"agent": {"testing_plugin": plugin.fact()}}}
        snapshot = {"rpc_resources": [resource]}
        self.assertEqual(rpc_rule_notice(snapshot), RPC_RULE_NOTICE)  # Never globally upgrade coverage.
        self.assertIsNone(rpc_rule_notice(snapshot, "p:one"))
        self.assertEqual(rpc_rule_notice(snapshot, "p:other"), RPC_RULE_NOTICE)
        resource["available"] = False
        self.assertEqual(rpc_rule_notice(snapshot, "p:one"), RPC_RULE_NOTICE)
        resource["available"] = True
        resource["data"]["agent"].pop("testing_plugin")  # Old bridge.
        self.assertEqual(rpc_rule_notice(snapshot, "p:one"), RPC_RULE_NOTICE)
        self.assertEqual(rpc_launches(snapshot)[0]["state"], "unverified")
        resource["data"] = None
        self.assertEqual(rpc_rule_notice(snapshot, "p:one"), RPC_RULE_NOTICE)

    def test_failed_resource_read_is_serializable_and_never_confirms_rpc_scope(self):
        class Client:
            def call(self, method, params=None):
                if method == "workspace.snapshot":
                    return {"epoch": "fixture", "entries": {}}
                raise ConnectionError("fixture resource lookup unavailable")
        status = setup_status(self.root / "state", self.root, client=Client(), hello={"server": "fixture"}, rpc_principal="p:one")
        json.dumps(status)
        self.assertFalse(status["rpc_testing"]["verified"])
        self.assertIn("lookup unavailable", status["rpc_testing"]["lookup_error"])
        self.assertIn(RPC_RULE_NOTICE, status["not_verified_reasons"])

    def _assert_cleanup_failure_does_not_veto_main(self, kill_error):
        binary = self.executable("print('fixture help')\n")
        process = Mock(returncode=None, stdout=io.BytesIO())
        process.wait.side_effect = [subprocess.TimeoutExpired([binary, "--help"], 2),
                                    subprocess.TimeoutExpired([binary, "--help"], 1)]
        if kill_error:
            process.kill.side_effect = OSError("fixture kill denied")
        reader = Mock()
        reader.is_alive.return_value = False
        args = ["--testing-plugin", str(self.plugin), "--", "pi", "--mode", "rpc"]
        # No actual process or Windows API is used. Exercise the real main/negotiation/cache path.
        with patch.object(launch, "exec_target", return_value=binary), \
                patch.object(launch, "_background_kwargs", return_value={"creationflags": 1234}), \
                patch.object(launch.subprocess, "Popen", return_value=process) as popen, \
                patch.object(launch.threading, "Thread", return_value=reader), \
                patch.object(launch.sys, "argv", ["launch.py", *args]), \
                patch.object(launch.os, "execv") as execute, contextlib.redirect_stderr(io.StringIO()) as notices:
            launch.main()
            execute.assert_called_once_with(binary, ["vqro-bridge-pi", "--", "pi", "--mode", "rpc"])
            self.assertEqual(launch.negotiated(args, binary), ["--", "pi", "--mode", "rpc"])
        popen.assert_called_once()
        self.assertEqual(popen.call_args.kwargs["creationflags"], 1234, "probe must use shared background flags")
        self.assertEqual(process.wait.call_count, 2)
        process.kill.assert_called_once()
        self.assertEqual(set(launch._HELP.values()), {False}, "cleanup failure must be negative-cached")
        warning = notices.getvalue()
        self.assertEqual(warning.count("NOT VERIFIED"), 1)
        self.assertIn("reap unconfirmed", warning)
        self.assertNotIn("reaped", warning)
        if kill_error:
            self.assertIn("kill failed; cleanup unconfirmed", warning)

    def test_hanging_help_and_kill_oserror_still_launch_once_without_testing(self):
        self._assert_cleanup_failure_does_not_veto_main(kill_error=True)

    def test_hanging_help_and_second_wait_timeout_still_launch_once_without_testing(self):
        self._assert_cleanup_failure_does_not_veto_main(kill_error=False)

    def _assert_warning_io_failure_does_not_veto_main(self, error_type, failing_operation):
        binary = self.executable("print('old fixture help')\n")
        process = Mock(returncode=0, stdout=io.BytesIO(b"old fixture help\n"))
        args = ["--testing-plugin", str(self.plugin), "--", "pi", "--mode", "rpc"]

        class Diagnostic:
            writes = flushes = 0
            def write(self, value):
                self.writes += 1
                if failing_operation == "write":
                    raise error_type("fixture diagnostic write failure")
                return len(value)
            def flush(self):
                self.flushes += 1
                if failing_operation == "flush":
                    raise error_type("fixture diagnostic flush failure")

        diagnostic = Diagnostic()
        with patch.object(launch, "exec_target", return_value=binary), \
                patch.object(launch, "_background_kwargs", return_value={}), \
                patch.object(launch.subprocess, "Popen", return_value=process) as popen, \
                patch.object(launch.sys, "argv", ["launch.py", *args]), \
                patch.object(launch.sys, "stderr", diagnostic), \
                patch.object(launch.os, "execv") as execute, contextlib.redirect_stdout(io.StringIO()) as stdout:
            launch.main()
            attempts = (diagnostic.writes, diagnostic.flushes)
            self.assertEqual(launch.negotiated(args, binary), ["--", "pi", "--mode", "rpc"])
            self.assertEqual((diagnostic.writes, diagnostic.flushes), attempts, "failed warning is attempted only once")
        execute.assert_called_once_with(binary, ["vqro-bridge-pi", "--", "pi", "--mode", "rpc"])
        popen.assert_called_once()
        self.assertEqual(set(launch._HELP.values()), {False})
        self.assertEqual(stdout.getvalue(), "", "never fall back to protocol stdout")
        if failing_operation == "write":
            self.assertEqual(attempts, (1, 0))
        else:
            self.assertGreaterEqual(attempts[0], 1)
            self.assertEqual(attempts[1], 1, "flush must occur inside the non-vetoing diagnostic guard")

    def test_warning_write_oserror_still_launches_once(self):
        self._assert_warning_io_failure_does_not_veto_main(OSError, "write")

    def test_warning_write_broken_pipe_still_launches_once(self):
        self._assert_warning_io_failure_does_not_veto_main(BrokenPipeError, "write")

    def test_warning_flush_oserror_still_launches_once(self):
        self._assert_warning_io_failure_does_not_veto_main(OSError, "flush")

    def test_warning_flush_broken_pipe_still_launches_once(self):
        self._assert_warning_io_failure_does_not_veto_main(BrokenPipeError, "flush")

    def _assert_main_lookup(self, windows, names, expected, advertised=True):
        cwd, tools = self.root / "cwd", self.root / "tools"
        cwd.mkdir(); tools.mkdir()
        for name in names:
            path = self.root / name
            path.write_bytes(b"inert fixture, never executed")
            path.chmod(0o700)
        wanted = str(self.root / expected) if expected else None
        process = Mock(returncode=0, stdout=io.BytesIO(b"--testing-plugin\n" if advertised else b"old help\n"))
        args = ["--testing-plugin", str(self.plugin), "--", "pi"]
        checked = []
        original_stat = os.stat
        def metadata(path, *args, **kwargs):
            checked.append(os.fspath(path))
            return original_stat(path, *args, **kwargs)
        with patch.object(launch.os, "name", "nt" if windows else "posix"), \
                patch.object(launch.os, "get_exec_path", return_value=[str(tools)]), \
                patch.object(launch.os, "getcwd", return_value=str(cwd)), \
                patch.dict(os.environ, {"PATHEXT": ".CMD;.EXE"}), \
                patch.object(launch.os, "stat", side_effect=metadata), \
                patch.object(launch, "_background_kwargs", return_value={}), \
                patch.object(launch.subprocess, "Popen", return_value=process) as popen, \
                patch.object(launch.sys, "argv", ["launch.py", *args]), \
                patch.object(launch.os, "execv") as execute, patch.object(launch.os, "execvp") as search, \
                contextlib.redirect_stderr(io.StringIO()):
            launch.main()
        self.assertFalse(any(path.startswith(str(cwd) + os.sep) for path in checked), "never prepend cwd")
        if wanted:
            popen.assert_called_once()
            self.assertEqual(popen.call_args.args[0], [wanted, "--help"])
            forwarded = args if advertised else ["--", "pi"]
            execute.assert_called_once_with(wanted, ["vqro-bridge-pi", *forwarded])
            search.assert_not_called()
        else:
            popen.assert_not_called()
            execute.assert_not_called()
            search.assert_called_once_with("vqro-bridge-pi", ["vqro-bridge-pi", "--", "pi"])

    def test_windows_cwd_shadow_cannot_advertise_for_the_path_bridge(self):
        self._assert_main_lookup(True, ["cwd/vqro-bridge-pi.exe", "tools/vqro-bridge-pi.exe"], "tools/vqro-bridge-pi.exe", advertised=False)

    def test_windows_crt_exe_precedes_cmd_regardless_of_pathext_order(self):
        self._assert_main_lookup(True, ["cwd/vqro-bridge-pi.exe", "tools/vqro-bridge-pi.exe", "tools/vqro-bridge-pi.cmd"], "tools/vqro-bridge-pi.exe")

    def test_windows_cmd_target_is_bound_to_the_same_probe_and_exec(self):
        self._assert_main_lookup(True, ["cwd/vqro-bridge-pi.exe", "tools/vqro-bridge-pi.cmd"], "tools/vqro-bridge-pi.cmd")

    def test_windows_extensionless_identity_is_conservatively_unverified(self):
        self._assert_main_lookup(True, ["cwd/vqro-bridge-pi.exe", "tools/vqro-bridge-pi", "tools/vqro-bridge-pi.exe"], None)

    def test_posix_path_order_does_not_add_cwd_or_pathext(self):
        self._assert_main_lookup(False, ["cwd/vqro-bridge-pi", "tools/vqro-bridge-pi", "tools/vqro-bridge-pi.cmd"], "tools/vqro-bridge-pi")

    def test_posix_does_not_expand_windows_suffixes(self):
        self._assert_main_lookup(False, ["cwd/vqro-bridge-pi", "tools/vqro-bridge-pi.exe", "tools/vqro-bridge-pi.cmd"], None)

    def _assert_optional_crt_guard(self, value, windows, omitted):
        launch._HELP.clear(); launch._HELP_REASONS.clear(); launch._WARNED.clear()
        binary = self.executable("print('fixture')\n")
        process = Mock(returncode=0, stdout=io.BytesIO(b"--testing-plugin\n"))
        prefix = ["--cwd", "ordinary bridge path with spaces"]
        native = ["pi", "ordinary native space", 'ordinary"quote', "ordinary\\\\trailing\\\\"]
        args = [*prefix, "--testing-plugin", value, "--", *native]
        expected = [*prefix, "--", *native] if omitted else args
        with patch.object(launch.os, "name", "nt" if windows else "posix"), \
                patch.object(launch, "exec_target", return_value=binary), \
                patch.object(launch, "_background_kwargs", return_value={}), \
                patch.object(launch.subprocess, "Popen", return_value=process) as popen, \
                patch.object(launch.sys, "argv", ["launch.py", *args]), \
                patch.object(launch.os, "execv") as execute, \
                contextlib.redirect_stderr(io.StringIO()) as notices, contextlib.redirect_stdout(io.StringIO()) as stdout:
            launch.main()
            self.assertEqual(launch.negotiated(args, binary), expected)
        execute.assert_called_once()
        self.assertEqual(execute.call_args.args[0], binary)
        encoded = execute.call_args.args[1]
        if windows:
            self.assertEqual(crt_split(" ".join(encoded)), ["vqro-bridge-pi", *expected])
            self.assertIn('"ordinary native space"', encoded)
        else:
            self.assertEqual(encoded, ["vqro-bridge-pi", *expected])
        self.assertEqual(stdout.getvalue(), "")
        self.assertEqual(notices.getvalue().count("NOT VERIFIED"), 1 if omitted else 0)
        if omitted:
            popen.assert_not_called()
        else:
            popen.assert_called_once()

    def test_windows_crt_encoding_roundtrips_spaced_quoted_empty_and_backslash_values(self):
        for value in ('C:\\Users\\First Last\\plugin.ts', 'C:\\safe\\a\\"quote\\plugin.ts', 'C:\\space dir\\', 'tab\tpath', ''):
            with self.subTest(value=value):
                self._assert_optional_crt_guard(value, True, False)

    def test_windows_embedded_nul_omits_only_optional_field_and_launches_once(self):
        self._assert_optional_crt_guard("bad\0plugin", True, True)

    def test_windows_apostrophe_and_unquoted_backslashes_are_literal_without_a_shell(self):
        for value in ("C:\\safe\\O'Brien\\plugin.ts", "C:\\safe\\trailing\\"):
            with self.subTest(value=value):
                self._assert_optional_crt_guard(value, True, False)

    def test_posix_optional_and_ordinary_arguments_are_unchanged_by_crt_guard(self):
        for value in ('space path', 'quote"and\\slashes\\', ''):
            with self.subTest(value=value):
                self._assert_optional_crt_guard(value, False, False)

    def executable(self, body):
        path = self.root / "bridge"
        path.write_text("#!" + sys.executable + "\n" + body)
        path.chmod(0o700)
        return str(path)

    @unittest.skipIf(os.name == "nt", "private shebang executable fixture")
    def test_help_probe_new_old_cache_and_mtime_invalidation(self):
        count = self.root / "probes"
        binary = self.executable("import pathlib\np=pathlib.Path(" + repr(str(count)) + ")\np.write_text(p.read_text()+'x' if p.exists() else 'x')\nprint('--testing-plugin')\n")
        args = ["--testing-plugin", str(self.plugin), "--", "pi", "--mode", "rpc"]
        self.assertEqual(launch.negotiated(args, binary), args)
        self.assertEqual(launch.negotiated(args, binary), args)
        self.assertEqual(count.read_text(), "x")
        binary = self.executable("print('older bridge')\n")
        with contextlib.redirect_stderr(io.StringIO()) as notices:
            self.assertEqual(launch.negotiated(args, binary), ["--", "pi", "--mode", "rpc"])
            self.assertEqual(launch.negotiated(args, binary), ["--", "pi", "--mode", "rpc"])
            self.assertEqual(launch.negotiated(["--testing-plugin=" + str(self.plugin), "--", "pi"], binary), ["--", "pi"])
        self.assertEqual(notices.getvalue().count("NOT VERIFIED"), 1)
        # A native flag is not mistaken for the wrapper's dedicated field.
        native = ["--", "pi", "--testing-plugin", str(self.plugin)]
        self.assertEqual(launch.negotiated(native, binary), native)

    @unittest.skipIf(os.name == "nt", "private shebang executable fixture")
    def test_help_probe_bounds_time_output_and_missing_executable(self):
        binary = self.executable("import time\ntime.sleep(10)\n")
        started = time.monotonic()
        self.assertFalse(launch.supports_testing(binary))
        self.assertLess(time.monotonic() - started, 3)
        binary = self.executable("print('x'*100000)\n")
        self.assertFalse(launch.supports_testing(binary))
        self.assertFalse(launch.supports_testing(str(self.root / "absent")))


if __name__ == "__main__":
    unittest.main()
