"""The scale probe enforces unchanged budgets and does not infer delivery from stream history."""
import copy
import sys
import unittest
from unittest import mock
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import e2e_tui_scale as probe  # noqa: E402


class Budgets(unittest.TestCase):
    within = {"cpu_pct_one_core": 18, "rss_peak_mib": 38.6, "key_p99_ms": 81.8,
              "background_fps_peak_complete_second": 55, "fps_peak_complete_second": 94}

    def valid_receipt(self, **changes):
        key_changes = changes.pop('keys', {})
        latency = key_changes.get('p99_ms', 81.8)
        latency = latency if probe.finite_measure(latency) else 81.8
        metrics = dict(started_unix_s=1000, stream_events_dropped=0, stream_events_cap=probe.STREAM_EVENT_CAP,
                       stream_attachments=[], stream_errors=[], stream_closures=[])
        ledger = probe.KeyLedger(1000)
        for index in range(20):
            token = f'K{index:05d}Z'
            ledger.sent(token, index * 4)
            ledger.observe(token, index * 4 + latency / 1000)
            ledger.finish(token)
        keys = ledger.summary(metrics, 'idle', 'flood', 80 + latency / 1000)
        keys.update(key_changes)
        receipt = dict(result='pass', correctness_ok=True, workload_valid=True, leftover_processes=[],
            tui=dict(self.within), keys=keys, metrics=metrics, events=dict(missed=0),
            extensions=dict(sidebar=dict(status='running', crashes=0)), budgets_exceeded=[],
            cleanup=dict(ok=True, shutdown_requested=True, shutdown_error=None, client_close_error=None,
                         core_exit=0, forced_kill=False, wait_error=None))
        receipt.update(changes)
        return receipt

    def test_measured_results_pass_with_margin(self):
        self.assertEqual(probe.exceeded(self.within), [])
        for name, (limit, worst) in probe.BUDGETS.items():
            self.assertGreaterEqual(limit, worst, name)

    def test_any_measure_over_its_limit_fails(self):
        for name, (limit, _) in probe.BUDGETS.items():
            self.assertEqual(probe.exceeded({**self.within, name: limit + 1}), [name])

    def test_only_pure_key_latency_breach_has_budget_exit(self):
        receipt = self.valid_receipt(result='fail', keys=dict(samples=20, lost=[], p99_ms=171),
                                     budgets_exceeded=['key_p99_ms'])
        self.assertEqual(probe.timing_exit(receipt), 78)
        for change in [dict(correctness_ok=False), dict(leftover_processes=[123]), dict(error='bad workload'),
                       dict(tui={**self.within, 'rss_peak_mib': 81}), dict(keys={'p99_ms': None}),
                       dict(keys={'p99_ms': float('nan')})]:
            with self.subTest(change=change):
                self.assertEqual(probe.timing_exit({**receipt, **change}), 1)
        self.assertEqual(probe.timing_exit(dict(result='pass')), 1)

    def test_pass_tag_or_claimed_breach_list_cannot_bypass_required_evidence(self):
        for receipt in [dict(result='pass'), self.valid_receipt(correctness_ok=False),
                        self.valid_receipt(cleanup={}), self.valid_receipt(workload_valid=False),
                        self.valid_receipt(tui={**self.within, 'rss_peak_mib': float('nan')}),
                        self.valid_receipt(keys=dict(samples=20, lost=[], p99_ms=float('nan'))),
                        self.valid_receipt(cleanup={**self.valid_receipt()['cleanup'], 'core_exit': 7}),
                        self.valid_receipt(cleanup={**self.valid_receipt()['cleanup'], 'forced_kill': True}),
                        self.valid_receipt(tui={**self.within, 'cpu_pct_one_core': 41}),
                        self.valid_receipt(keys=dict(samples=0, lost=[], p99_ms=1))]:
            with self.subTest(receipt=receipt):
                self.assertEqual(probe.timing_exit(receipt), 1)
        # The actual values, not result/breach tags, decide finite latency failures.
        receipt = self.valid_receipt(keys=dict(samples=20, lost=[], p99_ms=171), budgets_exceeded=[])
        self.assertEqual(probe.timing_exit(receipt), 78)
        receipt['result'] = 'fail'
        self.assertEqual(probe.timing_exit(receipt), 78)
        self.assertIn('key_p99_ms', probe.exceeded({**self.within, 'key_p99_ms': float('nan')}))
        self.assertEqual(probe.timing_exit(self.valid_receipt(budgets_exceeded=['rss_peak_mib'])), 0)

    def test_teardown_failures_are_hard_before_pass_or_latency_breach(self):
        for kind in ('shutdown-error', 'nonzero-exit', 'forced-kill'):
            for result in ('pass', 'fail'):
                with self.subTest(kind=kind, result=result):
                    receipt = self.valid_receipt(result=result,
                        keys=dict(samples=20, lost=[], p99_ms=171 if result == 'fail' else 81.8))
                    client = mock.Mock()
                    core = mock.Mock()
                    core.wait.return_value = 0
                    if kind == 'shutdown-error': client.call.side_effect = RuntimeError('lost shutdown reply')
                    if kind == 'nonzero-exit': core.wait.return_value = 7
                    if kind == 'forced-kill':
                        core.wait.side_effect = [probe.subprocess.TimeoutExpired('private core', 5), -9]
                    probe.cleanup_core(receipt, client, core)
                    self.assertEqual(receipt['result'], 'fail', receipt)
                    self.assertFalse(receipt['correctness_ok'], receipt)
                    self.assertFalse(receipt['cleanup']['ok'], receipt)
                    self.assertEqual(probe.timing_exit(receipt), 1)

    def test_clean_shutdown_and_exit_are_observed_before_pass(self):
        receipt = self.valid_receipt()
        client, core = mock.Mock(), mock.Mock()
        core.wait.return_value = 0
        probe.cleanup_core(receipt, client, core)
        self.assertTrue(receipt['cleanup']['ok'], receipt)
        self.assertEqual(probe.timing_exit(receipt), 0)
        client.call.assert_called_once_with('core.shutdown')
        client.close.assert_called_once()
        core.kill.assert_not_called()

    def test_a_missing_measurement_is_not_a_pass(self):
        for name in probe.BUDGETS:
            self.assertIn(name, probe.exceeded({k: v for k, v in self.within.items() if k != name}))


class DeliveryEvidence(unittest.TestCase):
    def metrics(self, errors=(), closures=(), attaches=()):
        return {"started_unix_s": 1000, "stream_events_dropped": 0, "stream_events_cap": probe.STREAM_EVENT_CAP,
                "stream_attachments": [
                    {"at_s": 0, "method": "resource.attach", "stream": 7, "connection": 0, "resource": "idle"},
                    {"at_s": 0, "method": "resource.attach", "stream": 9, "connection": 0, "resource": "flood"}, *attaches],
                "stream_errors": list(errors), "stream_closures": list(closures)}

    def error(self, at_s=1.5, stream=7, connection=0):
        return {"at_s": at_s, "method": "stream.error", "stream": stream, "connection": connection, "kind": "invalid"}

    def close(self, at_s=.5, stream=7, connection=0):
        return {"at_s": at_s, "method": "stream.closed", "stream": stream,
                "connection": connection, "reason": {"reason": "slow_consumer"}}

    def ledger(self):
        ledger = probe.KeyLedger(1000)
        ledger.sent("K00000Z", 1)
        ledger.finish("K00000Z")
        return ledger

    def summary(self, ledger, metrics=None):
        return ledger.summary(self.metrics() if metrics is None else metrics, "idle", "flood", 5)

    def assert_unknown(self, summary):
        self.assertNotIn("lost", summary)
        for row in summary["ledger"]:
            self.assertEqual(row["classification"], "unknown")
            self.assertNotIn("stream", row)  # Historical attach replies are not a forwarding witness.
            self.assertNotIn("non_delivery", row)
            for signal in row["associated_signals"]:
                self.assertEqual(signal["classification"], "unknown")

    def test_late_complete_precedes_associated_error_without_clearing_deadline_miss(self):
        ledger = self.ledger()
        ledger.observe("K00000Z", 3.5)
        summary = self.summary(ledger, self.metrics(errors=[self.error()]))
        self.assertEqual(summary["missed_deadline"], ["K00000Z"])
        self.assertNotIn("lost", summary)
        row = summary["ledger"][0]
        self.assertEqual((row["classification"], row["seen_s"]), ("observed", 3.5))
        self.assertEqual(row["associated_signals"][0]["classification"], "unknown")
        self.assertEqual(summary["p99_ms"], 2500)
        self.assertIn("key_p99_ms", probe.exceeded({**Budgets.within, "key_p99_ms": summary["p99_ms"]}))

    def test_stream_error_may_be_resize_close_or_backend_output_error_not_key_rejection(self):
        summary = self.summary(self.ledger(), self.metrics(errors=[self.error()]))
        self.assert_unknown(summary)
        self.assertEqual(summary["ledger"][0]["associated_signals"][0]["method"], "stream.error")

    def test_prior_attach_and_closure_do_not_witness_current_key_stream(self):
        attach = {"at_s": 1.2, "method": "resource.attach", "stream": 8, "connection": 1, "resource": "idle"}
        metrics = self.metrics(closures=[self.close()], attaches=[attach])
        summary = self.summary(self.ledger(), metrics)
        self.assert_unknown(summary)
        signals = summary["ledger"][0]["associated_signals"]
        self.assertEqual([s["method"] for s in signals], ["resource.attach"])
        self.assertEqual(signals[0]["stream"], 8)

    def test_ignored_or_early_closed_attach_reply_is_only_historical_signal(self):
        attach = {"at_s": 1.2, "method": "resource.attach", "stream": 8, "connection": 0, "resource": "idle"}
        summary = self.summary(self.ledger(), self.metrics(attaches=[attach], closures=[self.close(1.3, stream=8)]))
        self.assert_unknown(summary)
        self.assertEqual(len(summary["ledger"][0]["associated_signals"]), 2)

    def test_all_streams_in_window_are_associated_without_claiming_a_current_mapping(self):
        summary = self.summary(self.ledger(), self.metrics(errors=[self.error(stream=9)], closures=[self.close(2)]))
        self.assert_unknown(summary)
        self.assertEqual([s["stream"] for s in summary["ledger"][0]["associated_signals"]], [9, 7])

    def test_delayed_prior_error_in_later_key_window_remains_unknown(self):
        ledger = self.ledger()
        ledger.sent("K00001Z", 2)
        ledger.finish("K00001Z")
        summary = self.summary(ledger, self.metrics(errors=[self.error(2.5)]))
        self.assert_unknown(summary)
        self.assertEqual(summary["ledger"][0]["associated_signals"], [])
        self.assertEqual(len(summary["ledger"][1]["associated_signals"]), 1)

    def test_existing_capture_retains_a_late_prior_token(self):
        ledger = self.ledger()
        ledger.sent("K00001Z", 3.2)
        ledger.observe("K00000Z\nK00001Z", 3.3)
        ledger.finish("K00001Z")
        summary = self.summary(ledger)
        self.assertEqual(summary["missed_deadline"], ["K00000Z"])
        self.assertEqual([r["classification"] for r in summary["ledger"]], ["observed", "observed"])
        self.assertEqual(summary["samples"], 2)

    def test_cross_process_origins_only_correlate_signal_time(self):
        metrics = self.metrics(errors=[self.error(2.5)])
        metrics["started_unix_s"] = 999
        for attach in metrics["stream_attachments"]:
            attach["at_s"] = 1
        summary = self.summary(self.ledger(), metrics)
        self.assert_unknown(summary)
        self.assertEqual(summary["ledger"][0]["associated_signals"][0]["at_s"], 1.5)

    def test_ledger_is_bounded_and_eviction_is_explicit(self):
        ledger = probe.KeyLedger(1000, capacity=2)
        for index in range(3):
            token = f"K{index:05d}Z"
            ledger.sent(token, index)
            ledger.finish(token)
        summary = self.summary(ledger)
        self.assertEqual(summary["ledger_dropped"], 1)
        self.assertFalse(summary["stream_evidence_complete"])
        self.assertEqual(summary["evidence_status"], "evidence incomplete")
        self.assertEqual([r["token"] for r in summary["ledger"]], ["K00001Z", "K00002Z"])
        self.assertEqual(len(ledger.by_token), 2)

    def test_partial_ledger_on_exception_still_marks_elapsed_deadline(self):
        ledger = probe.KeyLedger(1000)
        ledger.sent("K00000Z", 1)  # No finish call: the probe failed while waiting.
        self.assertEqual(self.summary(ledger)["missed_deadline"], ["K00000Z"])
        ledger.observe("K00000Z", 3.5)
        self.assertEqual(self.summary(ledger)["missed_deadline"], ["K00000Z"])

    def test_each_missing_required_array_suppresses_all_association(self):
        for field in ("stream_attachments", "stream_errors", "stream_closures"):
            with self.subTest(field=field):
                metrics = self.metrics(errors=[self.error()], closures=[self.close()])
                del metrics[field]
                summary = self.summary(self.ledger(), metrics)
                self.assert_unknown(summary)
                self.assertFalse(summary["stream_evidence_complete"])
                self.assertEqual(summary["evidence_status"], "evidence incomplete")
                self.assertEqual(summary["ledger"][0]["associated_signals"], [])

    def test_dropped_metadata_cannot_infer_old_stream_loss_or_associate_signals(self):
        metrics = self.metrics(errors=[self.error()], closures=[self.close()])
        metrics["stream_events_dropped"] = 1  # A newer attachment may be missing.
        summary = self.summary(self.ledger(), metrics)
        self.assert_unknown(summary)
        self.assertEqual(summary["ledger"][0]["associated_signals"], [])
        self.assertEqual(summary["ledger"][0]["evidence_status"], "evidence incomplete")

    def test_capture_remains_positive_when_stream_metadata_is_incomplete(self):
        ledger = self.ledger()
        ledger.observe("K00000Z", 3.5)
        summary = self.summary(ledger, {})
        self.assertEqual(summary["ledger"][0]["classification"], "observed")
        self.assertEqual(summary["ledger"][0]["associated_signals"], [])
        self.assertEqual(summary["missed_deadline"], ["K00000Z"])
        self.assertFalse(summary["stream_evidence_complete"])

    def test_malformed_origins_arrays_ids_and_categories_are_incomplete(self):
        changes = [("started_unix_s", None), ("started_unix_s", True), ("started_unix_s", float("nan")),
                   ("started_unix_s", float("inf")), ("stream_events_dropped", True),
                   ("stream_events_cap", 999), ("stream_errors", {}), ("stream_closures", None)]
        for field, value in changes:
            with self.subTest(field=field, value=value):
                metrics = self.metrics(errors=[self.error()])
                metrics[field] = value
                self.assertFalse(probe.stream_evidence_complete(metrics))
                self.assertEqual(self.summary(self.ledger(), metrics)["ledger"][0]["associated_signals"], [])
        for field, value in [("stream", "7"), ("stream", True), ("stream", -1), ("stream", {}),
                             ("stream", 2**64), ("method", {}), ("at_s", float("nan")),
                             ("kind", "private text"), ("kind", "x" * 10000), ("private", "text")]:
            with self.subTest(field=field, value=value):
                metrics = self.metrics(errors=[self.error()])
                metrics["stream_errors"][0][field] = value
                self.assertFalse(probe.stream_evidence_complete(metrics))
        self.assertFalse(probe.stream_evidence_complete(None))

    def test_long_resources_and_uncategorical_reasons_are_incomplete(self):
        metrics = self.metrics(closures=[self.close(1.5)])
        for value in ["private text", "x" * 10000, {"reason": "closed", "message": "private"}]:
            with self.subTest(value=value):
                bad = copy.deepcopy(metrics)
                bad["stream_closures"][0]["reason"] = value
                self.assertFalse(probe.stream_evidence_complete(bad))
        metrics["stream_attachments"][0]["resource"] = "r" * 129
        self.assertFalse(probe.stream_evidence_complete(metrics))


class LedgerTimingGrading(unittest.TestCase):
    def receipt(self):
        return Budgets('test_measured_results_pass_with_margin').valid_receipt()

    def test_observed_late_echo_is_timing_not_loss_even_with_historical_error(self):
        receipt = self.receipt()
        receipt['metrics']['stream_errors'] = [dict(at_s=1.5, method='stream.error', stream=7,
                                                   connection=0, kind='invalid')]
        ledger = probe.KeyLedger(1000)
        ledger.sent('K00000Z', 1)
        ledger.finish('K00000Z')
        ledger.observe('K00000Z', 3.5)
        receipt['keys'] = ledger.summary(receipt['metrics'], 'idle', 'flood', 5)
        self.assertNotIn('lost', receipt['keys'])
        row = receipt['keys']['ledger'][0]
        self.assertEqual(row['classification'], 'observed')
        self.assertEqual(row['associated_signals'][0]['classification'], 'unknown')
        self.assertEqual(probe.timing_exit(receipt), 78)
        receipt['cleanup']['forced_kill'] = True
        self.assertEqual(probe.timing_exit(receipt), 1)

    def test_unknown_or_explicitly_lost_key_is_never_only_a_budget_breach(self):
        receipt = self.receipt()
        ledger = probe.KeyLedger(1000)
        ledger.sent('K00000Z', 1)
        ledger.finish('K00000Z')
        receipt['keys'] = ledger.summary(receipt['metrics'], 'idle', 'flood', 5)
        self.assertEqual(receipt['keys']['ledger'][0]['classification'], 'unknown')
        self.assertNotIn('lost', receipt['keys'])
        self.assertEqual(probe.timing_exit(receipt), 1)  # Missing evidence, not inferred loss.
        for classification in ('unknown', 'lost', 'rejected', 'not_sent'):
            receipt = self.receipt()
            receipt['keys']['p99_ms'] = 171
            receipt['keys']['ledger'][0]['classification'] = classification
            self.assertEqual(probe.timing_exit(receipt), 1, classification)
        receipt = self.receipt()
        receipt['keys']['lost'] = ['K00000Z']
        self.assertEqual(probe.timing_exit(receipt), 1)

    def test_incomplete_or_truncated_ledger_is_never_only_a_budget_breach(self):
        for field, value in [('ledger_dropped', 1), ('stream_evidence_complete', False),
                             ('evidence_status', 'evidence incomplete')]:
            receipt = self.receipt()
            receipt['keys']['p99_ms'] = 171
            receipt['keys'][field] = value
            self.assertEqual(probe.timing_exit(receipt), 1, field)

    def test_observed_deadline_miss_outside_p99_still_fails_the_existing_timing_guard(self):
        receipt = self.receipt()
        ledger = probe.KeyLedger(1000)
        for index in range(200):
            token = f'K{index:05d}Z'
            ledger.sent(token, index * 4)
            ledger.observe(token, index * 4 + (2.5 if index == 0 else .05))
            ledger.finish(token)
        receipt['keys'] = ledger.summary(receipt['metrics'], 'idle', 'flood', 805)
        self.assertLess(receipt['keys']['p99_ms'], probe.BUDGETS['key_p99_ms'][0])
        self.assertEqual(receipt['keys']['missed_deadline'], ['K00000Z'])
        self.assertEqual(probe.timing_exit(receipt), 78)


if __name__ == "__main__":
    unittest.main()
