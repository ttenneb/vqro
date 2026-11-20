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
from agent_conformance.wire import RpcError
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
        question = {"request": "q", "revision": 2, "scope": {"session": "s1"}}
        mail = {"agent": "r1", "epoch": "e7", "request": "q", "revision": 2, "scope": {"session": "s1"}}
        self.assertTrue(cases.mail_matches_request(mail, "r1", question, "e7"))
        for field, wrong in (("agent", "r2"), ("epoch", "e2"), ("request", "other"), ("revision", 1)):
            self.assertFalse(cases.mail_matches_request({**mail, field: wrong}, "r1", question, "e7"))
        for revision in (True, 1.0, "1", 0, -1):
            self.assertFalse(cases.mail_matches_request({**mail, "revision": revision}, "r1",
                {**question, "revision": 1}, "e7"))
        with self.assertRaises(TypeError): cases.mail_matches_request(mail, "r1", question)

    def test_mail_requires_exact_native_session_run_subject_or_operation_scope(self):
        scopes = [({"session": "s1"}, ({}, {"session": "other"}, {"session": "s1", "run": "invented"})),
            ({"session": "s1", "run": "r1"}, ({"session": "s1"}, {"run": "r1"},
                {"session": "other", "run": "r1"}, {"session": "s1", "run": "r2"})),
            ({"session": "s1", "run": "r1", "subject": "tool:a"},
                ({"session": "s1", "run": "r1"}, {"session": "s1", "run": "r1", "subject": "tool:b"})),
            ({"operation": "op1"}, ({}, {"operation": "op2"}, {"session": "s1"},
                {"operation": "op1", "session": "s1"}))]
        for scope, wrong_scopes in scopes:
            question = {"request": "q", "revision": 1, "scope": scope}
            mail = {"agent": "r1", "epoch": "e7", "request": "q", "revision": 1, "scope": scope}
            with self.subTest(scope=scope):
                self.assertTrue(cases.mail_matches_request(mail, "r1", question, "e7"))
                self.assertFalse(cases.mail_matches_request({k: v for k, v in mail.items() if k != "scope"}, "r1", question, "e7"))
                for wrong in wrong_scopes:
                    self.assertFalse(cases.mail_matches_request({**mail, "scope": wrong}, "r1", question, "e7"))

    def test_missing_or_malformed_scope_on_both_sides_is_not_positive_evidence(self):
        for scope in (None, {}, [], "s1", {"run": "r1"}, {"session": ""}, {"session": True},
                      {"session": "s1", "run": None}, {"session": "s1", "run": True},
                      {"operation": ""}, {"operation": True}, {"operation": "op", "session": "s1"}):
            with self.subTest(scope=scope):
                question = {"request": "q", "revision": 1, "scope": scope}
                mail = {"agent": "r1", "epoch": "e7", "request": "q", "revision": 1, "scope": scope}
                self.assertFalse(cases.mail_matches_request(mail, "r1", question, "e7"))
        self.assertFalse(cases.mail_matches_request({"agent": "r1", "epoch": "e7", "request": "q", "revision": 1},
            "r1", {"request": "q", "revision": 1}, "e7"))


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

    async def test_q2_uses_actual_native_ended_and_observed_nondefault_epoch(self):
        # Complete semantic helper with response doubles, never runtime evidence.
        def check(ok, message, **_):
            if not ok: raise CheckFailed(message)
        ctx = SimpleNamespace(agents=[{"id": "r1"}, {"id": "r2"}, {"id": "r3"}], check=check,
            native=SimpleNamespace(call=AsyncMock()), wait_agents=AsyncMock(), scope=AsyncMock(),
            native_set=AsyncMock(), grant=AsyncMock(), operation=AsyncMock(return_value="op"),
            command=AsyncMock(), resources=AsyncMock(return_value=[{"id": "r1", "data": {"agent": {"epoch": "e7"}}}]),
            peer=SimpleNamespace(call=AsyncMock(return_value={"entries": {"doc/agents/grants/q2-coordinator-answer": {"version": 1}}})))
        async def question(_ctx, identifier, **_):
            return {"request": identifier, "revision": 1, "scope": {"session": "s1"}}
        async def binding(_ctx, operation, **params):
            if operation != "observe": return {}
            q = params["question"]
            if q["request"] == "q2-exact":
                return {"mail": [{"to": "r2", "agent": "r1", "epoch": "e7", **q}], "human": []}
            return {"mail": [], "human": [{"reason": "unit observed fallback"}]}
        with patch.object(cases, "routing_binding", binding), patch.object(cases, "publish_question", question), patch.object(cases, "member", AsyncMock()):
            await cases.exact_recorded_coordinator_and_visible_fallback(ctx)
        ctx.native_set.assert_any_await(1, lifecycle="ended")
        lifecycles = [call.kwargs["lifecycle"] for call in ctx.native_set.await_args_list if "lifecycle" in call.kwargs]
        self.assertEqual(lifecycles, ["ended"], "the same ended incarnation is never resurrected")
        ctx.native.call.assert_awaited_once_with("add", ref="a2")
        ctx.native_set.assert_any_await(2, principal="worker:q2-other")
        ctx.scope.assert_awaited_once_with("resume", "agent:r2")
        ctx.grant.assert_any_await("q2-ended-answer", "worker:q2-coordinator", ["answer"], "agent:r1")

    async def test_complete_q3_uses_observed_epoch_for_every_real_response_argument(self):
        # Keep the actual respond() argument construction; double only public/native replies.
        for epoch in ("e1", "e7"):
            with self.subTest(epoch=epoch):
                responses, sent = [], []
                async def command(name, params, **_):
                    self.assertEqual(name, "control")
                    sent.append(params["args"]["epoch"])
                    if params["args"]["epoch"] != epoch or responses:
                        raise RpcError({"message": "stale", "data": {"kind": "stale"}})
                    responses.append({"answered": True})
                    return {"state": "observed"}
                async def resource_list(*_, **__):
                    return {"resources": [{"id": "r1", "data": {"agent": {"epoch": epoch, "requests": [
                        {"request": "q3-race", "status": "answered" if responses else "pending"}]}}}]}
                async def resources(): return (await resource_list())["resources"]
                async def binding(_ctx, operation, **params):
                    if operation != "observe": return {}
                    return {"human": [{"reason": "unit observed"}], "mail": [
                        {"to": "r2", "agent": "r1", "epoch": epoch, **params["question"]}]}
                async def question(_ctx, identifier, **_):
                    return {"request": identifier, "revision": 1, "scope": {"session": "s1"}}
                def check(ok, message, **_):
                    if not ok: raise CheckFailed(message)
                async def wait(predicate, **_): self.assertTrue(await predicate())
                peer = SimpleNamespace(call=AsyncMock(side_effect=resource_list))
                ctx = SimpleNamespace(agents=[{"id": "r1"}, {"id": "r2"}], peer=peer,
                    connect=AsyncMock(return_value=peer), native_set=AsyncMock(), grant=AsyncMock(), scope=AsyncMock(),
                    resources=resources, native=SimpleNamespace(records=lambda kind: responses),
                    operation=AsyncMock(return_value="op"), command=command, check=check, wait=wait)
                with patch.object(cases, "routing_binding", binding), patch.object(cases, "publish_question", question), patch.object(cases, "member", AsyncMock()):
                    await cases.both_routes_and_first_valid_answer_race(ctx)
                self.assertEqual(sent, [epoch] * 3)
                self.assertEqual(len(responses), 1)

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
