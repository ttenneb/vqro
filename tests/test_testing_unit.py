"""Mocked health checks; no core, backend or installed user state is accessed."""
import contextlib
import io
import json
import os
from pathlib import Path
import socket
import stat
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
import testing_unit as unit
import vqro_testing as shared


class HealthChecks(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.extensions = self.root / "extensions"
        self.state = self.root / "state"
        self.extensions.mkdir()
        self.state.mkdir()
        (self.extensions / "manifests").mkdir()
        (self.extensions / "sample.py").write_text("# fixture\n")
        (self.extensions / "manifests/sample.json").write_text('{"name":"sample"}')
        self.declarations = {"extension/sample": {"argv": ["python3", "sample.py"], "enabled": True}}
        self.facts = {"sample": {"display_status": "running", "enabled": True, "crashes": 0,
                                 "manifest": {"name": "sample"}, "dependencies": {"status": "ok"}}}
        self.snapshot = {"entries": {key: {"version": 1, "value": value}
                                     for key, value in self.declarations.items()}}
        self.client = Mock()
        self.client.call.side_effect = self.call

    def call(self, method, params=None):
        return {"hello": {"server": "vqro", "version": "fixture-v1"}, "extension.list": {"extensions": self.facts},
                "workspace.snapshot": self.snapshot}[method]

    def report(self, **kwargs):
        return unit.collect(self.state, self.extensions, client=self.client, **kwargs)

    def rows(self, result):
        return {r["id"]: r for r in result["rows"]}

    def test_live_report_only_reads_allowed_methods(self):
        result = self.report()
        self.assertEqual(result["verdict"], "pass")
        self.assertEqual(result["source"], "live")
        self.assertEqual([c.args[0] for c in self.client.call.call_args_list],
                         ["hello", "extension.list", "workspace.snapshot", "workspace.snapshot"])
        self.assertEqual(result["fingerprint"]["vqro"], {"server": "vqro", "version": "fixture-v1"})
        self.assertEqual(result["not_covered"][1], "sample: no tests installed: no coverage")
        self.assertEqual(self.rows(result)["tests/sample"]["status"], "skip")

    def test_every_status_mapping(self):
        for status, expected in [("running", "pass"), ("failed", "fail"), ("starting", "unverified"),
                                 ("provider-not-connected", "fail"), ("off", "skip"),
                                 ("stopped-by-safe-mode", "skip"), ("future-state", "unverified")]:
            with self.subTest(status=status):
                result = unit.status_row("sample", {"display_status": status, "enabled": True,
                                                    "failure": {"kind": "startup", "reason": "broken"},
                                                    "last_exits": ["exit 8"]})
                self.assertEqual(result["status"], expected)
                self.assertIn(status, result["evidence"])
                if status == "failed":
                    self.assertIn("startup", result["evidence"])
                    self.assertIn("exit 8", result["evidence"])

    def test_recent_crash_is_warning(self):
        result = unit.status_row("sample", {"display_status": "running", "crashes": 1})
        self.assertEqual(result["status"], "warn")
        self.assertIn("1 crashes", result["evidence"])

    def test_manifest_warnings_and_valid_manifest(self):
        for facts, status, evidence in [({"manifest_error": "bad metadata"}, "warn", "bad metadata"),
                                        ({}, "warn", "no manifest: no description"),
                                        ({"manifest": {"name": "sample"}}, "pass", "manifest available")]:
            with self.subTest(facts=facts):
                self.facts["sample"].update(manifest=None, manifest_error=None)
                self.facts["sample"].update(facts)
                item = self.rows(self.report())["manifest/sample"]
                self.assertEqual((item["status"], item["evidence"]), (status, evidence))

    def test_dependency_statuses_and_unmet_suggestion(self):
        for status, expected in [("missing", "fail"), ("degraded", "warn"), ("ok", "pass"), ("future", "unverified")]:
            with self.subTest(status=status):
                field = "works_with" if status == "degraded" else "needs"
                facts = {"enabled": True, "dependencies": {"status": status, field: [
                    {"cap": "command/fixture", "status": "satisfied" if status == "ok" else "missing", "suggest": "fixture"}]}}
                item = unit.dependency_row("sample", facts)
                self.assertEqual(item["status"], expected)
                if status != "ok":
                    self.assertIn("command/fixture", item["evidence"])
                self.assertEqual(item["advice"], None if status == "ok" else "fixture")
        self.assertEqual(unit.dependency_row("sample", {"dependencies": None})["status"], "skip")

    def test_ui_dependency_ignores_only_the_checkers_own_labels(self):
        for labels, expected in [(["test-unit"], "pass"), (["test-unit", "test-integration", "test-perf"], "pass"),
                                 (["test-unit", "other-client"], "warn"), (["other-client"], "warn"), ([], "warn")]:
            for kind in ("unknown_in", "lacking_in"):
                with self.subTest(labels=labels, kind=kind):
                    dependencies = {"status": "degraded", "needs": [
                        {"cap": "ui/vqro.items", "status": "unknown" if kind == "unknown_in" else "lacking", kind: labels}]}
                    original = json.loads(json.dumps(dependencies))
                    item = unit.dependency_row("sample", {"dependencies": dependencies})
                    self.assertEqual(item["status"], expected)
                    self.assertEqual(dependencies, original)
                    if "other-client" in labels:
                        self.assertIn("other-client", item["evidence"])
                        self.assertNotIn("test-unit", item["evidence"])
        real_missing = {"status": "missing", "needs": [
            {"cap": "ui/vqro.items", "status": "unknown", "unknown_in": ["test-unit"]},
            {"cap": "ability/missing", "status": "missing", "providers": []}]}
        self.assertEqual(unit.dependency_row("sample", {"dependencies": real_missing})["status"], "fail")
        real_ui = {"status": "ok", "needs": [{"cap": "ui/vqro.items", "status": "unknown", "unknown_in": ["other-client"]}]}
        self.assertEqual(unit.dependency_row("sample", {"dependencies": real_ui})["status"], "warn")

    def test_disabled_paths_warn_without_runtime_checks(self):
        self.facts["sample"].update(enabled=False, display_status="off")
        self.declarations["extension/sample"]["argv"] = ["/not/a/program"]
        with patch.object(unit, "tests_row", side_effect=AssertionError("must not execute")):
            result = self.report()
        rows = self.rows(result)
        self.assertEqual(result["verdict"], "unverified")
        self.assertEqual(rows["status/sample"]["status"], "skip")
        self.assertEqual(rows["argv/sample"]["status"], "warn")
        self.assertNotIn("deps/sample", rows)
        self.assertNotIn("log/sample", rows)
        self.assertNotIn("tests/sample", rows)
        self.assertEqual(self.report(strict=True)["verdict"], "fail")

    def test_safe_mode_warning_and_skip(self):
        self.facts["sample"].update(safe_mode=True, display_status="stopped-by-safe-mode")
        result = self.report()
        self.assertEqual(self.rows(result)["core/safe_mode"]["status"], "warn")
        self.assertEqual(result["verdict"], "unverified")

    def test_verdict_precedence_and_exit_codes(self):
        for statuses, strict, count, error, expected in [
            (["pass"], False, 1, False, "pass"), (["warn"], False, 1, False, "unverified"),
            (["skip", "unverified"], False, 2, False, "unverified"),
            (["pass", "warn"], False, 2, False, "pass"), (["pass", "warn"], True, 2, False, "fail"),
            (["pass", "fail"], False, 2, False, "fail"), (["pass"], False, 0, False, "error"),
            (["pass"], False, 1, True, "error")]:
            with self.subTest(statuses=statuses, strict=strict, count=count, error=error):
                rows = [shared.row("status", str(i), s, "fixture") for i, s in enumerate(statuses)]
                rows += [shared.row("manifest", "sample", "pass", "metadata"), shared.row("core", "reachable", "pass", "live")]
                result = shared.receipt(rows, source="live", started_at=shared.utc_now(), started=time.monotonic(),
                                        fingerprint_value={}, inputs=[], not_covered=[], extension_count=count,
                                        strict=strict, error=error)
                self.assertEqual(result["verdict"], expected)
        self.assertEqual(shared.EXIT_CODES, {"pass": 0, "fail": 1, "unverified": 77, "error": 2})

    def test_zero_extensions_is_error(self):
        self.snapshot["entries"] = {}
        self.facts.clear()
        self.assertEqual(self.report()["verdict"], "error")

    def test_static_success_does_not_verify_starting_extension(self):
        self.facts["sample"]["display_status"] = "starting"
        self.assertEqual(self.report()["verdict"], "unverified")

    def test_unknown_status_is_preserved(self):
        self.facts["sample"]["display_status"] = "future-status"
        result = self.report()
        self.assertEqual(self.rows(result)["status/sample"]["evidence"], "future-status")
        self.assertEqual(result["verdict"], "unverified")

    def test_strict_changes_manifest_warning_to_failure(self):
        self.facts["sample"]["manifest_error"] = "broken manifest"
        self.assertEqual(self.report()["verdict"], "pass")
        self.assertEqual(self.report(strict=True)["verdict"], "fail")

    def test_argv_missing_program_and_script(self):
        self.assertEqual(unit.argv_row("x", {"argv": []}, self.extensions)["status"], "fail")
        item = unit.argv_row("x", {"argv": ["vqro-missing"]}, self.extensions, path="")
        self.assertEqual(item["status"], "fail")
        item = unit.argv_row("x", {"argv": ["python3", "missing.py"]}, self.extensions)
        self.assertIn("missing script", item["evidence"])
        self.assertEqual(item["status"], "fail")

    def test_argv_cwd_and_python_flags(self):
        directory = self.root / "cwd"
        directory.mkdir()
        (directory / "fixture.py").write_text("pass\n")
        item = unit.argv_row("x", {"argv": ["python3", "-u", "fixture.py"], "cwd": str(directory)}, self.extensions)
        self.assertEqual(item["status"], "pass")
        self.assertIsNone(shared.script_path({"argv": ["python3", "-m", "fixture"]}, self.extensions))
        self.assertIn("missing cwd", unit.argv_row("x", {"argv": ["python3"], "cwd": "/missing/cwd"}, self.extensions)["evidence"])

    def test_vqro_program_sibling_then_path(self):
        sibling_dir = self.root / "bin"
        path_dir = self.root / "path"
        sibling_dir.mkdir()
        path_dir.mkdir()
        for directory in (sibling_dir, path_dir):
            (directory / "vqro-fixture").write_text("fixture")
            (directory / "vqro-fixture").chmod(0o700)
        kwargs = {"binary": str(sibling_dir / "vqro"), "path": str(path_dir)}
        self.assertEqual(shared.resolve_program("vqro-fixture", self.extensions, **kwargs).resolve(), (sibling_dir / "vqro-fixture").resolve())
        (sibling_dir / "vqro-fixture").unlink()
        self.assertEqual(shared.resolve_program("vqro-fixture", self.extensions, **kwargs).resolve(), (path_dir / "vqro-fixture").resolve())

    def test_absolute_program_requires_executable(self):
        program = self.root / "program"
        program.write_text("fixture")
        self.assertIsNone(shared.resolve_program(str(program), self.extensions))
        program.chmod(0o700)
        self.assertEqual(shared.resolve_program(str(program), self.extensions).resolve(), program.resolve())

    def test_dev_tree_and_worktree_warning(self):
        for directory in (self.root / "checkout", self.root / ".worktrees" / "branch"):
            directory.mkdir(parents=True)
            if directory.name == "checkout":
                (directory / ".git").mkdir()
                (directory / "extensions").mkdir()
                (directory / "extensions/vqro_ext.py").write_text("# vqro checkout marker\n")
            script = directory / "fixture.py"
            script.write_text("pass\n")
            item = unit.argv_row("x", {"argv": ["python3", str(script)]}, self.extensions)
            self.assertEqual(item["status"], "warn")
            self.assertIn("works only from a dev tree?", item["evidence"])
        self.assertFalse(unit.dev_tree(self.extensions / "sample.py", self.extensions))

    def test_dotfiles_home_is_not_a_dev_tree(self):
        home = self.root / "home"
        program = home / ".local/bin/vqro-herdr"
        program.parent.mkdir(parents=True)
        (home / ".git").mkdir()
        program.write_text("fixture")
        program.chmod(0o700)
        result = unit.argv_row("herdr", {"argv": [str(program)]}, self.extensions)
        self.assertEqual(result["status"], "pass")
        self.assertFalse(unit.dev_tree(program, self.extensions))

    def test_log_tail_is_bounded_and_uses_last_match(self):
        log = self.state / "ext-sample.log"
        log.write_bytes(b"Traceback old\n" + b"x" * (unit.LOG_BYTES + 1))
        self.assertEqual(unit.log_row("sample", self.state)["status"], "pass")
        with log.open("ab") as stream:
            stream.write(b"\nTraceback new\npanicked " + b"x" * 400 + b"\n")
        item = unit.log_row("sample", self.state)
        self.assertEqual(item["status"], "warn")
        self.assertTrue(item["evidence"].startswith("panicked"))
        self.assertEqual(len(item["evidence"]), 300)
        self.assertEqual(unit.log_row("missing", self.state)["status"], "skip")

    def install_test(self, source):
        directory = self.extensions / "tests" / "sample"
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "test_fixture.py").write_text(source, encoding="utf-8")
        return directory

    def test_tests_runner_pass_and_no_bytecode_writes(self):
        directory = self.install_test("import unittest\nclass Tests(unittest.TestCase):\n def test_ok(self): self.assertTrue(True)\n")
        self.assertEqual(unit.tests_row("sample", self.extensions)["status"], "pass")
        self.assertFalse((directory / "__pycache__").exists())

    def test_tests_runner_failure_and_bounded_output(self):
        self.install_test("import unittest\nprint('x' * 100000)\nclass Tests(unittest.TestCase):\n def test_bad(self): self.fail('negative control')\n")
        item = unit.tests_row("sample", self.extensions)
        self.assertEqual(item["status"], "fail")
        self.assertIn("exit 1", item["evidence"])
        self.assertLessEqual(len(item["evidence"]), 300)
        self.assertEqual(self.report()["verdict"], "fail")

    def test_tests_runner_timeout(self):
        self.install_test("import time\ntime.sleep(60)\n")
        start = time.monotonic()
        item = unit.tests_row("sample", self.extensions, timeout=0.1)
        self.assertEqual(item["status"], "fail")
        self.assertIn("timeout", item["evidence"])
        self.assertLess(time.monotonic() - start, 3)

    def test_extension_tests_share_one_budget_but_cli_keeps_full_timeouts(self):
        for name in ("sample", "second", "third"):
            directory = self.extensions / "tests" / name
            directory.mkdir(parents=True)
            (directory / "test_fixture.py").write_text("# test fixture\n")
            if name != "sample":
                self.snapshot["entries"]["extension/" + name] = {"version": 1, "value": self.declarations["extension/sample"]}
                self.facts[name] = dict(self.facts["sample"])
        clock = [100.0]
        timeouts = []

        def run(*args, **kwargs):
            timeouts.append(kwargs["timeout"])
            clock[0] += 5 if len(timeouts) == 1 else 15
            return Mock(returncode=0)

        with patch.object(unit.time, "monotonic", side_effect=lambda: clock[0]), patch.object(unit.subprocess, "run", side_effect=run):
            result = self.report(tests_budget=20)
            self.assertEqual(timeouts, [20, 15])
            skipped = self.rows(result)["tests/third"]
            self.assertEqual(skipped["status"], "unverified")
            self.assertEqual(skipped["evidence"], "not run: command time budget; run the CLI for full tests")
            self.assertIn("third: " + skipped["evidence"], result["not_covered"])
            timeouts.clear()
            self.report()
            self.assertEqual(timeouts, [120, 120, 120])
        self.assertEqual(unit.tests_row("absent", self.extensions, timeout=0)["status"], "skip")

    def test_budget_cut_short_is_unverified_not_fail(self):
        directory = self.extensions / "tests" / "sample"
        directory.mkdir(parents=True)
        (directory / "test_fixture.py").write_text("# test fixture\n")

        def run(*args, **kwargs):
            raise unit.subprocess.TimeoutExpired(args[0], kwargs["timeout"])

        with patch.object(unit.subprocess, "run", side_effect=run):
            budgeted = self.rows(self.report(tests_budget=5))["tests/sample"]
            own_limit = self.rows(self.report())["tests/sample"]
        self.assertEqual(budgeted["status"], "unverified")
        self.assertIn("command time budget", budgeted["evidence"])
        self.assertEqual(own_limit["status"], "fail")

    def test_tests_runner_preserves_non_ascii_diagnostic_bytes(self):
        self.install_test("print('café λ')\nimport unittest\nclass Tests(unittest.TestCase):\n def test_ok(self): pass\n")
        item = unit.tests_row("sample", self.extensions)
        self.assertEqual(item["status"], "pass", item)
        self.assertIn("café λ", item["evidence"])

    def test_tests_runner_uses_current_interpreter_and_utf8_diagnostics(self):
        self.install_test("import unittest\nclass Tests(unittest.TestCase):\n def test_ok(self): pass\n")
        with patch.object(unit.subprocess, "run", return_value=Mock(returncode=0)) as run:
            self.assertEqual(unit.tests_row("sample", self.extensions)["status"], "pass")
        self.assertEqual(run.call_args.args[0][0], sys.executable)
        self.assertEqual(run.call_args.kwargs["env"]["PYTHONIOENCODING"], "utf-8")

    def test_tests_runner_scrubs_all_live_environment(self):
        self.install_test("import os, pathlib, unittest\nclass Tests(unittest.TestCase):\n"
                          " def test_env(self):\n"
                          "  self.assertFalse(any(k.startswith(('VQRO_', 'HC_', 'HERDR_')) for k in os.environ if k != 'VQRO_TEST_EXTENSIONS_DIR'))\n"
                          "  self.assertEqual(set(os.environ) - {'LC_CTYPE'}, {'PATH', 'HOME', 'PYTHONIOENCODING', 'VQRO_TEST_EXTENSIONS_DIR'})\n"
                          f"  self.assertEqual(os.environ['VQRO_TEST_EXTENSIONS_DIR'], {str(self.extensions.resolve())!r})\n"
                          "  self.assertTrue(pathlib.Path(os.environ['HOME']).is_dir())\n")
        with patch.dict(os.environ, {"VQRO_SOCKET": "canary", "VQRO_PRINCIPAL": "secret", "HC_SOCKET": "canary",
                                     "HERDR_ENV": "1", "HERDR_SOCKET": "canary", "PRIVATE_SECRET": "canary"}):
            self.assertEqual(unit.tests_row("sample", self.extensions)["status"], "pass")

    def test_no_tests_and_disabled_tests_are_not_pass(self):
        item = unit.tests_row("sample", self.extensions)
        self.assertEqual((item["status"], item["evidence"]), ("skip", "no tests installed: no coverage"))
        self.install_test("raise RuntimeError('must not execute')\n")
        self.assertEqual(unit.tests_row("sample", self.extensions, enabled=False)["status"], "skip")
        self.assertEqual(self.rows(self.report(tests=False))["tests/sample"]["evidence"], "tests disabled by caller")

    def write_workspace(self):
        (self.state / "workspace.json").write_text(json.dumps({"rev": 1, "next_txn": 2,
                                                              **self.snapshot, "history": []}))

    def test_offline_file_fallback_is_read_only(self):
        self.write_workspace()
        path = self.state / "workspace.json"
        original = path.read_bytes()
        result = unit.collect(self.state, self.extensions, offline=True)
        self.assertEqual(result["source"], "offline")
        self.assertEqual(result["verdict"], "unverified")
        self.assertEqual(self.rows(result)["core/reachable"]["status"], "warn")
        self.assertEqual(self.rows(result)["manifest/sample"]["status"], "pass")
        self.assertEqual(path.read_bytes(), original)
        self.assertEqual(sorted(p.name for p in self.state.iterdir()), ["workspace.json"])

    def test_offline_bad_manifest_is_warning(self):
        self.write_workspace()
        path = self.extensions / "manifests/sample.json"
        for content in ('{"name":"other"}', "not json", '"not an object"', "x" * 32769):
            path.write_text(content)
            result = unit.collect(self.state, self.extensions, offline=True)
            self.assertEqual(self.rows(result)["manifest/sample"]["status"], "warn")

    def test_missing_workspace_is_harness_error(self):
        result = unit.collect(self.state, self.extensions, offline=True)
        self.assertEqual(result["verdict"], "error")
        self.assertEqual(result["source"], "offline")

    def test_midrun_failure_is_harness_error(self):
        self.client.call.side_effect = ConnectionError("core disappeared")
        result = self.report()
        self.assertEqual(result["verdict"], "error")
        self.assertIn("core disappeared", result["rows"][-1]["evidence"])

    def test_fingerprints_cover_decl_script_manifest_and_directory(self):
        def fp():
            return shared.fingerprint(self.extensions, self.declarations, {})
        original = fp()
        (self.extensions / "sample.py").write_text("# changed\n")
        changed = fp()
        self.assertNotEqual(original["extensions_dir"], changed["extensions_dir"])
        self.assertNotEqual(original["extensions"], changed["extensions"])
        self.assertEqual(original["declarations"], changed["declarations"])
        self.declarations["extension/sample"]["description"] = "changed"
        self.assertNotEqual(fp()["declarations"], changed["declarations"])
        changed = fp()
        (self.extensions / "manifests/sample.json").write_text('{"name":"sample","summary":"changed"}')
        self.assertNotEqual(fp()["extensions"], changed["extensions"])
        changed = fp()
        (self.extensions / "unrelated.txt").write_text("installed asset")
        self.assertNotEqual(fp(), changed)
        self.assertEqual(fp(), shared.fingerprint(self.extensions, dict(reversed(list(self.declarations.items()))), {}))

    def test_receipt_permissions_and_retention(self):
        result = self.report()
        for _ in range(22):
            destination = shared.save_receipt(result, self.state)
        paths = list((self.state / "test-unit").glob("receipt-*.json"))
        self.assertEqual(len(paths), 20)
        self.assertEqual(json.loads(Path(destination).read_text(encoding="utf-8")), result)
        if sys.platform != "win32":  # Owner-only POSIX modes are not a Windows ACL assertion.
            self.assertEqual(stat.S_IMODE(Path(destination).stat().st_mode), 0o600)
            Path(destination).chmod(0o666)
            shared.write_receipt(result, destination)
            self.assertEqual(stat.S_IMODE(Path(destination).stat().st_mode), 0o600)

    def test_receipt_and_panel_bounds(self):
        self.facts["sample"].update(display_status="failed", failure={"reason": "x" * 10000})
        result = self.report()
        self.assertTrue(all(len(r["evidence"]) <= 300 for r in result["rows"]))
        self.assertLess(len(shared.canonical(result)), 1024 * 1024)
        result["rows"] *= 200
        result["not_covered"] *= 200
        self.assertLessEqual(len(unit.panel(result)["items"]), 400)
        large = shared.receipt([shared.row("status", str(i), "pass", "x" * 300) for i in range(4000)],
                               source="live", started_at=shared.utc_now(), started=time.monotonic(),
                               fingerprint_value={}, inputs=[], not_covered=[], extension_count=4000)
        self.assertEqual(large["verdict"], "error")
        self.assertLess(len(shared.canonical(large)), shared.RECEIPT_BYTES)

    def test_cli_endpoint_precedence(self):
        args = shared.cli_parser().parse_args([])
        env = {"HOME": "/home/fixture", "XDG_STATE_HOME": "/xdg"}
        self.assertEqual(shared.endpoints(args, env), ("/xdg/vqro/vqro.sock", Path("/xdg/vqro"),
                                                       Path("/home/fixture/.local/share/vqro/extensions")))
        env.update(VQRO_STATE_DIR="/state", VQRO_SOCKET="/live/socket", VQRO_SOCKET_SOURCE="terminal",
                   VQRO_EXTENSIONS_DIR="/extensions")
        self.assertEqual(shared.endpoints(args, env)[0], "/state/vqro.sock")
        args = shared.cli_parser().parse_args(["--socket", "/explicit/socket", "--state-dir", "/explicit/state",
                                               "--extensions-dir", "/explicit/extensions"])
        self.assertEqual(shared.endpoints(args, env), ("/explicit/socket", Path("/explicit/state"), Path("/explicit/extensions")))
        env.pop("VQRO_SOCKET_SOURCE")
        self.assertEqual(shared.endpoints(shared.cli_parser().parse_args([]), env)[0], "/live/socket")

    def test_cli_offline_json_and_out(self):
        self.write_workspace()
        out = self.root / "receipt.json"
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            code = unit.main(["--offline", "--json", "--state-dir", str(self.state), "--extensions-dir",
                              str(self.extensions), "--out", str(out)])
        self.assertEqual(code, 77)
        self.assertEqual(json.loads(stdout.getvalue()), json.loads(out.read_text(encoding="utf-8")))
        if sys.platform != "win32":  # Windows file modes do not prove an owner-only ACL.
            self.assertEqual(stat.S_IMODE(out.stat().st_mode), 0o600)

    def test_cli_connection_failure_falls_back(self):
        self.write_workspace()
        with contextlib.redirect_stdout(io.StringIO()) as stdout:
            code = unit.main(["--json", "--socket", str(self.root / "missing.sock"), "--state-dir", str(self.state),
                              "--extensions-dir", str(self.extensions)])
        self.assertEqual(code, 77)
        self.assertEqual(json.loads(stdout.getvalue())["source"], "offline")
        self.assertFalse((self.state / "test-unit").exists())

    def test_extension_commands_publish_and_save_only_when_asked(self):
        handlers = {}
        ext = Mock()
        ext.info = {"server": "vqro", "version": "fixture-v1", "state_dir": str(self.state)}
        ext.vqro = self.client

        def command(name, title):
            def register(fn):
                handlers[name] = fn
                return fn
            return register

        ext.command.side_effect = command
        with patch("vqro_ext.Extension", return_value=ext), patch.dict(os.environ, {"VQRO_EXTENSIONS_DIR": str(self.extensions)}):
            unit.extension_main()
            self.assertEqual([call.args[0] for call in self.client.call.call_args_list], ["workspace.snapshot"])
            self.assertEqual([call.args[0] for call in ext.publish.call_args_list], ["testing.status", "settings.page.testing"])
            ext.contribute.assert_called_once()
            self.assertEqual(ext.contribute.call_args.args[0]["view/test-unit.report"]["kind"], "panel")
            empty = handlers["test.unit.show"]({})
            self.assertEqual(empty["client"]["popup"]["view"], "test-unit.report")
            self.assertEqual(list((self.state / "test-unit").glob("receipt-*.json")), [])
            with patch.object(unit, "collect", wraps=unit.collect) as collect:
                first = handlers["test.unit"]({"tests": False})
                self.assertEqual(collect.call_args.kwargs["tests_budget"], 20)
            self.assertEqual(first["verdict"], "pass")
            self.assertTrue(Path(first["receipt"]).is_file())
            saved = json.loads(Path(first["receipt"]).read_text())
            self.assertIn("sample: tests disabled by caller", saved["not_covered"])
            calls = self.client.call.call_count
            handlers["test.unit.show"]({})
            self.assertEqual(self.client.call.call_count, calls + 1)  # current status rescan, not another Unit run.
            self.assertEqual(ext.publish.call_args.args[0], "test-unit.report")
            self.facts["sample"]["manifest_error"] = "broken metadata"
            second = handlers["test.unit"]({"strict": True, "tests": False})
            self.assertEqual(second["verdict"], "fail")

    def test_cli_write_failure_is_error(self):
        self.write_workspace()
        with contextlib.redirect_stdout(io.StringIO()) as stdout:
            code = unit.main(["--offline", "--json", "--state-dir", str(self.state), "--extensions-dir",
                              str(self.extensions), "--out", str(self.root / "missing" / "receipt.json")])
        self.assertEqual(code, 2)
        self.assertEqual(json.loads(stdout.getvalue())["verdict"], "error")


class PlainClientTests(unittest.TestCase):
    def test_protocol_error_and_line_bound(self):
        with patch.object(shared, "connect_endpoint") as connector:
            transport = connector.return_value
            stream = transport.makefile.return_value
            client = shared.PlainClient("fixture", timeout=1)
            connector.assert_called_once_with("fixture", timeout=1)
            stream.readline.return_value = b'{"id":1,"error":{"message":"broken"}}\n'
            with self.assertRaisesRegex(RuntimeError, "broken"):
                client.call("hello")
            client.MAX_RECORD_BYTES = 16
            stream.readline.return_value = b"x" * 17
            with self.assertRaisesRegex(ConnectionError, "size bound"):
                client.call("extension.list")
            stream.readline.side_effect = TimeoutError("deadline")
            with self.assertRaises(TimeoutError):
                client.call("workspace.snapshot")
            client.close()
            stream.close.assert_called_once()
            transport.close.assert_called_once()

    def test_makefile_error_closes_connected_transport(self):
        with patch.object(shared, "connect_endpoint") as connector:
            connector.return_value.makefile.side_effect = OSError("no stream")
            with self.assertRaisesRegex(OSError, "no stream"):
                shared.PlainClient("fixture")
            connector.return_value.close.assert_called_once()

    def test_connect_error_propagates_without_a_fallback_transport(self):
        with patch.object(shared, "connect_endpoint", side_effect=OSError("no core")) as connector:
            with self.assertRaisesRegex(OSError, "no core"):
                shared.PlainClient("fixture")
            connector.assert_called_once_with("fixture", timeout=15)

    def test_readonly_client_handles_notifications_and_eof(self):
        # A local fixture socket, never an actual core.
        with tempfile.TemporaryDirectory(prefix="vqtu-") as directory:
            path = str(Path(directory) / "s")
            left, right = socket.socketpair()
            self.addCleanup(left.close)
            self.addCleanup(right.close)
            requests = []

            def serve():
                with right, right.makefile("rb") as stream:
                    connection = right
                    request = json.loads(stream.readline())
                    requests.append(request)
                    connection.sendall(b'{"jsonrpc":"2.0","method":"notice"}\n')
                    connection.sendall(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": {"version": "fixture"}}).encode() + b"\n")
                    requests.append(json.loads(stream.readline()))

            worker = threading.Thread(target=serve)
            worker.start()
            with patch.object(shared, "connect_endpoint", return_value=left):
                client = shared.PlainClient(path, timeout=1)
            try:
                self.assertEqual(client.call("hello", {"client": "test-unit"}), {"version": "fixture"})
                with self.assertRaises(ValueError):
                    client.call("workspace.apply")
                with self.assertRaises(ConnectionError):
                    client.call("extension.list")
            finally:
                client.close()
                worker.join(timeout=2)
            self.assertFalse(worker.is_alive())
            self.assertEqual(requests[0]["params"], {"client": "test-unit"})
            self.assertNotIn("extension_token", requests[0]["params"])


if __name__ == "__main__":
    unittest.main()
