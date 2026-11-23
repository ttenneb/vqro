"""Offline mocks and private file fixtures for the integration harness."""
import json
import os
from pathlib import Path
import socket
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
import vqro_testing as harness
import testing_integration as integration


class SandboxTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="vqi-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = self.root / "source"
        self.source.mkdir()
        (self.source / "example.py").write_text("# fixture\n")
        self.snapshot = {"entries": {"extension/example": {"version": 1, "value": {
            "argv": ["python3", str(self.source / "example.py")], "enabled": True}}}}

    def sandbox(self, **kwargs):
        sandbox = harness.Sandbox(self.source, self.snapshot, root=self.root / "run", **kwargs)
        self.addCleanup(sandbox.cleanup)
        return sandbox

    @unittest.skipIf(sys.platform == "win32", "constructs the explicitly unsupported native Windows sandbox")
    def test_copy_rebases_launch_paths_and_scrubs_environment(self):
        with patch.dict(os.environ, {"VQRO_SOCKET": "/live/socket", "HC_SOCKET": "/live/hc", "HERDR_PANE_ID": "real", "TMUX": "real"}):
            sandbox = self.sandbox().prepare()
        value = sandbox.declarations["extension/example"]
        self.assertEqual(value["argv"][1], str(sandbox.ext / "example.py"))
        self.assertEqual(value["cwd"], str(sandbox.ext))
        self.assertEqual(sandbox.info["canary"]["status"], "pass")
        self.assertFalse(any(key.startswith(("HERDR_", "TMUX")) for key in sandbox.env))
        self.assertNotEqual(sandbox.env["HC_SOCKET"], "/live/hc")
        self.assertFalse(sandbox.info["started"])
        self.assertEqual((self.source / "example.py").read_text(), "# fixture\n")

    @unittest.skipIf(sys.platform == "win32", "constructs the explicitly unsupported native Windows sandbox")
    def test_credential_entries_contributions_and_resources_are_excluded(self):
        for key, value in {"doc/settings/x": {"token": "sensitive"}, "doc/auth": {},
                           "doc/owned": {"owner": "example"}, "view/x": {"kind": "panel"},
                           "layout/x": {}, "doc/good": {"value": 4}}.items():
            self.snapshot["entries"][key] = {"version": 1, "value": value}
        sandbox = self.sandbox().prepare()
        self.assertEqual(set(sandbox.values), {"extension/example", "doc/good"})
        self.assertIn("doc/settings/x", sandbox.info["excluded"])
        self.assertIn("doc/owned", sandbox.info["excluded"])

    @unittest.skipIf(sys.platform == "win32", "constructs the explicitly unsupported native Windows sandbox")
    def test_whole_tree_preserves_scenario_names_and_links_without_dereferencing(self):
        external = self.root / "outside"
        external.write_text("private")
        (self.source / "link").symlink_to(external)
        (self.source / "token.json").write_text("private")
        sandbox = self.sandbox().prepare()
        self.assertTrue((sandbox.ext / "link").is_symlink())
        self.assertEqual((sandbox.ext / "token.json").read_text(), "private")
        overlay = self.root / "overlay"
        overlay.mkdir()
        (overlay / "link").write_text("candidate")
        sandbox.copy_tree(overlay, sandbox.ext)
        self.assertFalse((sandbox.ext / "link").is_symlink())
        self.assertEqual(external.read_text(), "private")

    @unittest.skipIf(sys.platform == "win32", "constructs the explicitly unsupported native Windows sandbox")
    def test_fake_backend_deps_are_unverified_only_when_audit_caused_the_outage(self):
        for name, enabled in (("herdr", True), ("tmux", False)):
            self.snapshot["entries"]["extension/" + name] = {"value": {"argv": ["vqro-" + name], "kind": "provider", "enabled": enabled}}
        sandbox = self.sandbox().prepare()
        providers = [{"name": name, "display_status": "off"} for name in ("herdr", "tmux")]
        need = {"cap": "ability/vqro.terminals", "status": "off", "providers": providers}
        facts = {"dependencies": {"status": "missing", "needs": [need]}}
        item = harness.dependency_row("example", facts, fake_providers=sandbox.info["fake_providers"])
        self.assertEqual(sandbox.info["fake_providers"], {"herdr": True, "tmux": False})
        self.assertEqual(item["status"], "unverified")
        self.assertIn(harness.FAKE_BACKEND_COVERAGE, item["evidence"])
        self.assertEqual(item["backend_unavailable"], [need["cap"]])
        self.assertEqual(harness.dependency_row("example", facts)["status"], "fail")
        self.assertEqual(harness.dependency_row("example", facts, fake_providers={"herdr": False, "tmux": False})["status"], "fail")
        for missing in ({"cap": "ability/actually-missing", "status": "missing", "providers": []},
                        {"cap": "ability/other", "status": "off", "providers": [{"name": "other", "display_status": "off"}]}):
            with self.subTest(missing=missing):
                mixed = {"dependencies": {"status": "missing", "needs": [need, missing]}}
                failed = harness.dependency_row("example", mixed, fake_providers=sandbox.info["fake_providers"])
                self.assertEqual(failed["status"], "fail")
                self.assertIn(missing["cap"], failed["evidence"])
        for baseline in ("pass", "unverified"):
            compared = harness.compare_runs({"rows": [harness.row("deps", "example", baseline, "baseline")]}, {"rows": [item]}, {})
            self.assertEqual(compared[0]["status"], "unverified")
            self.assertIn(harness.FAKE_BACKEND_COVERAGE, compared[0]["evidence"])

    @unittest.skipIf(sys.platform == "win32", "constructs the explicitly unsupported native Windows sandbox")
    def test_fake_disables_providers(self):
        self.snapshot["entries"]["extension/example"]["value"].update(kind="provider")
        sandbox = self.sandbox().prepare()
        self.assertFalse(sandbox.declarations["extension/example"]["enabled"])
        self.assertEqual(json.loads(sandbox.env["VQRO_HERDR"]), [sys.executable, "-c", "raise SystemExit(1)"])
        self.assertEqual(sandbox.enabled(), [])

    @unittest.skipIf(sys.platform == "win32", "constructs the explicitly unsupported native Windows sandbox")
    def test_external_path_kept_off_unless_explicitly_allowed(self):
        self.snapshot["entries"]["extension/example"]["value"]["argv"][1] = str(self.root / "external.py")
        sandbox = self.sandbox().prepare()
        self.assertEqual(sandbox.audit_rows[0]["status"], "unverified")
        self.assertFalse(sandbox.declarations["extension/example"]["enabled"])
        sandbox.cleanup()
        allowed = self.sandbox(allow_external=True).prepare()
        self.assertTrue(allowed.declarations["extension/example"]["enabled"])

    @unittest.skipIf(sys.platform == "win32", "constructs the explicitly unsupported native Windows sandbox")
    def test_socket_bound_fails_before_core_start(self):
        sandbox = harness.Sandbox(self.source, self.snapshot, root=self.root / ("deep" * 30))
        self.addCleanup(sandbox.cleanup)
        with patch.object(harness.subprocess, "Popen") as launch:
            with self.assertRaisesRegex(ValueError, "socket path"):
                sandbox.prepare()
            launch.assert_not_called()
        self.assertEqual(sandbox.info["canary"]["status"], "fail")
        self.assertFalse(sandbox.info["started"])

    @unittest.skipIf(sys.platform == "win32", "constructs the explicitly unsupported native Windows sandbox")
    def test_existing_nonempty_root_is_never_removed(self):
        root = self.root / "run"
        root.mkdir()
        marker = root / "keep"
        marker.write_text("user file")
        sandbox = self.sandbox()
        with self.assertRaisesRegex(ValueError, "absent or empty"):
            sandbox.prepare()
        sandbox.cleanup()
        self.assertTrue(marker.exists())

    @unittest.skipIf(sys.platform == "win32", "constructs the explicitly unsupported native Windows sandbox")
    def test_candidate_overlay_and_ops_are_audited(self):
        candidate = self.root / "candidate"
        candidate.mkdir()
        (candidate / "example.py").write_text("# candidate\n")
        sandbox = self.sandbox(candidate_ext=candidate, candidate_ops=[
            {"op": "set", "key": "doc/test", "value": {"candidate": True}}]).prepare()
        self.assertEqual((sandbox.ext / "example.py").read_text(), "# candidate\n")
        self.assertEqual(sandbox.values["doc/test"], {"candidate": True})
        self.assertEqual((self.source / "example.py").read_text(), "# fixture\n")

    @unittest.skipIf(sys.platform == "win32", "constructs the explicitly unsupported native Windows sandbox")
    def test_candidate_cannot_seed_credentials_or_resources(self):
        sandbox = self.sandbox(candidate_ops=[{"op": "set", "key": "doc/x", "value": {"password": "private"}}])
        with self.assertRaisesRegex(ValueError, "credential-free"):
            sandbox.prepare()

    @unittest.skipIf(sys.platform == "win32", "tests Unix endpoint containment, after the explicit native Windows refusal")
    def test_sandbox_client_rejects_external_socket_before_connect(self):
        with patch.object(harness.socket, "socket") as factory:
            with self.assertRaisesRegex(ValueError, "outside root"):
                harness.SandboxClient(self.root / "external", self.root / "run")
            factory.assert_not_called()

    def test_apply_fills_compare_and_set(self):
        client = object.__new__(harness.SandboxClient)
        client.call = Mock(side_effect=[{"entries": {"doc/x": {"version": 8}}}, {"txn": 9}])
        result = client.apply([{"op": "set", "key": "doc/x", "value": 4}, {"op": "set", "key": "doc/new", "value": 2}])
        self.assertEqual(result, {"txn": 9})
        self.assertEqual([op["expect"] for op in client.call.call_args.args[1]["ops"]], [8, 0])

    @unittest.skipIf(sys.platform == "win32", "constructs the explicitly unsupported native Windows sandbox")
    def test_lifetime_survivors_reported_then_only_owned_group_signalled(self):
        sandbox = self.sandbox().prepare()
        sandbox.groups = [54321]
        survivor = [{"pid": 54322, "command": "uncooperative fixture"}]
        with patch.object(harness, "process_group_members", return_value=survivor), patch.object(harness.time, "monotonic", side_effect=[0, 20]), patch.object(harness.time, "sleep"), patch.object(harness.os, "killpg") as kill:
            sandbox.stop()
        self.assertEqual(sandbox.leftovers, survivor)
        self.assertTrue(sandbox.leftovers)
        self.assertEqual([call.args[0] for call in kill.call_args_list], [54321, 54321])

    @unittest.skipIf(sys.platform == "win32", "constructs the explicitly unsupported native Windows sandbox")
    def test_refused_optional_document_is_reported_after_atomic_seed_fallback(self):
        self.snapshot["entries"]["doc/protected"] = {"value": {"example": True}}
        sandbox = self.sandbox().prepare()
        client = Mock()
        client.call.side_effect = lambda method, *args: {"hello": {"server": "vqro", "version": "fixture"}, "workspace.snapshot": {"entries": {}}}[method]
        client.apply.side_effect = [RuntimeError("forbidden"), {"txn": 1}, RuntimeError("protected document refused")]
        with patch.object(harness.subprocess, "Popen", return_value=Mock(pid=123456, poll=Mock(return_value=None))), patch.object(harness, "SandboxClient", return_value=client):
            sandbox.start()
        self.assertEqual(client.call.call_args_list[0].args[1]["identity"], {"kind": "owner"})
        self.assertEqual(len(client.apply.call_args_list[0].args[0]), 2)
        self.assertEqual([op["key"] for op in client.apply.call_args_list[1].args[0]], ["extension/example"])
        self.assertEqual(sandbox.info["not_copied"][0]["key"], "doc/protected")
        sandbox.client = sandbox.process = None
        sandbox.groups.clear()

    @unittest.skipIf(sys.platform == "win32", "constructs the explicitly unsupported native Windows sandbox")
    def test_leftovers_are_attributed_only_when_the_script_is_identified(self):
        self.snapshot["entries"]["extension/other"] = {"value": {"argv": ["python3", "other.py"]}}
        sandbox = self.sandbox().prepare()
        sandbox.info["started"] = True
        leftovers = [{"pid": 111, "start_ticks": 1, "command": f"python3 {sandbox.ext / 'example.py'}"},
                     {"pid": 112, "start_ticks": 1, "command": "unknown child"}]
        with patch.object(harness, "private_processes", return_value=leftovers), patch.object(harness, "process_identity", return_value=None):
            sandbox.stop()
        statuses = {r["id"]: r["status"] for r in sandbox.lifetime_rows}
        self.assertEqual(statuses["lifetime/example"], "fail")
        self.assertEqual(statuses["lifetime/other"], "pass")
        self.assertEqual(statuses["lifetime/unattributed"], "fail")
        sandbox.info["started"] = False

    def test_process_facts_exclude_other_groups_and_zombies(self):
        output = "12 12 S core\n13 12 S extension\n14 12 Z zombie\n21 21 S unrelated\n"
        with patch.object(harness.os, "name", "posix"), \
                patch.object(harness.subprocess, "run", return_value=Mock(stdout=output)):
            self.assertEqual([p["pid"] for p in harness.process_group_members(12, {})], [12, 13])

    @unittest.skipIf(sys.platform == "win32", "constructs the explicitly unsupported native Windows sandbox")
    def test_smoke_checks_registration_views_recovery_without_extension_commands(self):
        sandbox = self.sandbox().prepare()
        fact = {"display_status": "running", "crashes": 0, "generation": 1,
                "dependencies": {"status": "ok"}, "manifest": {"provides": ["command/example.show", "view/example.panel"]}}
        sandbox.wait_running = Mock(return_value={"example": fact})
        sandbox.stop = Mock()
        sandbox.start = Mock()
        client = Mock()
        sandbox.client = client
        client.call.side_effect = lambda method: {
            "extension.list": {"extensions": {"example": fact}},
            "workspace.snapshot": {"entries": {"extension/example": {"value": sandbox.declarations["extension/example"]},
                                               "view/example.panel": {"value": {}}}, "live": {}},
            "command.list": {"commands": [{"name": "example.show", "available": True}]}}[method]
        client.apply.return_value = {"txn": 42}
        rows = harness.smoke(sandbox, window=0)
        indexed = {r["id"]: r for r in rows}
        self.assertEqual(indexed["start/example"]["status"], "pass")
        self.assertEqual(indexed["commands/example"]["status"], "pass")
        self.assertEqual(indexed["views/example"]["status"], "pass")
        self.assertEqual(indexed["recovery/restart"]["status"], "pass")
        self.assertEqual([c.args[0] for c in client.invoke.call_args_list], ["vqro.safe_mode", "vqro.safe_mode", "vqro.revert"])
        fact["manifest"]["provides"].append("command/not.registered")
        indexed = {r["id"]: r for r in harness.smoke(sandbox, window=0)}
        self.assertEqual(indexed["commands/example"]["status"], "fail")
        fact["manifest"]["provides"].append("view/agent-*")
        indexed = {r["id"]: r for r in harness.smoke(sandbox, window=0)}
        self.assertEqual(indexed["views/example"]["status"], "unverified")
        self.assertIn("view family needs live resources", indexed["views/example"]["evidence"])
        fact["manifest"]["provides"].append("view/missing.concrete")
        indexed = {r["id"]: r for r in harness.smoke(sandbox, window=0)}
        self.assertEqual(indexed["views/example"]["status"], "fail")
        sandbox.stop = Mock()

    @unittest.skipIf(sys.platform == "win32", "constructs the explicitly unsupported native Windows sandbox")
    def test_readiness_waits_for_delayed_manifest_command_registration(self):
        sandbox = self.sandbox().prepare()
        client = object.__new__(harness.SandboxClient)
        sandbox.client = client
        facts = {"example": {"display_status": "running", "manifest": {"provides": ["command/example.ready"]}}}
        client.call = Mock(side_effect=[{"extensions": facts}, {"commands": []},
                                        {"extensions": facts}, {"commands": [{"name": "example.ready", "available": True}]}])
        client.next_event = Mock(return_value={"method": "event", "params": {"type": "command"}})
        self.assertEqual(sandbox.wait_running(), facts)
        client.next_event.assert_called_once()
        self.assertEqual([call.args[0] for call in client.call.call_args_list],
                         ["extension.list", "command.list", "extension.list", "command.list"])
        sandbox.client = None

    @unittest.skipIf(sys.platform == "win32", "constructs the explicitly unsupported native Windows sandbox")
    def test_smoke_zero_enabled_is_failure(self):
        sandbox = self.sandbox().prepare()
        sandbox.declarations["extension/example"]["enabled"] = False
        self.assertEqual(harness.smoke(sandbox)[0]["status"], "fail")


class IntegrationModesTests(unittest.TestCase):
    setUp = SandboxTests.setUp

    def write_source(self):
        state = self.root / "source-state"
        state.mkdir()
        (state / "workspace.json").write_text(json.dumps(self.snapshot))
        return state

    @unittest.skipIf(sys.platform == "win32", "CLI scenario constructs the explicitly unsupported native Windows sandbox")
    def test_cli_offline_candidate_new_scenario_uses_top_level_comparisons(self):
        state = self.write_source()
        scenario_dir = self.source / "tests/example/scenarios"
        scenario_dir.mkdir(parents=True)
        (scenario_dir / "new.json").write_text(json.dumps({"new": True, "steps": [{"expect": {"entry": "doc/x", "exists": True}}]}))
        candidate = self.root / "candidate"
        candidate.mkdir()
        args = integration.parser().parse_args(["--offline", "--state-dir", str(state), "--extensions-dir", str(self.source),
                                                "--root", str(self.root / "run"), "--candidate-ext", str(candidate)])
        baseline = {"rows": [harness.row("start", "example", "pass", "running"),
                             harness.row("scenario", "example/new", "fail", "expected baseline failure")], "error": False}
        changed = {"rows": [harness.row("start", "example", "pass", "running"),
                            harness.row("scenario", "example/new", "pass", "candidate success")], "error": False}
        with patch.object(integration, "run_copy", side_effect=[baseline, changed]):
            result = integration.run(args)
        self.assertEqual(result["verdict"], "pass")
        self.assertEqual(result["baseline"]["rows"][1]["status"], "fail")
        self.assertEqual(result["rows"][1]["id"], "compare/scenario/example/new")
        self.assertFalse((self.root / "run").exists())

    def test_fake_backend_dependency_gap_is_listed_as_not_covered(self):
        sandbox = Mock(audit_rows=[], lifetime_rows=[], leftovers=[], info={}, declarations={}, ext=self.source)
        backend = harness.row("deps", "example", "unverified", harness.FAKE_BACKEND_COVERAGE)
        backend["backend_unavailable"] = ["ability/vqro.terminals"]
        with patch.object(integration, "smoke", return_value=[backend, harness.row("recovery", "restart", "pass", "ready")]), patch.object(integration, "run_scenarios", return_value=[]):
            result = integration.run_copy(sandbox, {})
        self.assertTrue(any(harness.FAKE_BACKEND_COVERAGE in gap and "ability/vqro.terminals" in gap for gap in result["not_covered"]))

    @unittest.skipIf(sys.platform == "win32", "CLI scenario constructs the explicitly unsupported native Windows sandbox")
    def test_cli_canary_error_has_no_spawn_and_existing_root_is_untouched(self):
        state = self.write_source()
        root = self.root / ("deep" * 30)
        args = integration.parser().parse_args(["--offline", "--json", "--state-dir", str(state), "--extensions-dir", str(self.source), "--root", str(root)])
        with patch.object(harness.subprocess, "Popen") as spawn:
            result = integration.run(args)
            spawn.assert_not_called()
        self.assertEqual(result["verdict"], "error")
        self.assertFalse(result["sandbox"]["started"])
        self.assertEqual(result["sandbox"]["canary"]["status"], "fail")
        self.assertFalse(root.exists())
        root.mkdir()
        marker = root / "user-file"
        marker.write_text("untouched")
        result = integration.run(args)
        self.assertEqual(result["verdict"], "error")
        self.assertEqual(marker.read_text(), "untouched")

    def test_scenarios_are_not_invoked_after_failed_restart_readiness(self):
        sandbox = Mock(audit_rows=[], lifetime_rows=[], leftovers=[], info={}, declarations={}, ext=self.source)
        with patch.object(integration, "smoke", return_value=[harness.row("recovery", "restart", "fail", "commands not ready")]), patch.object(integration, "run_scenarios") as scenarios:
            result = integration.run_copy(sandbox, {"example/check": {"steps": [{"invoke": "example.check"}]}})
            scenarios.assert_not_called()
        failed = next(r for r in result["rows"] if r["id"] == "scenario/example/check")
        self.assertEqual(failed["status"], "fail")
        self.assertIn("readiness failed", failed["evidence"])

    def test_async_extension_returns_before_waiting_then_publishes_completion(self):
        handlers = {}
        ext = Mock()
        state = self.root / "extension-state"
        state.mkdir()
        ext.info = {"state_dir": str(state), "socket": str(self.root / "source.sock")}
        ext.command.side_effect = lambda name, title: lambda fn: handlers.setdefault(name, fn)
        child = Mock()
        child.poll.return_value = None
        with patch("vqro_ext.Extension", return_value=ext), patch.object(integration.subprocess, "Popen", return_value=child) as launch, patch.object(integration.threading, "Thread") as thread, patch.dict(os.environ, {"VQRO_EXTENSIONS_DIR": str(self.source), "HERDR_PANE_ID": "canary"}):
            integration.extension_main()
            first = handlers["test.integration"]({"backend": "fake"})
            self.assertTrue(first["started"])
            child.wait.assert_not_called()
            ext.publish.assert_not_called()
            self.assertEqual(set(launch.call_args.kwargs["env"]), {"HOME", "PATH"})
            self.assertNotIn("VQRO_EXTENSION_LIFETIME", launch.call_args.kwargs["env"])
            self.assertFalse(handlers["test.integration"]({})["started"])
            result = {"verdict": "pass", "counts": {}, "rows": []}
            Path(first["receipt"]).write_text(json.dumps(result))
            thread.call_args.kwargs["target"]()
            child.wait.assert_called_once()
            self.assertEqual(ext.publish.call_args.args[0], "test-integration.report")
            popup = handlers["test.integration.show"]({})
            self.assertEqual(popup["client"]["popup"]["view"], "test-integration.report")


class SourceMarkTests(unittest.TestCase):
    def test_source_seq_and_unrelated_documents_are_informational(self):
        before = {"seq": 10, "epoch": "fixture", "entries": {"extension/example": {"version": 1, "value": {"argv": ["python3", "example.py"]}}}}
        after = json.loads(json.dumps(before))
        after["seq"] = 20
        after["entries"]["doc/unrelated"] = {"version": 1, "value": "busy client"}
        facts = {"extensions": {"example": {"generation": 1}}}
        client = Mock()
        client.call.side_effect = [before, facts, after, facts]
        _, start = harness.source_mark(client)
        _, end = harness.source_mark(client)
        item = harness.source_untouched(start, end)
        self.assertEqual(item["status"], "pass")
        self.assertEqual(item["observed"]["after"]["seq"], 20)
        self.assertIn("informational", item["evidence"])
        self.assertEqual(start["declarations"], end["declarations"])

    def test_setup_changes_fail_both_suite_verdicts_but_missing_proof_does_not(self):
        import testing_perf as perf
        before = {"seq": 10, "epoch": "fixture", "declarations": "original", "generations": {"example": 1}}
        for field, value in (("declarations", "changed"), ("generations", {"example": 2})):
            with self.subTest(field=field):
                after = {**before, field: value, "seq": 20}
                failed = harness.source_untouched(before, after)
                self.assertEqual(failed["status"], "fail")
                self.assertEqual(perf.verdict([failed]), "fail")
                self.assertEqual(integration.summarize([failed], [harness.row("start", "example", "pass", "ready")]), "fail")
        informational = harness.row("live", "untouched", "unverified", "source disconnected; proof unavailable")
        valid = harness.row("startup", "core", "pass", "finite samples")
        self.assertEqual(perf.verdict([valid, informational]), "pass")
        self.assertEqual(integration.summarize([informational], [harness.row("start", "example", "pass", "ready")]), "pass")
        self.assertIs(perf.source_mark, integration.source_mark)

    def test_declaration_values_are_in_the_source_digest(self):
        snapshots = [{"seq": 1, "epoch": "fixture", "entries": {"extension/example": {"value": {"argv": ["python3", script]}}}} for script in ("before.py", "after.py")]
        client = Mock(side_effect=None)
        client.call.side_effect = [snapshots[0], {"extensions": {}}, snapshots[1], {"extensions": {}}]
        _, before = harness.source_mark(client)
        _, after = harness.source_mark(client)
        self.assertNotEqual(before["declarations"], after["declarations"])
        self.assertEqual(harness.source_untouched(before, after)["status"], "fail")


class ScenarioTests(unittest.TestCase):
    def test_apply_invoke_subset_expect_and_wait(self):
        client = Mock()
        client.invoke.return_value = {"result": {"ok": True, "extra": 1}}
        client.call.return_value = {"entries": {"doc/x": {"value": {"show": "all"}}},
                                    "live": {"fixture.panel": {"items": [{"text": "probe"}]}}}
        client.wait.side_effect = lambda observe, predicate, timeout: observe() if predicate(observe()) else None
        scenario = {"fixture/new": {"steps": [
            {"apply": [{"op": "set", "key": "doc/x", "value": {"show": "all"}}]},
            {"invoke": "fixture.show", "result": {"result": {"ok": True}}},
            {"expect": {"entry": "doc/x", "path": "show", "equals": "all"}},
            {"wait": {"content": "fixture.panel", "contains": "probe"}, "within_ms": 100}]}}
        rows = harness.run_scenarios(client, scenario)
        self.assertEqual(rows[0]["id"], "scenario/fixture/new")
        self.assertEqual(rows[0]["status"], "pass")
        self.assertEqual(client.wait.call_args.args[2], 0.1)
        client.apply.assert_called_once()
        self.assertEqual(client.step_deadline, float("inf"))

    def test_failed_expect_includes_last_observation(self):
        client = Mock()
        client.call.return_value = {"entries": {"doc/x": {"value": "actual"}}}
        rows = harness.run_scenarios(client, {"fixture/existing": {"steps": [
            {"expect": {"entry": "doc/x", "equals": "expected"}}]}})
        self.assertEqual(rows[0]["status"], "fail")
        self.assertIn("actual", rows[0]["evidence"])

    def test_wait_timeout_reports_last_value_without_sleeping(self):
        client = object.__new__(harness.SandboxClient)
        client.call = Mock(return_value={"entries": {"doc/x": {"value": "not yet"}}})
        client.next_event = Mock(side_effect=TimeoutError("deadline"))
        with patch.object(harness.time, "sleep", side_effect=AssertionError("scenario waits must use events")):
            rows = harness.run_scenarios(client, {"fixture/wait": {"steps": [
                {"wait": {"entry": "doc/x", "equals": "never"}, "within_ms": 10}]}})
        self.assertEqual(rows[0]["status"], "fail")
        self.assertIn("last observed", rows[0]["evidence"])
        self.assertIn("not yet", rows[0]["evidence"])

    def test_observation_rpc_timeout_preserves_previous_value(self):
        client = object.__new__(harness.SandboxClient)
        client.call = Mock(side_effect=[{"entries": {"doc/ctl-wait": {"value": "not-ready"}}},
                                        TimeoutError("sandbox response/event deadline")])
        client.next_event = Mock(return_value={"method": "event"})
        rows = harness.run_scenarios(client, {"fixture/deadline": {"steps": [
            {"wait": {"entry": "doc/ctl-wait", "equals": "ready"}, "within_ms": 300}]}})
        self.assertEqual(rows[0]["status"], "fail")
        self.assertIn("last observed", rows[0]["evidence"])
        self.assertIn("not-ready", rows[0]["evidence"])

    def test_empty_unknown_and_runtime_steps_fail_closed(self):
        for steps in ([], [{"unknown": True}], [{"invoke": "pi.new"}], [{"invoke": "fixture.show", "within_ms": 0}]):
            client = Mock()
            rows = harness.run_scenarios(client, {"fixture/invalid": {"steps": steps}})
            self.assertEqual(rows[0]["status"], "fail")
            client.invoke.assert_not_called()

    def test_predicate_missing_and_list_path(self):
        client = Mock()
        client.call.return_value = {"entries": {"doc/x": {"value": {"items": ["first"]}}}}
        observed = harness.scenario_observation(client, {"entry": "doc/x", "path": "items.0"})
        self.assertTrue(harness.scenario_matches(observed, {"equals": "first"}))
        missing = harness.scenario_observation(client, {"entry": "doc/missing"})
        self.assertTrue(harness.scenario_matches(missing, {"exists": False}))
        self.assertFalse(harness.scenario_matches(missing, {"equals": None}))

    def test_comparison_fixed_shapes(self):
        for new, before, after, expected in [(True, "fail", "pass", "pass"), (True, "pass", "pass", "weak"),
                                             (True, "pass", "fail", "fail"), (False, "pass", "pass", "pass"),
                                             (False, "pass", "fail", "fail"), (False, "fail", "fail", "warn")]:
            with self.subTest(new=new, before=before, after=after):
                rows = harness.compare_runs({"rows": [harness.row("scenario", "fixture/x", before, "baseline")]},
                                            {"rows": [harness.row("scenario", "fixture/x", after, "candidate")]},
                                            {"fixture/x": {"new": new}})
                self.assertEqual((rows[0]["id"], rows[0]["status"]), ("compare/scenario/fixture/x", expected))
        before = {"rows": [harness.row("start", "fixture", "pass", "baseline")]}
        after = {"rows": [harness.row("start", "fixture", "fail", "candidate")]}
        self.assertEqual(harness.compare_runs(before, after, {})[0]["status"], "fail")
        before["rows"][0]["status"] = "fail"
        self.assertEqual(harness.compare_runs(before, after, {})[0]["status"], "warn")

    def test_candidate_verdict_counts_comparisons_not_expected_baseline_failures(self):
        counting = [harness.row("start", "fixture", "pass", "running")]
        self.assertEqual(integration.summarize([harness.row("compare/scenario", "fixture/x", "pass", "red to green")], counting), "pass")
        self.assertEqual(integration.summarize([harness.row("compare/scenario", "fixture/x", "weak", "both pass")], counting), "unverified")
        self.assertEqual(integration.summarize([harness.row("compare/smoke", "start/fixture", "fail", "regression")], counting), "fail")
        self.assertEqual(integration.summarize([], []), "unverified")
        self.assertEqual(integration.summarize([], counting, error=True), "error")

    def test_panel_bound(self):
        result = {"verdict": "pass", "counts": {}, "rows": [harness.row("start", str(i), "pass", "running") for i in range(1000)]}
        self.assertLessEqual(len(integration.panel(result)["items"]), 400)


if __name__ == "__main__":
    unittest.main()
