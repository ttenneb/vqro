"""Inherited component proofs must remain bounded, image-bound and falsifiable."""
import hashlib
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest

from agent_conformance.evidence import artifact, connection_scale, host_generation_fence, producer_overflow
from agent_conformance.results import CheckFailed, HarnessGap


class ComponentEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="k1-proof-")
        self.root = Path(self.temp.name)
        self.ctx = SimpleNamespace(manifest={}, metadata={"adapter_image": {"sha256": "image"}, "core_image_sha256": "core-image"}, evidence=[])

    def tearDown(self):
        self.temp.cleanup()

    def put(self, name, value):
        data = value.encode() if isinstance(value, str) else json.dumps(value).encode()
        path = self.root / name
        path.write_bytes(data)
        return {"path": str(path), "sha256": hashlib.sha256(data).hexdigest()}

    def bundle(self, ident):
        row = {"component": "shared-component", "adapter_image_sha256": "image", "producer_source": "pinned-source",
               "receipt": self.put("receipt", {"kind": "vqro.check-receipt", "mode": "full", "result": "pass", "commit": "source",
                                                "steps": [{"name": "cargo-test", "result": "PASS"}, {"name": "e2e-item-streams", "result": "PASS"}]}),
               "core_image_sha256": "core-image",
               "required_tests": {"snapshot_hold": "snapshot_hold", "overflow_before_next_piece": "unfinished_cut_overflow", "retention_reset_recovery": "reset_recovery"},
               "unit_log": self.put("unit-log", "test snapshot_hold ... ok\ntest unfinished_cut_overflow ... ok\ntest reset_recovery ... ok\n")}
        self.ctx.manifest = {"component_evidence": {ident: row}}
        return row

    def test_missing_component_evidence_is_harness_failure(self):
        with self.assertRaises(HarnessGap):
            producer_overflow(self.ctx)

    def test_linkage_to_another_adapter_image_is_not_inherited(self):
        self.bundle("O3")["adapter_image_sha256"] = "other-image"
        with self.assertRaises(HarnessGap):
            producer_overflow(self.ctx)

    def test_mutated_receipt_is_not_inherited(self):
        row = self.bundle("O3")
        Path(row["receipt"]["path"]).write_text("{}")
        with self.assertRaises(HarnessGap):
            producer_overflow(self.ctx)

    def test_known_counterexample_prevents_a_passing_old_gate_from_certifying(self):
        self.bundle("O3")["known_counterexamples"] = ["unfinished snapshot emits after overflow"]
        with self.assertRaises(CheckFailed) as result:
            producer_overflow(self.ctx)
        self.assertEqual(result.exception.component, "producer")

    def test_required_regression_cannot_be_missing(self):
        row = self.bundle("O3")
        row["required_tests"]["overflow_before_next_piece"] = "unrun_regression"
        with self.assertRaises(CheckFailed):
            producer_overflow(self.ctx)

    def test_passing_producer_proof_is_explicitly_inherited_not_another_runtime(self):
        self.bundle("O3")
        producer_overflow(self.ctx)
        self.assertTrue(self.ctx.evidence[-1]["not_a_separate_runtime"])
        self.assertEqual(self.ctx.evidence[-1]["kind"], "inherited_component")

    def test_host_fence_needs_both_stale_source_and_queued_effect_proofs(self):
        row = self.bundle("L3")
        row["required_tests"] = {"stale_source_ignored": "stale_source", "old_queued_effect_rejected": "old_effect"}
        row["unit_log"] = self.put("host-log", "test stale_source ... ok\ntest old_effect ... ok\n")
        host_generation_fence(self.ctx)
        del row["required_tests"]["old_queued_effect_rejected"]
        with self.assertRaises(HarnessGap):
            host_generation_fence(self.ctx)

    def test_200_stream_scale_proof_requires_identical_reconstruction_and_retention_gaps(self):
        row = self.bundle("O11")
        row["required_tests"] = {"reserved_budget": "reserved", "opens_outside_effect_limit": "reserved_opens", "writing_stream_closes": "writer_closed", "idle_sibling_stays_open": "idle_open"}
        row["unit_log"] = self.put("transport-log", "test reserved ... ok\ntest reserved_opens ... ok\ntest writer_closed ... ok\ntest idle_open ... ok\n")
        counters = {key: 0 for key in ("bad_json", "bad_record", "bad_utf8", "duplicate", "missing", "stale_base", "unknown_entity")}
        counters["gaps_retention"] = 1
        summary = {name: {"agents": 200, "identical_to_runtime": 200, "counters": counters} for name in ("v1", "v2", "v3")}
        summary["stalled_attached"] = 200
        row["summary"] = self.put("summary", summary)
        connection_scale(self.ctx)
        summary["v1"]["identical_to_runtime"] = 199
        row["summary"] = self.put("summary", summary)
        with self.assertRaises(CheckFailed):
            connection_scale(self.ctx)
