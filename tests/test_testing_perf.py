"""Deterministic performance-policy tests: no live core, models or installed user files."""
import contextlib
import copy
import io
import json
import math
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
import testing_perf as perf
import vqro_testing as shared

WORKLOAD = {"repeats": 5, "window_s": 10, "settle_s": 2}


def measured(ident, samples, unit="ms"):
    return perf.measurement(ident, samples, 5, unit, WORKLOAD)


class ThresholdTests(unittest.TestCase):
    def limits(self, value):
        return perf.thresholds({"entries": {"doc/settings/test-perf": {"value": value}}})

    def test_defaults_are_copied_and_settings_merge_nested_fields(self):
        defaults, warnings = perf.thresholds({})
        self.assertEqual(defaults["source"], "default")
        self.assertFalse(warnings)
        config, warnings = self.limits({"relative_slowdown": 0.5, "absolute": {"interaction_ms": 900, "startup_core_ms": 50}})
        self.assertEqual(config["source"], "settings")
        self.assertEqual(config["values"]["relative_slowdown"], 0.5)
        self.assertEqual(config["values"]["absolute"]["interaction_ms"], 900)
        self.assertEqual(config["values"]["absolute"]["rss_mb"], 90)
        self.assertIn("absolute.interaction_ms", config["overrides"])
        self.assertEqual(perf.DEFAULTS["absolute"]["interaction_ms"], 170)
        self.assertFalse(warnings)

    def test_unknown_settings_are_ignored_and_listed(self):
        config, warnings = self.limits({"future_option": 5, "absolute": {"future_ceiling": 10}})
        self.assertEqual(config["ignored"], ["future_option", "absolute.future_ceiling"])
        self.assertEqual(config["values"], perf.DEFAULTS)
        self.assertFalse(warnings)

    def test_malformed_settings_fall_back_with_warning_never_failure(self):
        for value in (None, [], "bad", {"relative_slowdown": -1}, {"noise_ms": float("nan")},
                      {"idle_new_activity": 1}, {"absolute": False}, {"absolute": {"rss_mb": "many"}}):
            with self.subTest(value=value):
                config, warnings = self.limits(value)
                self.assertEqual(config["source"], "default")
                self.assertEqual(config["values"], perf.DEFAULTS)
                self.assertEqual(warnings[0]["id"], "thresholds/settings")
                self.assertEqual(warnings[0]["status"], "warn")
                self.assertEqual(perf.verdict(warnings), "pass")

    def test_null_absolute_disables_only_its_ceiling(self):
        config, _ = self.limits({"absolute": {"interaction_ms": None}})
        rows = perf.absolute_rows([measured("interaction/fixture/x/1/reply", [1000] * 5)], config["values"], 10)
        self.assertEqual(rows[0]["status"], "pass")


class MeasurementTests(unittest.TestCase):
    def test_statistics_and_raw_samples(self):
        result = measured("startup/core", [5, 1, 3, 2, 4])
        self.assertEqual(result["status"], "pass")
        self.assertEqual(result["measure"], {"n": 5, "median": 3, "min": 1, "max": 5,
                                             "samples": [5, 1, 3, 2, 4], "unit": "ms", "workload": WORKLOAD})

    def test_missing_zero_nonfinite_and_negative_samples_fail(self):
        for values in ([], [1], [1, 2, 3, 4, float("nan")], [1, 2, 3, 4, math.inf],
                       [1, 2, 3, 4, -1], [1, 2, 3, 4, True], [1, 2, 3, 4, "5"]):
            with self.subTest(values=values):
                result = measured("startup/core", values)
                self.assertEqual(result["status"], "fail")
                self.assertEqual(perf.verdict([result]), "fail")
                json.dumps(result, allow_nan=False)
        self.assertEqual(perf.measurement("startup/core", [], 0, "ms", {})["status"], "fail")

    def test_absolute_breaches_warn_and_never_fail(self):
        rows = [measured("startup/core", [500] * 5), measured("interaction/fixture/x/1/reply", [200] * 5),
                measured("idle/cpu/fixture", [5] * 5, "cpu_s"), measured("idle/rss/fixture", [100] * 5, "mb"),
                measured("size/item/panel", [2000000] * 5, "bytes"), measured("size/total", [10000000] * 5, "bytes"),
                measured("idle/events", [5] * 5, "events_per_s")]
        result = perf.absolute_rows(rows, perf.DEFAULTS, 10)
        self.assertEqual(result[0]["status"], "pass")
        self.assertTrue(all(r["status"] == "warn" for r in result[1:]))
        self.assertEqual(perf.verdict(result), "pass")

    def compare(self, ident, baseline, candidate, unit="ms", limits=None):
        limits = limits or perf.DEFAULTS
        before = {"rows": perf.absolute_rows([measured(ident, baseline, unit)], limits, 10)}
        after = {"rows": perf.absolute_rows([measured(ident, candidate, unit)], limits, 10)}
        return perf.evaluate(before, after, limits, 10)

    def test_relative_slowdown_is_warning_outside_spread(self):
        rows = self.compare("interaction/fixture/x/1/reply", [10, 11, 12, 13, 14], [200, 201, 202, 203, 204])
        self.assertEqual(rows[0]["id"], "compare/interaction/fixture/x/1/reply")
        self.assertEqual(rows[0]["status"], "warn")
        self.assertIn("slower", rows[0]["evidence"])
        self.assertEqual(rows[0]["measure"]["difference"], 190)
        self.assertAlmostEqual(rows[0]["measure"]["ratio"], 202 / 12)
        self.assertEqual(perf.verdict(rows), "pass")

    def test_noise_is_unverified_not_pass(self):
        rows = self.compare("startup/core", [1, 10, 10, 10, 100], [5, 30, 30, 30, 110])
        self.assertEqual(rows[0]["status"], "unverified")
        self.assertIn("can't tell", rows[0]["evidence"])
        self.assertEqual(perf.verdict(rows), "unverified")

    def test_relative_floor_ratio_and_spread_all_matter(self):
        for before, after in [([1] * 5, [2] * 5), ([100] * 5, [120] * 5)]:
            self.assertEqual(self.compare("startup/core", before, after)[0]["status"], "pass")
        self.assertEqual(self.compare("idle/rss/fixture", [50] * 5, [80] * 5, "mb")[0]["status"], "warn")
        self.assertEqual(self.compare("size/item/panel", [10] * 5, [21] * 5, "bytes")[0]["status"], "warn")

    def test_new_idle_activity_warns(self):
        rows = self.compare("idle/events", [0] * 5, [10] * 5, "events_per_s")
        self.assertEqual(rows[0]["status"], "warn")
        self.assertIn("new steady idle activity", rows[0]["evidence"])
        self.assertEqual(perf.verdict(rows), "pass")
        self.assertIsNone(rows[0]["measure"]["ratio"])

    def test_broken_measurements_always_count_with_candidate(self):
        before = {"rows": [measured("startup/core", [1] * 5)]}
        after = {"rows": [measured("startup/core", [2]) ]}
        rows = perf.evaluate(before, after, perf.DEFAULTS, 10)
        self.assertEqual(rows[0]["status"], "fail")
        self.assertEqual(perf.verdict(rows), "fail")
        rows = perf.evaluate(before, {"rows": []}, perf.DEFAULTS, 10)
        self.assertEqual(rows[0]["status"], "fail")
        self.assertEqual(rows[0]["measure"]["n"], 0)

    def test_verdict_precedence_and_warn_never_fails(self):
        warning = shared.row("idle", "events", "warn", "busy")
        unknown = shared.row("idle", "cpu/x", "unverified", "unavailable")
        broken = measured("startup/core", [])
        self.assertEqual(perf.verdict([warning]), "pass")
        self.assertEqual(perf.verdict([warning, unknown]), "unverified")
        self.assertEqual(perf.verdict([unknown, broken]), "fail")
        self.assertEqual(perf.verdict([broken], error=True), "error")


class MonitorTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="vqp-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / "manifests").mkdir()
        (self.root / "manifests/fixture.json").write_text(json.dumps({"name": "fixture", "provides": ["command/fixture.show"]}))
        self.sandbox = Mock(ext=self.root, declarations={"extension/fixture": {"argv": ["python3", "fixture.py"]}})
        self.sandbox.enabled.return_value = ["fixture"]
        self.monitor = perf.Monitor(self.sandbox, {})

    def event(self, kind, data, stamp):
        self.monitor({"method": "event", "params": {"type": kind, "data": data}}, stamp)

    def test_startup_waits_for_running_and_registered_command_events(self):
        self.monitor({"method": "sandbox.activating"}, 10)
        self.event("extension", {"name": "fixture", "state": {"status": "running"}}, 11)
        self.assertNotIn("fixture", self.monitor.ready)
        self.event("command", {"name": "fixture.show", "available": True}, 12)
        self.assertEqual(self.monitor.ready["fixture"] - self.monitor.activation, 2)

    def test_idle_event_content_attribution_and_size_peaks(self):
        self.monitor.window = (10, 20)
        self.event("view.content", {"view": "panel", "content": {"text": "probe"}, "author": "extension:fixture"}, 9)
        self.event("view.content", {"view": "panel", "content": {"text": "probe"}, "author": "extension:fixture"}, 12)
        size = len(shared.canonical({"text": "probe"}))
        self.assertEqual(self.monitor.events, 1)
        self.assertEqual(self.monitor.content, size)
        self.assertEqual(self.monitor.ext_events["fixture"], 1)
        self.assertEqual(self.monitor.ext_content["fixture"], size)
        self.assertEqual(self.monitor.item_max["panel"], size)
        self.assertEqual(self.monitor.total_max, size)
        self.event("view.content", {"view": "panel", "content": None}, 21)
        self.assertFalse(self.monitor.live_sizes)
        self.assertEqual(self.monitor.total_max, size)

    def test_interaction_reply_and_content_can_arrive_in_either_order(self):
        scenario = {"steps": [{"invoke": "fixture.show"}, {"wait": {"content": "panel", "contains": "probe"}}]}
        self.monitor.client = Mock(step_deadline=100)
        with patch.object(perf.time, "monotonic", side_effect=[10, 10.2]):
            self.monitor.step("before", "fixture/feature", 1, scenario["steps"][0], scenario)
            self.event("view.content", {"view": "panel", "content": {"text": "probe"}}, 10.1)
            self.monitor.step("reply", "fixture/feature", 1, scenario["steps"][0], scenario)
            self.monitor.step("after", "fixture/feature", 2, scenario["steps"][1], scenario)
        self.assertAlmostEqual(self.monitor.values["interaction/fixture/feature/1/reply"], 200)
        self.assertAlmostEqual(self.monitor.values["interaction/fixture/feature/1/content"], 100)

    def test_entry_content_timing_and_event_gaps(self):
        self.monitor.active = {"id": "interaction/fixture/x/1", "started": 1, "seen": None,
                               "predicate": {"entry": "doc/x", "path": "value", "equals": "ready"}}
        self.event("entries", {"changes": [{"key": "doc/x", "version": 2, "value": {"value": "ready"}}]}, 1.2)
        self.assertEqual(self.monitor.active["seen"], 1.2)
        self.monitor({"method": "missed"}, 2)
        self.assertTrue(self.monitor.gap)


class CollectorTests(unittest.TestCase):
    def test_missing_repeat_is_fail_and_all_unavailable_is_unverified(self):
        sandbox = Mock()
        sandbox.enabled.return_value = ["fixture"]
        records = [{"values": {"startup/core": 1}, "unavailable": {"idle/rss/fixture": "unavailable"}, "errors": []},
                   {"values": {}, "unavailable": {"idle/rss/fixture": "unavailable"}, "errors": []}]
        with patch.object(perf, "expected_scopes", return_value={"startup/core": "ms", "idle/rss/fixture": "mb"}), patch.object(perf, "collect_copy", side_effect=records):
            section = perf.collect_section([sandbox, sandbox], {}, 2, 1, perf.DEFAULTS)
        rows = {r["id"]: r for r in section["rows"]}
        self.assertEqual(rows["startup/core"]["status"], "fail")
        self.assertEqual(rows["idle/rss/fixture"]["status"], "unverified")
        self.assertIsNone(rows["idle/rss/fixture"]["measure"]["median"])

    def test_invalid_repeats_or_window_are_structured_error_without_launch(self):
        for flag, value in [("--repeats", "0"), ("--repeats", "51"), ("--repeats", "1.5"), ("--repeats", "bad"),
                            ("--window", "0"), ("--window", "121"), ("--window", "nan")]:
            with self.subTest(flag=flag, value=value), patch.object(perf, "Sandbox") as sandbox:
                with contextlib.redirect_stdout(io.StringIO()) as output:
                    code = perf.main(["--json", flag, value])
                receipt = json.loads(output.getvalue())
                self.assertEqual(code, 2)
                self.assertEqual(receipt["verdict"], "error")
                self.assertFalse(receipt["sandbox"]["started"])
                sandbox.assert_not_called()

    def test_settings_override_receipt_and_busy_machine_warnings_do_not_fail(self):
        with tempfile.TemporaryDirectory(prefix="vqpt-") as directory:
            root = Path(directory)
            ext, state = root / "ext", root / "state"
            ext.mkdir()
            state.mkdir()
            declaration = {"argv": ["python3", "fixture.py"]}
            (ext / "fixture.py").write_text("# fixture\n")
            snapshot = {"entries": {"extension/fixture": {"value": declaration},
                                    "doc/settings/test-perf": {"value": {"absolute": {"startup_core_ms": 0.5}}}}}
            workspace = state / "workspace.json"
            workspace.write_text(json.dumps(snapshot))
            original = workspace.read_bytes()
            sandbox = Mock(ext=ext, declarations={"extension/fixture": declaration}, leftovers=[],
                           info={"started": False, "canary": {"status": "pass"}, "vqro": {"version": "fixture"}, "not_copied": []})

            def collect(group, scenarios, repeats, window, limits):
                item = perf.measurement("startup/core", [1], 1, "ms", {"repeats": 1, "window_s": 1, "settle_s": 2})
                return {"rows": perf.absolute_rows([item], limits, window), "runs": []}

            out = root / "receipt.json"
            with patch.object(perf, "Sandbox", return_value=sandbox), patch.object(perf, "collect_section", side_effect=collect), patch.object(perf, "machine", return_value={"cpu_count": 1, "loadavg": [2, 2, 2]}), contextlib.redirect_stdout(io.StringIO()) as output:
                code = perf.main(["--json", "--offline", "--state-dir", str(state), "--extensions-dir", str(ext),
                                  "--root", str(root / "run"), "--repeats", "1", "--window", "1", "--out", str(out)])
            receipt = json.loads(output.getvalue())
            self.assertEqual(code, 0)
            self.assertEqual(receipt["verdict"], "pass")
            self.assertEqual(receipt["thresholds"]["source"], "settings")
            self.assertEqual(receipt["thresholds"]["values"]["absolute"]["startup_core_ms"], 0.5)
            self.assertEqual(receipt["counts"]["warn"], 2)
            self.assertEqual(receipt, json.loads(out.read_text()))
            if sys.platform != "win32":  # Windows inherits directory ACLs, not POSIX mode bits.
                self.assertEqual(out.stat().st_mode & 0o777, 0o600)
            self.assertEqual(workspace.read_bytes(), original)
            self.assertFalse((root / "run").exists())

    def test_async_extension_scrubs_environment_and_returns_without_waiting(self):
        with tempfile.TemporaryDirectory(prefix="vqpa-") as directory:
            ext, child, handlers = Mock(), Mock(), {}
            ext.info = {"state_dir": directory, "socket": str(Path(directory) / "source.sock")}
            ext.command.side_effect = lambda name, title: lambda fn: handlers.setdefault(name, fn)
            child.poll.return_value = None
            child.pid = os.getpid()
            ext.vqro.call.return_value = {"entries": {}, "epoch": "fixture"}
            with patch("vqro_ext.Extension", return_value=ext), patch.object(perf.subprocess, "Popen", return_value=child) as launch, patch.object(perf.threading, "Thread") as thread, patch.dict(os.environ, {"HERDR_PANE_ID": "canary", "VQRO_EXTENSIONS_DIR": directory}):
                perf.extension_main()
                first = handlers["test.perf"]({"repeats": 2})
                self.assertTrue(first["started"])
                child.wait.assert_not_called()
                ext.publish.assert_not_called()
                self.assertEqual(set(launch.call_args.kwargs["env"]), {"HOME", "PATH"})
                self.assertIn("--repeats", launch.call_args.args[0])
                self.assertFalse(handlers["test.perf"]({})["started"])
                result = {"verdict": "pass", "counts": {}, "warnings": "0 performance warnings", "rows": []}
                Path(first["receipt"]).write_text(json.dumps(result))
                thread.call_args.kwargs["target"]()
                child.wait.assert_called_once()
                self.assertEqual(ext.publish.call_args.args[0], "test-perf.report")
                self.assertEqual(handlers["test.perf.show"]({})["client"]["popup"]["view"], "test-perf.report")

    def test_event_observer_timestamps_once_when_message_is_read_not_when_dequeued(self):
        client = object.__new__(shared.SandboxClient)
        client.buffer = bytearray(b'{"method":"event","params":{"type":"command","data":{}}}\n')
        client.event_observer = Mock()
        with patch.object(shared.time, "monotonic", return_value=5):
            message = client.read_message(10)
        client.event_observer.assert_called_once_with(message, 5)
        from collections import deque
        client.events = deque([(message, 10)])
        client.event_bytes = 10
        self.assertEqual(client.next_event(10), message)
        client.event_observer.assert_called_once()

    def test_panel_and_cli_warn_only_line(self):
        result = {"verdict": "pass", "warnings": "1 performance warnings", "counts": {"warn": 1},
                  "rows": [shared.row("idle", "events", "warn", "busy")]}
        self.assertIn("1 performance warnings", perf.panel(result)["items"][0]["text"])
        with patch.object(perf, "run", return_value=result), contextlib.redirect_stdout(io.StringIO()) as output:
            self.assertEqual(perf.main([]), 0)
        self.assertIn("1 performance warnings", output.getvalue())
        result["rows"] *= 1000
        self.assertLessEqual(len(perf.panel(result)["items"]), 400)


if __name__ == "__main__":
    unittest.main()
