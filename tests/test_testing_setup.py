"""Model-free acceptance controls for setup identity and completion evidence."""
import contextlib
import copy
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import time
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
import testing_setup as setup
import testing_unit as unit
import vqro_testing as shared


class SetupTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="tfg-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.ext, self.state = self.root / "ext", self.root / "state"
        self.ext.mkdir()
        self.state.mkdir()
        self.image = self.root / "python-fixture"
        self.image.write_text("#!/bin/sh\nexit 0\n")
        self.image.chmod(0o700)
        (self.ext / "sample.py").write_text("# installed extension\n")
        (self.ext / "manifests").mkdir()
        (self.ext / "manifests/sample.json").write_text('{"name":"sample"}')
        self.snapshot = {"epoch": "private-instance", "seq": 1, "extensions": {"test-unit": {"enabled": True, "display_status": "running"}}, "entries": {
            "extension/sample": {"version": 1, "value": {"argv": [str(self.image), "sample.py"], "enabled": True}},
            setup.POLICY_KEY: {"version": 1, "value": True},
            "doc/principals/owner": {"version": 1, "value": "human"}}}
        self.hello = {"server": "vqro", "version": "fixture", "epoch": "private-instance",
                      "socket": str(self.root / "private.sock"), "state_dir": str(self.state)}
        self.facts = {"sample": {"enabled": True, "display_status": "running", "crashes": 0,
                                 "manifest": {"name": "sample"}, "dependencies": {"status": "ok"}}}
        self.client = Mock()
        self.client.call.side_effect = self.call
        self.store = setup.SetupStore(self.state)

    def call(self, method, *args):
        return {"hello": self.hello, "workspace.snapshot": self.snapshot, "extension.list": {"extensions": self.facts}}[method]

    def value(self):
        return setup.scan(self.client, self.hello, self.state, self.ext)[1]

    def status(self):
        return self.store.status(self.value(), self.snapshot)

    def context(self):
        return self.store.context(self.store.observe(self.value(), self.snapshot))

    def coverage(self):
        directory = self.ext / "tests/sample"
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "test_sample.py").write_text("import unittest\nclass Tests(unittest.TestCase):\n def test_ok(self): self.assertTrue(True)\n")

    def record(self, verdict="pass", suite="unit", coverage=True):
        context = self.context()
        directory = self.state / ("test-unit" if suite == "unit" else "test-integration" if suite == "integration" else "test-perf")
        directory.mkdir(exist_ok=True)
        path = directory / ("receipt-" + str(time.time_ns()) + ".json")
        rows = [shared.row("status", "sample", "pass", "running"),
                shared.row("tests", "sample", "pass" if coverage else "skip", "fixture" if coverage else "no tests installed: no coverage")]
        if verdict == "fail":
            rows.append(shared.row("check", "sample", "fail", "intentional failed check"))
        receipt = {"kind": "vqro.test-receipt", "version": 1, "suite": suite, "verdict": verdict,
                   "started_at": shared.utc_now(), "rows": rows, "counts": {s: sum(r["status"] == s for r in rows) for s in shared.STATUSES},
                   "recorded": suite == "unit", "tests_enabled": True}
        setup.stamp_receipt(receipt, context, context)
        shared.write_receipt(receipt, path)
        return path, receipt

    @unittest.skipUnless(os.name == "posix", "requires the supported recorder lock; missing-lock health has separate portable controls")
    def test_real_handler_transient_recorder_fault_cannot_recover_old_pass_after_new_fail(self):
        self.coverage()
        for phase in ("observe", "begin"):
            with self.subTest(phase=phase):
                self.facts["sample"]["display_status"] = "running"
                handlers = {}
                ext = Mock()
                ext.info, ext.vqro = self.hello, self.client
                ext.entries = self.snapshot["entries"]
                ext.value.side_effect = lambda key, default=None: self.snapshot["entries"].get(key, {}).get("value", default)
                ext.command.side_effect = lambda name, title: lambda fn: handlers.setdefault(name, fn)
                with patch("vqro_ext.Extension", return_value=ext), patch.object(unit.threading, "Thread"), patch.dict(os.environ, {"VQRO_EXTENSIONS_DIR": str(self.ext)}):
                    unit.extension_main()
                    self.record()
                    self.store.skip(self.value(), self.snapshot, "integration", "current fixture explanation")
                    before = handlers["test.setup.status"]({})
                    self.assertTrue(before["finish_ok"], before)
                    old_setup = before["setup_id"]
                    self.facts["sample"]["display_status"] = "failed"  # Runtime failure, not a configured-byte change.
                    original = getattr(setup.SetupStore, phase)
                    calls = [0]

                    def transient(store, *args, **kwargs):
                        calls[0] += 1
                        if calls[0] == 1:
                            raise OSError("one-attempt recorder fault")
                        return original(store, *args, **kwargs)

                    with patch.object(setup.SetupStore, phase, transient), patch.object(unit, "tests_row", return_value=shared.row("tests", "sample", "pass", "mocked installed suite")), patch.object(unit, "collect", wraps=unit.collect) as collect:
                        latest = handlers["test.unit"]({})
                    collect.assert_called_once()  # No replay of the failed health attempt.
                    self.assertEqual(latest["health_verdict"], "fail")
                    self.assertFalse(latest["recorded"])
                    recovered = handlers["test.setup.status"]({})
                    self.assertEqual(recovered["setup_id"], old_setup)
                    self.assertGreater(recovered["generation"], before["generation"])
                    self.assertFalse(recovered["finish_ok"], recovered)
                    self.assertNotEqual(recovered["unit"]["state"], "pass")

    def test_in_app_invalidation_error_reads_fresh_policy_not_cached_on(self):
        handlers = {}
        ext = Mock()
        ext.info, ext.vqro = self.hello, self.client
        ext.entries = self.snapshot["entries"]
        ext.value.side_effect = lambda key, default=None: True if key == setup.POLICY_KEY else default
        ext.command.side_effect = lambda name, title: lambda fn: handlers.setdefault(name, fn)
        with patch("vqro_ext.Extension", return_value=ext), patch.object(unit.threading, "Thread"), \
             patch.object(setup.SetupStore, "invalidate", side_effect=OSError("evidence store inaccessible")), \
             patch.dict(os.environ, {"VQRO_EXTENSIONS_DIR": str(self.ext)}):
            unit.extension_main()
            for raw, expected in ((True, "on"), (False, "off"), ("malformed", "unknown")):
                self.snapshot["entries"][setup.POLICY_KEY]["value"] = raw
                status = handlers["test.setup.status"]({})
                self.assertEqual(status["policy_state"], expected)
                self.assertEqual(status["require_unit"], raw is True)
                self.assertFalse(status["finish_ok"])
                self.assertEqual(status["unit"]["state"], "error")
            original = self.client.call.side_effect

            def unavailable(method, *args, **kwargs):
                if method == "workspace.snapshot":
                    raise OSError("fresh snapshot unavailable")
                return original(method, *args, **kwargs)

            self.client.call.side_effect = unavailable
            status = handlers["test.setup.status"]({})
            self.assertEqual(status["policy_state"], "unknown")
            self.assertFalse(status["require_unit"])
            self.assertFalse(status["finish_ok"])

    def test_malformed_policy_has_explicit_unknown_state(self):
        self.snapshot["entries"][setup.POLICY_KEY]["value"] = "not-a-boolean"
        status = self.status()
        self.assertEqual(status["policy_state"], "unknown")
        self.assertFalse(status["finish_ok"])
        self.assertIn("Unit policy: unknown", unit.setup_page(status)["items"][1]["text"])

    def test_policy_off_status_and_health_never_touch_recorder_lock(self):
        self.snapshot["entries"][setup.POLICY_KEY]["value"] = False
        with patch.dict(sys.modules, {"fcntl": None}), patch.object(setup.SetupStore, "locked", side_effect=AssertionError("OFF must not lock")), patch.object(unit, "tests_row", return_value=shared.row("tests", "sample", "pass", "portable mocked suite")):
            status = self.status()
            result = unit.collect(self.state, self.ext, client=self.client, hello=self.hello, record=False)
        self.assertFalse(status["require_unit"])
        self.assertEqual(result["health_verdict"], "pass")
        self.assertEqual(result["recording"]["state"], "not-requested")
        self.assertFalse(result["recorded"])
        self.assertFalse((self.state / "test-unit/setup-state.lock").exists())

    def test_rpc_followup_notice_is_in_status_page_segment_and_receipt(self):
        self.snapshot["entries"]["extension/pi"] = {"version": 1, "value": {"enabled": True, "argv": [str(self.image), "pi.py"]}}
        (self.ext / "pi.py").write_text("# declared RPC launcher fixture\n")
        (self.ext / "manifests/pi.json").write_text('{"name":"pi"}')
        self.facts["pi"] = {"enabled": True, "display_status": "running", "crashes": 0, "manifest": {"name": "pi"}, "dependencies": {"status": "ok"}}
        with patch.object(unit, "tests_row", side_effect=lambda name, *args, **kwargs: shared.row("tests", name, "pass", "portable mocked suite")):
            result = unit.collect(self.state, self.ext, client=self.client, hello=self.hello, record=True)
        status = self.status()
        self.assertIn(setup.RPC_RULE_NOTICE, status["not_verified_reasons"])
        self.assertEqual(unit.notice(status)["label"], setup.RPC_RULE_NOTICE)
        self.assertTrue(any(setup.RPC_RULE_NOTICE in row.get("text", "") for row in unit.setup_page(status)["items"]))
        self.assertIn(setup.RPC_RULE_NOTICE, result["not_covered"])
        for suite in ("integration", "perf"):
            report = {"not_covered": []}
            setup.bind_start(report, self.value(), self.snapshot, self.state, suite)
            self.assertIn(setup.RPC_RULE_NOTICE, report["not_covered"])
        self.snapshot["entries"]["extension/pi"]["value"]["enabled"] = False
        self.assertIsNone(setup.rpc_rule_notice(self.snapshot))
        self.snapshot["entries"]["extension/custom-pi-launcher"] = {"version": 1, "value": {"enabled": True, "argv": [str(self.image)],
            "manifest": {"provides": ["command/pi.new"]}}}
        self.assertEqual(setup.rpc_rule_notice(self.snapshot), setup.RPC_RULE_NOTICE)

    def test_missing_fcntl_preserves_health_checks_without_completion_evidence(self):
        self.coverage()
        with patch.dict(sys.modules, {"fcntl": None}), patch.object(unit, "tests_row", return_value=shared.row("tests", "sample", "pass", "portable mocked suite")) as run:
            result = unit.collect(self.state, self.ext, client=self.client, hello=self.hello, record=True)
            status = unit.setup_status(self.state, self.ext, client=self.client, hello=self.hello)
        run.assert_called_once()
        self.assertEqual(result["health_verdict"], "pass")
        self.assertEqual(result["verdict"], "unverified")
        self.assertFalse(result["recorded"])
        self.assertEqual(result["recording"]["state"], "unsupported")
        self.assertIn("fcntl", result["recording"]["reason"])
        self.assertTrue(any(row["id"] == "tests/sample" and row["status"] == "pass" for row in result["rows"]))
        self.assertFalse(any(row["id"] == "error/harness" for row in result["rows"]))
        self.assertFalse(status["finish_ok"])
        self.assertFalse((self.state / "test-unit/setup-state.lock").exists())

    def test_observer_error_does_not_skip_portable_health_checks(self):
        with patch.object(setup.SetupStore, "observe", side_effect=RuntimeError("observer fixture failure")), patch.object(unit, "tests_row", return_value=shared.row("tests", "sample", "pass", "portable mocked suite")) as run:
            result = unit.collect(self.state, self.ext, client=self.client, hello=self.hello, record=True)
        run.assert_called_once()
        self.assertEqual(result["health_verdict"], "pass")
        self.assertEqual(result["recording"]["state"], "unsupported")
        self.assertFalse(result["recorded"])
        self.assertFalse(any(row["id"] == "error/harness" for row in result["rows"]))

    def test_record_cli_with_unsupported_lock_prints_health_without_writing_a_record(self):
        with patch.dict(sys.modules, {"fcntl": None}), patch("vqro_testing.PlainClient", return_value=self.client), patch.object(unit, "tests_row", return_value=shared.row("tests", "sample", "pass", "portable mocked suite")), patch.object(unit, "save_receipt", side_effect=AssertionError("must not publish unsupported recording")), contextlib.redirect_stdout(io.StringIO()) as stdout:
            code = unit.main(["--json", "--record", "--socket", self.hello["socket"], "--state-dir", str(self.state), "--extensions-dir", str(self.ext)])
        result = json.loads(stdout.getvalue())
        self.assertEqual(code, 77)
        self.assertEqual(result["health_verdict"], "pass")
        self.assertEqual(result["recording"]["state"], "unsupported")
        self.assertFalse(result["recorded"])
        self.assertNotIn("receipt", result)

    def test_record_cli_refuses_no_tests_before_connecting_or_writing(self):
        before = set(self.state.iterdir())
        with patch("vqro_testing.PlainClient", side_effect=AssertionError("must not connect")), contextlib.redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit) as error:
                unit.main(["--record", "--no-tests", "--state-dir", str(self.state), "--extensions-dir", str(self.ext)])
        self.assertEqual(error.exception.code, 2)
        self.assertEqual(set(self.state.iterdir()), before)

    def test_caller_disabled_tests_cannot_grant_finish_even_with_matching_pass_receipt(self):
        self.coverage()
        for enabled in (False, None):
            path, result = self.record()
            result["tests_enabled"] = enabled
            shared.write_receipt(result, path)
            self.store.skip(self.value(), self.snapshot, "integration", "unit-only fixture")
            status = self.status()
            self.assertEqual(status["unit"]["state"], "unverified")
            self.assertFalse(status["finish_ok"])

    def test_in_app_style_record_with_tests_disabled_is_unverified(self):
        self.coverage()
        result = unit.collect(self.state, self.ext, client=self.client, hello=self.hello, tests=False, record=True)
        self.assertNotEqual(result["verdict"], "pass")
        self.assertFalse(result["tests_enabled"])
        shared.save_receipt(result, self.state, result.get("receipt"))
        self.store.skip(self.value(), self.snapshot, "integration", "unit-only fixture")
        self.assertFalse(self.status()["finish_ok"])

    def test_fingerprint_covers_recursive_packages_assets_tests_settings_and_images(self):
        baseline = self.value()
        self.assertEqual(baseline["version"], 2)
        self.assertIsNotNone(baseline["setup_id"])
        for relative in ("pkg/nested/code.py", "tests/sample/scenarios/change.json", "assets/theme.css"):
            path = self.ext / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("new installed input")
            after = self.value()
            self.assertNotEqual(after["setup_id"], baseline["setup_id"])
            baseline = after
        self.snapshot["entries"]["doc/settings/sample"] = {"version": 1, "value": {"enabled": True}}
        after = self.value()
        self.assertNotEqual(after["setup_id"], baseline["setup_id"])
        self.image.write_text("#!/bin/sh\nexit 1\n")
        self.assertNotEqual(self.value()["setup_id"], after["setup_id"])

    def test_receipts_logs_and_caches_never_dirty_the_setup(self):
        baseline = self.value()["setup_id"]
        for relative in ("__pycache__/sample.pyc", ".run/whatever", "test-unit/receipt-fixture.json", "receipt-fixture.json", "ext-sample.log", "logs/running.log", "receipts/data.json"):
            path = self.ext / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text("not a configured installed input")
        self.assertEqual(self.value()["setup_id"], baseline)
        (self.ext / "tests/test-unit").mkdir(parents=True)
        (self.ext / "tests/test-unit/test_fixture.py").write_text("# installed suite, not a result directory\n")
        self.assertNotEqual(self.value()["setup_id"], baseline)
        first = self.value()["setup_id"]
        (self.ext / "tests/test-unit/receipt-scenario.json").write_text('{"steps":[]}')
        self.assertNotEqual(self.value()["setup_id"], first)

    def test_unknown_oversized_unreadable_and_unresolved_inputs_never_hash_as_empty(self):
        missing = self.snapshot["entries"]["extension/sample"]["value"]["argv"]
        missing[0] = str(self.root / "not-installed")
        value = self.value()
        self.assertIsNone(value["setup_id"])
        self.assertTrue(any("argv[0]" in reason for reason in value["unknown"]))
        missing[0] = str(self.image)
        asset = self.ext / "big.asset"
        asset.write_bytes(b"12345")
        with patch.object(shared, "ASSET_FILE_BYTES", 4):
            self.assertIsNone(self.value()["setup_id"])
        asset.unlink()
        original = Path.open

        def denied(path, *args, **kwargs):
            if path == self.ext / "sample.py":
                raise PermissionError("fixture unreadable")
            return original(path, *args, **kwargs)

        with patch.object(Path, "open", denied):
            value = self.value()
        self.assertIsNone(value["setup_id"])
        self.assertTrue(any("unreadable" in reason for reason in value["unknown"]))
        status = self.store.status(value, self.snapshot)
        self.assertFalse(status["finish_ok"])
        self.assertTrue(status["not_verified_reasons"])

    def test_generation_catches_entry_change_revert_and_observer_restart(self):
        path, _ = self.record()
        before = self.status()
        self.snapshot["entries"]["extension/sample"]["version"] += 2
        after = self.status()
        self.assertEqual(before["setup_id"], after["setup_id"])
        self.assertGreater(after["generation"], before["generation"])
        self.assertEqual(after["unit"]["state"], "stale")
        self.store.invalidate("missed entries")
        missed = self.status()
        self.assertGreater(missed["generation"], after["generation"])
        self.assertNotEqual(missed["unit"]["state"], "pass")
        self.store.invalidate("observer restarted")
        self.assertGreater(self.status()["generation"], missed["generation"])
        self.assertTrue(path.exists())

    def test_default_off_and_unknown_identity_are_distinct(self):
        self.snapshot["entries"].pop(setup.POLICY_KEY)
        status = self.status()
        self.assertFalse(status["require_unit"])
        self.assertTrue(status["finish_ok"])
        self.assertEqual(unit.notice(status)["label"], "NOT VERIFIED")
        self.snapshot["entries"]["extension/sample"]["value"]["argv"][0] = str(self.root / "missing")
        status = self.status()
        self.assertIsNone(status["setup_id"])
        self.assertFalse(status["finish_ok"])

    def test_matching_pass_and_explicit_integration_skip_allow_finish(self):
        self.record()
        status = self.status()
        self.assertEqual(status["unit"]["state"], "pass")
        self.assertFalse(status["finish_ok"])
        status = self.store.skip(self.value(), self.snapshot, "integration", "No behavioural change; reviewed explicitly")
        self.assertEqual(status["integration"]["state"], "skipped")
        self.assertTrue(status["finish_ok"])
        self.assertEqual(status["performance"]["state"], "missing")
        self.assertIn("NOT VERIFIED", unit.notice(status)["label"])

    def test_coverage_gaps_warn_but_do_not_turn_a_unit_pass_into_a_finish_block(self):
        self.record(coverage=False)
        status = self.store.skip(self.value(), self.snapshot, "integration", "explicit unit-only change")
        self.assertEqual(status["unit"]["verdict"], "pass")
        self.assertEqual(status["unit"]["state"], "pass")
        self.assertTrue(status["finish_ok"])
        self.assertEqual(status["coverage_gaps"], ["no tests installed for sample"])
        self.assertTrue(any("Coverage gap:" in item.get("text", "") for item in unit.setup_page(status)["items"]))

    def test_only_current_owner_can_accept_and_exception_expires_after_change(self):
        self.record(verdict="fail")
        self.store.skip(self.value(), self.snapshot, "integration", "Intentional skip")
        for caller, owner in (({"actor": "agent", "principal": "human"}, "human"), ({"actor": "human", "principal": "agent"}, "human"), (None, "human"), ({"actor": "human", "principal": "human"}, None)):
            with self.subTest(caller=caller), self.assertRaisesRegex(PermissionError, "workspace owner principal"):
                self.store.accept(self.value(), self.snapshot, "explicit exception", caller, owner)
        status = self.store.accept(self.value(), self.snapshot, "Known failing check accepted", {"actor": "human", "principal": "human"}, "human")
        self.assertTrue(status["finish_ok"])
        self.assertEqual(status["unit"]["state"], "fail")
        self.assertEqual(status["unit"]["exception"]["accepted_by"], "human")
        (self.ext / "sample.py").write_text("# changed again\n")
        status = self.status()
        self.assertIsNone(status["unit"]["exception"])
        self.assertFalse(status["finish_ok"])

    def test_wrong_endpoint_suite_generation_and_truncated_receipts_never_pass(self):
        path, original = self.record()
        corruptions = [dict(original, instance={**original["instance"], "socket": "different-instance"}),
                       dict(original, suite="integration"), dict(original, generation=original["generation"] - 1),
                       dict(original, closure={"matched": True}), dict(original, version=True),
                       dict(original, rows=[], counts={})]
        for malformed in corruptions:
            with self.subTest(malformed=malformed):
                shared.write_receipt(malformed, path)
                self.assertNotEqual(self.status()["unit"]["state"], "pass")
                self.assertFalse(self.status()["finish_ok"])
        path.write_text('{"kind":')
        self.assertEqual(self.status()["unit"]["state"], "unverified")
        self.assertFalse(self.status()["finish_ok"])

    def test_pending_attempt_is_not_replaced_by_an_older_pass(self):
        self.record()
        state = self.store.observe(self.value(), self.snapshot)
        pending = self.store.begin(state)
        self.assertEqual(self.status()["unit"]["state"], "pending")
        self.assertFalse(self.status()["finish_ok"])
        Path(pending["receipt"]).write_text('{"kind":')
        self.assertEqual(self.status()["unit"]["state"], "unverified")
        self.assertFalse(self.status()["finish_ok"])

    def test_pending_identity_uses_pid_start_time_not_volatile_scheduler_state(self):
        context = self.context()
        destination = self.store.directory / "receipt-pending.json"
        with patch.object(setup, "process_identity", return_value=(123, "R")):
            self.store.pending("unit", destination, 777, context)
        with patch.object(setup, "process_identity", return_value=(123, "S")):
            self.assertEqual(self.status()["unit"]["state"], "pending")
        with patch.object(setup, "process_identity", return_value=(456, "S")):
            self.assertEqual(self.status()["unit"]["state"], "error")

    def test_integration_pending_failure_and_skip_never_hide_a_result(self):
        self.record()
        self.store.skip(self.value(), self.snapshot, "integration", "initially not needed")
        self.record(suite="integration")
        context = self.context()
        destination = self.state / "test-integration/receipt-pending.json"
        self.store.pending("integration", destination, os.getpid(), context)
        self.assertEqual(self.status()["integration"]["state"], "pending")
        self.assertFalse(self.status()["finish_ok"])
        with self.assertRaises(ValueError):
            self.store.skip(self.value(), self.snapshot, "integration", "hide the pending run")
        _, failed = self.record(suite="integration", verdict="fail")
        shared.write_receipt(failed, destination)
        status = self.status()
        self.assertEqual(status["integration"]["state"], "fail")
        self.assertIsNone(status["integration"]["skip_reason"])
        with self.assertRaises(ValueError):
            self.store.skip(self.value(), self.snapshot, "integration", "hide the failure")
        self.assertTrue(status["finish_ok"])  # attached failure is disclosed, not called a pass.
        self.store.pending("integration", self.state / "test-integration/receipt-later.json", 99999999, context)
        self.assertEqual(self.status()["integration"]["state"], "fail")
        self.assertIsNone(self.status()["integration"]["skip_reason"])
        with self.assertRaises(ValueError):
            self.store.skip(self.value(), self.snapshot, "integration", "a dead later attempt cannot hide the completed failure")

    def test_disabled_guard_and_corrupt_state_are_prominently_non_green(self):
        self.record()
        self.store.skip(self.value(), self.snapshot, "integration", "explicit reason")
        self.snapshot["extensions"] = {"test-unit": {"enabled": False, "display_status": "off"}}
        status = self.status()
        self.assertFalse(status["finish_ok"])
        self.assertTrue(any("changes are not tracked" in reason for reason in status["not_verified_reasons"]))
        status = self.store.accept(self.value(), self.snapshot, "Owner explicitly accepts this current guard exception", {"actor": "human", "principal": "human"}, "human")
        self.assertTrue(status["finish_ok"])
        self.assertEqual(unit.notice(status)["label"].split(" · ")[0], "NOT VERIFIED")
        self.store.path.write_text('{"version":')
        status = unit.setup_status(self.state, self.ext, client=self.client, hello=self.hello)
        self.assertEqual(status["unit"]["state"], "error")
        self.assertFalse(status["finish_ok"])
        self.store.path.unlink()
        self.assertNotEqual(self.status()["unit"]["state"], "pass")

    def test_recorded_unit_has_before_after_closure_and_cli_adopts_its_own_path(self):
        self.coverage()
        self.store.skip(self.value(), self.snapshot, "integration", "unit-only change")
        with patch.object(shared, "PlainClient", return_value=self.client), contextlib.redirect_stdout(io.StringIO()) as output:
            code = unit.main(["--json", "--record", "--socket", self.hello["socket"], "--state-dir", str(self.state), "--extensions-dir", str(self.ext)])
        result = json.loads(output.getvalue())
        self.assertEqual(code, 0)
        self.assertTrue(result["recorded"])
        self.assertTrue(result["closure"]["matched"])
        self.assertTrue(Path(result["receipt"]).is_file())
        self.assertEqual(Path(result["receipt"]).stat().st_mode & 0o777, 0o600)
        self.assertEqual(self.status()["unit"]["state"], "pass")
        with patch.object(shared, "PlainClient", return_value=self.client), contextlib.redirect_stdout(io.StringIO()) as output:
            self.assertEqual(unit.main(["--setup-status", "--json", "--socket", self.hello["socket"], "--state-dir", str(self.state), "--extensions-dir", str(self.ext)]), 0)
        self.assertTrue(json.loads(output.getvalue())["finish_ok"])

    def test_ad_hoc_out_is_not_an_acknowledgement_even_in_the_receipt_directory(self):
        self.coverage()
        self.status()
        path = self.store.directory / "receipt-adhoc.json"
        with patch.object(shared, "PlainClient", return_value=self.client), contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(unit.main(["--json", "--out", str(path), "--socket", self.hello["socket"], "--state-dir", str(self.state), "--extensions-dir", str(self.ext)]), 0)
        self.assertFalse(json.loads(path.read_text())["recorded"])
        self.assertNotEqual(self.status()["unit"]["state"], "pass")

    def test_unit_concurrent_edit_makes_the_receipt_unverified(self):
        self.coverage()

        def changed(*args, **kwargs):
            (self.ext / "sample.py").write_text("# changed while checking\n")
            return shared.row("tests", "sample", "pass", "fixture passed")

        with patch.object(unit, "tests_row", side_effect=changed):
            result = unit.collect(self.state, self.ext, client=self.client, hello=self.hello, record=True)
        shared.save_receipt(result, self.state, result["receipt"])
        self.assertEqual(result["verdict"], "unverified")
        self.assertFalse(result["closure"]["matched"])
        self.assertNotEqual(self.status()["unit"]["state"], "pass")
        self.assertFalse(self.status()["finish_ok"])


if __name__ == "__main__":
    unittest.main()
