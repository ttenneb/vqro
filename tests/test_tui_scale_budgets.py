"""The TUI scale probe enforces its performance budgets (pure check; the probe itself runs in check.sh full)."""
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import e2e_tui_scale as probe  # noqa: E402


class Budgets(unittest.TestCase):
    within = {"cpu_pct_one_core": 18, "rss_peak_mib": 38.6, "key_p99_ms": 81.8,
              "background_fps_peak_complete_second": 55, "fps_peak_complete_second": 94}

    def test_measured_results_pass_with_margin(self):
        self.assertEqual(probe.exceeded(self.within), [])
        for name, (limit, worst) in probe.BUDGETS.items():
            self.assertGreaterEqual(limit, worst, name)

    def test_any_measure_over_its_limit_fails(self):
        for name, (limit, _) in probe.BUDGETS.items():
            self.assertEqual(probe.exceeded({**self.within, name: limit + 1}), [name])

    def test_a_missing_measurement_is_not_a_pass(self):
        for name in probe.BUDGETS:
            self.assertIn(name, probe.exceeded({k: v for k, v in self.within.items() if k != name}))


class DeliveryEvidence(unittest.TestCase):
    def metrics(self, errors=(), closures=(), attaches=()):
        return {"started_unix_s": 1000, "stream_events_dropped": 0,
                "stream_attachments": [
                    {"at_s": 0, "stream": 7, "connection": 0, "resource": "idle"},
                    {"at_s": 0, "stream": 9, "connection": 0, "resource": "flood"}, *attaches],
                "stream_errors": list(errors), "stream_closures": list(closures)}

    def ledger(self):
        ledger = probe.KeyLedger(1000)
        ledger.sent("K00000Z", 1)
        return ledger

    def summary(self, ledger, metrics=None):
        return ledger.summary(self.metrics() if metrics is None else metrics, "idle", "flood", 5)

    def test_late_is_a_deadline_miss_not_lost_even_with_stream_error(self):
        ledger = self.ledger()
        ledger.finish("K00000Z")
        ledger.observe("K00000Z", 3.5)
        metrics = self.metrics(errors=[{"at_s": 1.5, "stream": 7, "connection": 0, "kind": "not_found"}])
        summary = self.summary(ledger, metrics)
        self.assertEqual(summary["missed_deadline"], ["K00000Z"])
        self.assertEqual(summary["lost"], [])
        self.assertEqual(summary["ledger"][0]["seen_s"], 3.5)
        self.assertEqual(summary["p99_ms"], 2500)
        self.assertIn("key_p99_ms", probe.exceeded({**Budgets.within, "key_p99_ms": summary["p99_ms"]}))

    def test_unseen_alone_is_not_proven_loss(self):
        ledger = self.ledger()
        ledger.finish("K00000Z")
        summary = self.summary(ledger)
        self.assertEqual(summary["missed_deadline"], ["K00000Z"])
        self.assertEqual(summary["lost"], [])
        self.assertIsNone(summary["ledger"][0]["seen_s"])
        self.assertEqual(summary["ledger"][0]["stream"], 7)
        self.assertEqual(summary["ledger"][0]["flood_stream"], 9)

    def test_same_stream_error_during_key_window_is_positive_signal(self):
        ledger = self.ledger()
        ledger.finish("K00000Z")
        summary = self.summary(ledger, self.metrics(errors=[{"at_s": 1.5, "stream": 7, "connection": 0, "kind": "overloaded"}]))
        self.assertEqual(summary["lost"], ["K00000Z"])
        self.assertEqual(summary["ledger"][0]["non_delivery"][0]["kind"], "overloaded")

    def test_other_stream_or_earlier_error_is_not_key_loss(self):
        ledger = self.ledger()
        ledger.finish("K00000Z")
        metrics = self.metrics(errors=[
            {"at_s": 1.5, "stream": 9, "connection": 0, "kind": "not_found"},
            {"at_s": .5, "stream": 7, "connection": 0, "kind": "not_found"}],
            closures=[{"at_s": 1.5, "stream": 7, "connection": 0, "reason": {"reason": "slow_consumer"}}])
        self.assertEqual(self.summary(ledger, metrics)["lost"], [])

    def test_error_is_not_assigned_to_an_earlier_key_window(self):
        ledger = self.ledger()
        ledger.finish("K00000Z")
        ledger.sent("K00001Z", 2)
        ledger.finish("K00001Z")
        metrics = self.metrics(errors=[{"at_s": 2.5, "stream": 7, "connection": 0, "kind": "not_found"}])
        self.assertEqual(self.summary(ledger, metrics)["lost"], ["K00001Z"])

    def test_closed_before_send_requires_same_connection_and_current_stream(self):
        close = {"at_s": .5, "stream": 7, "connection": 0, "reason": {"reason": "slow_consumer"}}
        ledger = self.ledger()
        ledger.finish("K00000Z")
        self.assertEqual(self.summary(ledger, self.metrics(closures=[close]))["lost"], ["K00000Z"])
        for connection, stream in [(0, 8), (1, 7)]:
            with self.subTest(connection=connection, stream=stream):
                attach = {"at_s": .8, "stream": stream, "connection": connection, "resource": "idle"}
                self.assertEqual(self.summary(ledger, self.metrics(closures=[close], attaches=[attach]))["lost"], [])

    def test_existing_capture_retains_a_late_prior_token(self):
        ledger = self.ledger()
        ledger.finish("K00000Z")
        ledger.sent("K00001Z", 3.2)
        ledger.observe("K00000Z\nK00001Z", 3.3)
        ledger.finish("K00001Z")
        summary = self.summary(ledger)
        self.assertEqual(summary["missed_deadline"], ["K00000Z"])
        self.assertEqual(summary["lost"], [])
        self.assertEqual(summary["samples"], 2)

    def test_cross_process_origins_are_correlated(self):
        ledger = self.ledger()
        ledger.finish("K00000Z")
        metrics = self.metrics(errors=[{"at_s": 2.5, "stream": 7, "connection": 0, "kind": "not_found"}])
        metrics["started_unix_s"] = 999
        metrics["stream_attachments"][0]["at_s"] = 1
        metrics["stream_attachments"][1]["at_s"] = 1
        summary = self.summary(ledger, metrics)
        self.assertEqual(summary["lost"], ["K00000Z"])
        self.assertEqual(summary["ledger"][0]["non_delivery"][0]["at_s"], 1.5)

    def test_ledger_is_bounded_and_eviction_is_explicit(self):
        ledger = probe.KeyLedger(1000, capacity=2)
        for index in range(3):
            token = f"K{index:05d}Z"
            ledger.sent(token, index)
            ledger.finish(token)
        summary = self.summary(ledger)
        self.assertEqual(summary["ledger_dropped"], 1)
        self.assertEqual([r["token"] for r in summary["ledger"]], ["K00001Z", "K00002Z"])
        self.assertEqual(len(ledger.by_token), 2)

    def test_partial_ledger_on_exception_still_marks_elapsed_deadline(self):
        ledger = self.ledger()  # No finish call: the probe can fail while waiting.
        summary = self.summary(ledger)
        self.assertEqual(summary["missed_deadline"], ["K00000Z"])
        self.assertEqual(summary["lost"], [])
        ledger.observe("K00000Z", 3.5)
        self.assertEqual(self.summary(ledger)["missed_deadline"], ["K00000Z"])

    def test_incomplete_stream_evidence_is_explicit(self):
        ledger = self.ledger()
        ledger.finish("K00000Z")
        self.assertFalse(self.summary(ledger, {})["stream_evidence_complete"])
        metrics = self.metrics()
        metrics["stream_events_dropped"] = 1
        self.assertFalse(self.summary(ledger, metrics)["stream_evidence_complete"])


if __name__ == "__main__":
    unittest.main()
