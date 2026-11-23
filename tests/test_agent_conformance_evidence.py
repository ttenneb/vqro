"""Shared proof integrity tests; synthetic Git sources are not implementations."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from agent_conformance.evidence import CLOSURES, REGRESSIONS, component, connection_scale, host_generation_fence, producer_overflow, regressions, retained_memory_accounting
from agent_conformance.results import CheckFailed, HarnessGap

NAMES = {ident: {key: name for key, (name, _) in rows.items()} for ident, rows in REGRESSIONS.items()}


class ComponentEvidenceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="k1-proof-")
        self.root = Path(self.temp.name)
        self.repo = self.root / "source"
        self.repo.mkdir()
        definitions = {}
        for rows in REGRESSIONS.values():
            for name, path in rows.values():
                definitions.setdefault(path, set()).add(name.split("::")[-1])
        for roles in CLOSURES.values():
            for paths in roles.values():
                for path in paths:
                    definitions.setdefault(path, set())
        self.source_text = {}
        for path, leaves in definitions.items():
            text = "".join("#[test]\nfn " + leaf + "() {}\n" for leaf in sorted(leaves))
            self.source_text[path] = text or "fn runtime_component() {}\n"
            target = self.repo / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(self.source_text[path])
        self.run_git("init", "-q", "-b", "main")
        self.run_git("add", ".")
        self.revision = self.commit()
        trusted = patch("agent_conformance.evidence.HARNESS_REPO", self.repo)
        trusted.start()
        self.addCleanup(trusted.stop)
        self.ctx = SimpleNamespace(manifest={}, metadata={"adapter_image": {"sha256": "a" * 64}, "core_image_sha256": "b" * 64}, evidence=[])

    def tearDown(self):
        self.temp.cleanup()

    def run_git(self, *args):
        return subprocess.run(["git", "-C", str(self.repo), *args], env={"PATH": os.environ.get("PATH", os.defpath) if sys.platform == "win32" else os.defpath,
            "HOME": str(self.root), "GIT_CONFIG_NOSYSTEM": "1"}, capture_output=True, check=True).stdout

    def commit(self):
        self.run_git("-c", "user.name=Harness Test", "-c", "user.email=harness@example.invalid", "commit", "-qm", "synthetic source")
        return self.run_git("rev-parse", "HEAD").decode().strip()

    def put(self, name, value):
        data = value.encode() if isinstance(value, str) else json.dumps(value).encode()
        path = self.root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        return {"path": str(path), "sha256": hashlib.sha256(data).hexdigest()}

    def bundle(self, ident):
        tests = dict(NAMES[ident])
        binaries = {}
        for rows in REGRESSIONS.values():
            for name, path in rows.values():
                binaries.setdefault(path if path.startswith("tests/") else "src/lib.rs", set()).add(name)
        text = ""
        for binary, names in binaries.items():
            text += "Running " + ("unittests " if binary == "src/lib.rs" else "") + binary + " (/synthetic/test-bin)\n"
            text += "".join("test " + name + " ... ok\n" for name in sorted(names))
            text += "test result: ok. 9 passed; 0 failed; 0 ignored;\n"
        unit = self.put("gate/cargo-test.log", text)
        row = {"component": "synthetic-shared-component", "adapter_image_sha256": "a" * 64,
               "core_image_sha256": "b" * 64, "producer_source": self.revision, "source_repo": str(self.repo),
               "source_files": {role: sorted(paths) for role, paths in CLOSURES[ident].items()},
               "build_manifest": self.put("build.json", {"kind": "vqro.component-build-v1",
                   "adapter": {"sha256": "a" * 64, "source_commit": self.revision},
                   "core": {"sha256": "b" * 64, "source_commit": self.revision}}),
               "required_tests": tests, "test_sources": {name: path for name, path in REGRESSIONS[ident].values()}, "unit_log": unit}
        counters = {key: 0 for key in ("bad_json", "bad_record", "bad_utf8", "duplicate", "missing", "stale_base", "unknown_entity")}
        counters["gaps_retention"] = 1
        summary = {name: {"agents": 200, "identical_to_runtime": 200, "counters": counters} for name in ("v1", "v2", "v3")}
        summary["stalled_attached"] = 200
        row["summary"] = self.put("gate/scale/summary.json", summary)
        row["scale_log"] = self.put("gate/e2e-item-streams.log", json.dumps({"result": "pass", "failures": [], "out": str(self.root / "gate/scale")}) + "\n")
        row["receipt"] = self.put("receipt.json", {"kind": "vqro.check-receipt", "mode": "full", "result": "pass",
            "commit": self.revision, "failed": [], "finished_unix": 1234,
            "steps": [{"name": "cargo-test", "result": "PASS", "log": unit["path"]},
                      {"name": "e2e-item-streams", "result": "PASS", "log": row["scale_log"]["path"]}]})
        self.ctx.manifest = {"component_evidence": {ident: row}}
        return row

    def replace_receipt(self, row, **changes):
        data = json.loads(Path(row["receipt"]["path"]).read_text())
        data.update(changes)
        row["receipt"] = self.put("receipt.json", data)

    def test_o12_atomic_append_refusal_requires_exact_library_witness(self):
        self.bundle("O12")
        row, receipt, repo, producer = component(self.ctx, "O12")
        regressions(row, receipt, repo, producer, "O12")
        self.assertEqual(set(row["required_tests"]), {"append_refusal_atomic"})
        self.assertEqual(row["source_files"], {"adapter": ["src/provider_client/items.rs"]})

    def test_o12_other_passing_library_test_cannot_replace_atomic_refusal(self):
        row = self.bundle("O12")
        row["required_tests"]["append_refusal_atomic"] = NAMES["O10"]["journal_envelope_and_floor"]
        with self.assertRaises(HarnessGap):
            row, receipt, repo, producer = component(self.ctx, "O12")
            regressions(row, receipt, repo, producer, "O12")

    def test_missing_component_evidence_is_harness_failure(self):
        with self.assertRaises(HarnessGap): producer_overflow(self.ctx)

    def test_linkage_to_another_adapter_image_is_not_inherited(self):
        self.bundle("O3")["adapter_image_sha256"] = "c" * 64
        with self.assertRaises(HarnessGap): producer_overflow(self.ctx)

    def test_mutated_receipt_is_not_inherited(self):
        row = self.bundle("O3")
        Path(row["receipt"]["path"]).write_text("{}")
        with self.assertRaises(HarnessGap): producer_overflow(self.ctx)

    def test_known_counterexample_overrides_an_old_green_gate(self):
        self.bundle("O3")["known_counterexamples"] = ["unfinished snapshot emits after overflow"]
        with self.assertRaises(CheckFailed) as result: producer_overflow(self.ctx)
        self.assertEqual(result.exception.component, "producer")

    def test_empty_names_missing_commit_and_not_a_commit_are_refused_for_all_inheritance(self):
        for ident, verifier in (("O3", producer_overflow), ("L3", host_generation_fence), ("O11", connection_scale), ("O10", retained_memory_accounting)):
            for corruption in ("empty", "commit", "source"):
                with self.subTest(case=ident, corruption=corruption):
                    row = self.bundle(ident)
                    if corruption == "empty": row["required_tests"] = {key: "" for key in row["required_tests"]}
                    elif corruption == "commit": self.replace_receipt(row, commit=None)
                    else: row["producer_source"] = "not-a-commit"
                    with self.assertRaises(HarnessGap): verifier(self.ctx)

    def test_exact_regression_cannot_be_replaced_with_an_unrelated_passing_line(self):
        row = self.bundle("O3")
        row["unit_log"] = self.put("gate/cargo-test.log", "test provider::tests::unrelated ... ok\n")
        with self.assertRaises(CheckFailed): producer_overflow(self.ctx)

    def test_substring_does_not_count_as_exact_passing_test(self):
        row = self.bundle("O3")
        row["unit_log"] = self.put("gate/cargo-test.log", "".join("test " + name + "_not_the_test ... ok\n" for name in row["required_tests"].values()))
        with self.assertRaises(CheckFailed): producer_overflow(self.ctx)

    def test_declared_name_must_exist_as_test_at_pinned_source(self):
        row = self.bundle("O3")
        name = "provider::tests::not_in_source"
        row["required_tests"]["snapshot_hold"] = name
        row["test_sources"][name] = "src/provider.rs"
        row["unit_log"] = self.put("gate/cargo-test.log", "test " + name + " ... ok\n")
        with self.assertRaises(HarnessGap): producer_overflow(self.ctx)

    def test_existing_unrelated_passing_regression_cannot_replace_semantic_witness(self):
        row = self.bundle("O3")
        row["required_tests"]["snapshot_hold"] = row["required_tests"]["retention_reset_recovery"]
        with self.assertRaises(HarnessGap): producer_overflow(self.ctx)

    def test_known_test_must_exist_at_exact_pinned_commit(self):
        row = self.bundle("O3")
        name, path = REGRESSIONS["O3"]["snapshot_hold"]
        text = self.source_text[path].replace("#[test]\nfn " + name.split("::")[-1] + "() {}\n", "")
        (self.repo / path).write_text(text)
        self.run_git("add", ".")
        revision = self.commit()
        row["producer_source"] = revision
        self.replace_receipt(row, commit=revision)
        build = json.loads(Path(row["build_manifest"]["path"]).read_text())
        build["adapter"]["source_commit"] = revision
        row["build_manifest"] = self.put("build.json", build)
        with self.assertRaisesRegex(HarnessGap, "not a test at the pinned commit"):
            producer_overflow(self.ctx)

    def test_exact_name_in_wrong_cargo_binary_is_not_a_witness(self):
        row = self.bundle("O3")
        text = Path(row["unit_log"]["path"]).read_text().replace("Running unittests src/lib.rs", "Running tests/unrelated.rs")
        row["unit_log"] = self.put("gate/cargo-test.log", text)
        with self.assertRaises(CheckFailed): producer_overflow(self.ctx)

    def test_snippet_from_another_run_is_not_the_receipts_log(self):
        row = self.bundle("O3")
        row["unit_log"] = self.put("detached-log", "test provider::tests::snapshot_hold ... ok\n")
        with self.assertRaises(HarnessGap): producer_overflow(self.ctx)

    def test_gate_and_producer_source_must_match(self):
        row = self.bundle("O3")
        self.replace_receipt(row, commit="f" * 40)
        with self.assertRaises(HarnessGap): producer_overflow(self.ctx)

    def test_executed_build_component_must_equal_tested_source(self):
        row = self.bundle("O3")
        path = "src/provider_client/items.rs"
        (self.repo / path).write_text(self.source_text[path] + "fn changed_runtime() {}\n")
        self.run_git("add", ".")
        other = self.commit()
        build = json.loads(Path(row["build_manifest"]["path"]).read_text())
        build["adapter"]["source_commit"] = other
        row["build_manifest"] = self.put("build.json", build)
        with self.assertRaises(HarnessGap): producer_overflow(self.ctx)

    def test_green_receipt_cannot_hide_failed_unit_log(self):
        row = self.bundle("O3")
        text = Path(row["unit_log"]["path"]).read_text() + "test result: FAILED. 8 passed; 1 failed;\n"
        row["unit_log"] = self.put("gate/cargo-test.log", text)
        with self.assertRaises(HarnessGap): producer_overflow(self.ctx)

    def test_passing_producer_proof_is_explicitly_inherited(self):
        self.bundle("O3")
        producer_overflow(self.ctx)
        self.assertTrue(self.ctx.evidence[-1]["not_a_separate_runtime"])
        self.assertIn("asserted", self.ctx.evidence[-1]["build_source_linkage"])

    def test_callers_main_cannot_authorize_a_producer_outside_trusted_main(self):
        trusted = self.repo
        caller = self.root / "caller-repository"
        self.run_git("clone", "-q", str(trusted), str(caller))
        self.repo = caller
        (caller / "untrusted.txt").write_text("caller-only history\n")
        self.run_git("add", ".")
        self.revision = self.commit()
        # Everything else is a valid, internally consistent proof, including
        # its producer being the tip of the caller's own branch named main.
        for ident, verify in (("O3", producer_overflow), ("O10", retained_memory_accounting),
                              ("L3", host_generation_fence), ("O11", connection_scale)):
            with self.subTest(case=ident):
                self.bundle(ident)
                with self.assertRaisesRegex(HarnessGap, "not reachable from trusted harness main"):
                    verify(self.ctx)

    def test_existing_but_unmerged_producer_is_not_authorized(self):
        self.run_git("checkout", "-qb", "unmerged-producer")
        (self.repo / "unmerged.txt").write_text("not part of main\n")
        self.run_git("add", ".")
        self.revision = self.commit()
        self.bundle("O3")
        with self.assertRaisesRegex(HarnessGap, "not reachable from trusted harness main"):
            producer_overflow(self.ctx)

    def test_producer_may_be_an_ancestor_instead_of_the_main_tip(self):
        self.bundle("O3")
        (self.repo / "later.txt").write_text("later main commit\n")
        self.run_git("add", ".")
        main = self.commit()
        producer_overflow(self.ctx)
        ancestry = next(row for row in self.ctx.evidence if row["kind"] == "trusted_producer_main")
        self.assertEqual((ancestry["main"], ancestry["producer_source"]), (main, self.revision))

    def test_source_checkout_override_does_not_replace_the_trust_anchor(self):
        trusted = self.repo
        caller = self.root / "source-checkout"
        self.run_git("clone", "-q", str(trusted), str(caller))
        row = self.bundle("O3")
        row["source_repo"] = str(caller)
        producer_overflow(self.ctx)
        ancestry = next(row for row in self.ctx.evidence if row["kind"] == "trusted_producer_main")
        self.assertEqual(ancestry["repo"], str(trusted))

    def test_replace_refs_cannot_hide_a_changed_image_component(self):
        caller = self.root / "source-checkout"
        self.run_git("clone", "-q", str(self.repo), str(caller))
        row = self.bundle("O3")
        row["source_repo"] = str(caller)
        self.repo = caller
        path = "src/provider_client/items.rs"
        (caller / path).write_text(self.source_text[path] + "fn changed_runtime() {}\n")
        self.run_git("add", ".")
        changed = self.commit()
        self.run_git("replace", changed, self.revision)
        build = json.loads(Path(row["build_manifest"]["path"]).read_text())
        build["adapter"]["source_commit"] = changed
        row["build_manifest"] = self.put("build.json", build)
        with self.assertRaisesRegex(HarnessGap, "component differs from tested source"):
            producer_overflow(self.ctx)

    def test_o10_accounting_requires_both_internal_witnesses_and_reports_scope(self):
        row = self.bundle("O10")
        retained_memory_accounting(self.ctx)
        self.assertEqual(self.ctx.evidence[-1]["release"], ["close", "retention_gap", "next_step_after_epoch"])
        self.assertEqual(self.ctx.evidence[-2]["case"], "O10")
        del row["required_tests"]["cut_versions_and_release"]
        with self.assertRaises(HarnessGap): retained_memory_accounting(self.ctx)

    def test_host_fence_requires_both_exact_regressions(self):
        row = self.bundle("L3")
        host_generation_fence(self.ctx)
        del row["required_tests"]["old_queued_effect_rejected"]
        with self.assertRaises(HarnessGap): host_generation_fence(self.ctx)

    def test_scale_summary_must_be_from_recorded_gate_not_synthetic_side_file(self):
        row = self.bundle("O11")
        connection_scale(self.ctx)
        row["summary"] = self.put("unrelated-summary", {"stalled_attached": 200})
        with self.assertRaises(HarnessGap): connection_scale(self.ctx)

    def test_scale_integrity_failure_is_not_waived_by_a_green_receipt(self):
        row = self.bundle("O11")
        summary = json.loads(Path(row["summary"]["path"]).read_text())
        summary["v1"]["identical_to_runtime"] = 199
        row["summary"] = self.put("gate/scale/summary.json", summary)
        with self.assertRaises(CheckFailed): connection_scale(self.ctx)
