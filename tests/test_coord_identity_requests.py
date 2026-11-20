# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Adversarial predicate regressions; these are not public-core qualification results."""
import hashlib
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import AsyncMock, patch
import agent_conformance.part_b.identity_request_cases as cases
from agent_conformance.results import CheckFailed, FixtureFailed, HarnessGap
from agent_conformance.part_b.identity_backend import IDENTITY_KEYS, child_identity_is_scrubbed, empty_terminal_inventory, pinned_artifact
from agent_conformance.part_b.identity_request_cases import (
    PROBE_NAME, callback_attribution_is_actual, managed_principal_is_bound_not_inherited,
)


class IdentityWitnessTests(unittest.TestCase):
    def setUp(self):
        self.actor = "extension:" + PROBE_NAME
        self.hello = {"actor": self.actor, "you": "ext:" + PROBE_NAME, "actual_author": "client:epoch:7"}

    def test_backend_child_requires_complete_typed_observation(self):
        valid = {"pid": 7, "principal": None, "present": {key: False for key in IDENTITY_KEYS}}
        self.assertTrue(child_identity_is_scrubbed(valid))
        self.assertFalse(child_identity_is_scrubbed({**valid, "principal": "owner"}))
        explicit = {**valid, "principal": "worker:explicit", "present": {**valid["present"], "VQRO_PRINCIPAL": True}}
        self.assertTrue(child_identity_is_scrubbed(explicit, "worker:explicit"))
        self.assertFalse(child_identity_is_scrubbed(explicit, "owner"))
        self.assertFalse(child_identity_is_scrubbed({**valid, "present": {**valid["present"], "VQRO_PRINCIPAL": True}}))
        for invalid in ({}, {**valid, "pid": True}, {**valid, "present": {}},
                        {**valid, "present": {key: None for key in IDENTITY_KEYS}}):
            with self.assertRaises(FixtureFailed): child_identity_is_scrubbed(invalid)

    def test_missing_inventory_cannot_prove_terminal_removal(self):
        self.assertTrue(empty_terminal_inventory({"terminals": []}))
        self.assertFalse(empty_terminal_inventory({"terminals": [{"address": "t1"}]}))
        for invalid in ({}, {"terminals": None}):
            with self.assertRaises(FixtureFailed): empty_terminal_inventory(invalid)

    def test_backend_pin_is_checked_before_any_process_can_start(self):
        with self.assertRaises(FixtureFailed): pinned_artifact(None)
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "owned-binary"
            path.write_bytes(b"owned fixture bytes")
            good = {"path": str(path), "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}
            self.assertEqual(pinned_artifact(good), path)
            with self.assertRaises(FixtureFailed): pinned_artifact({**good, "sha256": "0" * 64})

    def test_own_core_assigned_principal_is_not_caller_inheritance(self):
        for principal in (None, self.actor):
            self.assertTrue(managed_principal_is_bound_not_inherited({"hello": self.hello, "launch_principal": principal}))

    def test_caller_or_unrelated_principal_cannot_pass_as_managed_identity(self):
        for principal in ("owner", "worker:must-not-leak", "extension:other"):
            self.assertFalse(managed_principal_is_bound_not_inherited({"hello": self.hello, "launch_principal": principal}))
        self.assertFalse(managed_principal_is_bound_not_inherited({"hello": {"actor": "owner"}, "launch_principal": None}))

    def test_combined_callback_requires_distinct_actual_actor_and_initiator(self):
        row = {"actor": self.actor, "author": self.hello["actual_author"], "principal": "owner", "initiator": "worker:W"}
        self.assertTrue(callback_attribution_is_actual(row, self.hello, "owner", "worker:W"))
        for field, wrong in (("actor", "owner"), ("author", self.hello["you"]), ("author", "client:caller"), ("principal", self.actor), ("initiator", "owner")):
            self.assertFalse(callback_attribution_is_actual({**row, field: wrong}, self.hello, "owner", "worker:W"), field)

    def test_matching_principal_alone_cannot_witness_callback_propagation(self):
        self.assertFalse(callback_attribution_is_actual({"principal": "owner"}, self.hello, "owner", "worker:W"))
        self.assertFalse(callback_attribution_is_actual({"actor": self.actor, "principal": "owner", "initiator": "worker:W"},
            {"actor": self.actor}, "owner", "worker:W"))
        self.assertFalse(callback_attribution_is_actual({"actor": self.actor, "author": self.hello["actual_author"]}, self.hello, None, None))

    def test_mail_requires_every_exact_request_identity_field(self):
        question = {"request": "q", "revision": 2}
        mail = {"agent": "r1", "epoch": "e1", "request": "q", "revision": 2}
        self.assertTrue(cases.mail_matches_request(mail, "r1", question))
        for field, wrong in (("agent", "r2"), ("epoch", "e2"), ("request", "other"), ("revision", 1)):
            self.assertFalse(cases.mail_matches_request({**mail, field: wrong}, "r1", question))
        for revision in (True, 1.0, "1", 0, -1):
            self.assertFalse(cases.mail_matches_request({**mail, "revision": revision}, "r1", {"request": "q", "revision": 1}))


class RoutingAndResponseIntegrityTests(unittest.IsolatedAsyncioTestCase):
    async def test_unknown_target_never_inherits_known_unimplemented_verdict(self):
        ctx = SimpleNamespace(manifest={"coord_revision": "future-target"})
        with self.assertRaises(HarnessGap): await cases.routing_binding(ctx, "configure")

    async def test_pinned_missing_routing_is_required_fail_not_unsupported(self):
        ctx = SimpleNamespace(manifest={"coord_revision": "fe8e3d02c31fb78abfdc8d1616f2067e963a58e4"},
            peer=SimpleNamespace(call=AsyncMock(return_value={"commands": [{"name": "coord.control"}]})),
            resources=AsyncMock(return_value=[{"id": "r1"}]), evidence=[])
        with self.assertRaisesRegex(CheckFailed, "routing not implemented in target") as failure:
            await cases.routing_binding(ctx, "launch")
        self.assertEqual(failure.exception.component, "coordination")
        self.assertFalse(ctx.evidence[0]["semantic_probes_executed"])
        ctx.peer.call.assert_awaited_once_with("command.list")

    def context(self, counts):
        records = iter(counts)
        def check(ok, message, **_):
            if not ok: raise CheckFailed(message)
        return SimpleNamespace(check=check, native=SimpleNamespace(records=lambda *_, **__: list(range(next(records)))))

    async def test_zero_responses_cannot_make_unknown_outcome_a_definite_rejection(self):
        with patch.object(cases, "respond", AsyncMock(return_value={"state": "unknown"})):
            with self.assertRaises(CheckFailed):
                await cases.rejected_answer(self.context([0]), {"request": "q"}, reason="stale")

    async def test_rejected_receipt_cannot_hide_an_actual_native_response(self):
        with patch.object(cases, "respond", AsyncMock(return_value={"state": "rejected"})):
            with self.assertRaises(CheckFailed):
                await cases.rejected_answer(self.context([0, 1, 1]), {"request": "q"}, reason="stale")


if __name__ == "__main__":
    unittest.main()
