# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Complete-helper checker negatives; doubles are not public-core certification."""
from copy import deepcopy
from types import SimpleNamespace
import unittest

from agent_conformance.part_b import handover_cases as subject
from agent_conformance.part_b.context import PublicCoordination
from agent_conformance.results import CheckFailed
from agent_conformance.wire import RpcError

SOURCE = {"id": "source", "incarnation": "source-inc", "backend_ref": "native-source"}
DESTINATION = {"id": "dest", "incarnation": "dest-inc", "backend_ref": "native-dest"}


def pending(delivery="moved", agent=DESTINATION):
    return {"delivery": delivery, "state": "held", "reason": "handover_pending", "attempt": None,
        "to": subject.exact(agent), "content": [{"type": "text", "text": "preserve me"}],
        "guard": {"session": "s1"}, "intent": "intent:" + delivery, "origin": {"principal": "sender"}}


def invocation():
    return {"op": "agent.send", "ref": DESTINATION["backend_ref"], "seq": 1,
        "args": {"delivery": "sentinel", "mode": "admit"}, "witness": {"entry": {"value": {
            "state": "claimed", "delivery": "sentinel", "to": subject.exact(DESTINATION),
            "mode": "admit", "attempt": {"strategy": "admit"}}}}}


class Context:
    """Same complete helper path and actual generic definite-refusal grader."""
    check = PublicCoordination.check
    rejected = PublicCoordination.rejected

    def __init__(self, rows, calls=(), mutation=None):
        self.rows, self.calls = deepcopy(rows), deepcopy(list(calls))
        self.mutation = mutation
        self.evidence, self.assertions = [], 0
        self.agents = [deepcopy(SOURCE), deepcopy(DESTINATION)]
        self.gates = {SOURCE["id"]: False, DESTINATION["id"]: True}
        self.peer = SimpleNamespace(call=self.rpc)
        self.native = SimpleNamespace(records=lambda kind, **kwargs: self.calls if kind == "invoke" else [])

    async def rpc(self, method, params):
        assert method == "operational.get"
        if params["key"] == "operational/coord/control":
            return {"entry": {"value": {"gates": deepcopy(self.gates)}}}
        return {"entry": {"value": deepcopy(self.rows[params["key"].rsplit("/", 1)[-1]])}}

    async def send(self, **kwargs):
        assert kwargs["target"] == DESTINATION["id"] and kwargs["mode"] == "admit"
        return {"delivery": "sentinel"}

    async def wait_command(self, name, predicate, **kwargs):
        row = {"state": "accepted"}
        assert predicate(row)
        return row

    async def operation(self, *args):
        return "op"

    async def command(self, name, args, **kwargs):
        assert name == "handover"
        if self.mutation: self.mutation(self.rows)
        raise RpcError({"message": "too late", "data": {"kind": "too_late"}})


def mixed(state):
    rows = {"sibling": pending("sibling", SOURCE), "protected": pending("protected", SOURCE)}
    rows["protected"].update(state=state, attempt={"id": "attempt"})
    return rows


class SchedulingWitness(unittest.IsolatedAsyncioTestCase):
    async def grade(self, call):
        context = Context({"moved": pending()}, [call])
        await subject.scheduling_witness(context, ["moved"], DESTINATION["id"], "plan", "unit")
        return context

    async def test_real_observed_destination_admit_claim_passes(self):
        context = await self.grade(invocation())
        self.assertEqual(context.evidence[-1]["kind"], "handover_scheduling_barrier")

    async def test_reviewer_wrong_endpoint_start_and_other_claim_target_fails(self):
        call = invocation()
        call.update(ref="WRONG-ENDPOINT")
        call["args"]["mode"] = "start"
        call["witness"]["entry"]["value"]["to"] = {"agent": "wrong-agent", "incarnation": "wrong-inc"}
        with self.assertRaises(CheckFailed): await self.grade(call)

    async def test_each_actual_endpoint_lane_and_claim_field_is_required(self):
        mutations = [
            lambda c: c.update(ref="wrong"),
            lambda c: c["args"].update(mode="start"),
            lambda c: c["args"].update(delivery="wrong"),
            lambda c: c["witness"]["entry"]["value"].update(delivery="wrong"),
            lambda c: c["witness"]["entry"]["value"]["to"].update(agent="wrong"),
            lambda c: c["witness"]["entry"]["value"]["to"].update(incarnation="wrong"),
            lambda c: c["witness"]["entry"]["value"].update(mode="start"),
            lambda c: c["witness"]["entry"]["value"]["attempt"].update(strategy="start"),
            lambda c: c["witness"]["entry"]["value"].update(state="recorded"),
        ]
        for index, mutate in enumerate(mutations):
            with self.subTest(field=index):
                call = invocation(); mutate(call)
                with self.assertRaises(CheckFailed): await self.grade(call)


class MixedRefusal(unittest.IsolatedAsyncioTestCase):
    async def grade(self, state, mutation=None):
        context = Context(mixed(state), mutation=mutation)
        await subject.refusal_preserves(context, SOURCE, DESTINATION, ["sibling", "protected"], state)
        return context

    async def test_unchanged_mixed_claimed_and_unknown_refusals_pass(self):
        for state in ("claimed", "unknown"):
            with self.subTest(state=state): await self.grade(state)

    async def test_reviewer_withdrawn_content_lost_pending_sibling_fails(self):
        def mutate(rows):
            rows["sibling"].update(state="withdrawn", content=[])
        for state in ("claimed", "unknown"):
            with self.subTest(state=state):
                with self.assertRaises(CheckFailed): await self.grade(state, mutate)

    async def test_each_members_protected_state_and_intent_must_remain_unchanged(self):
        mutations = {"state": "withdrawn", "reason": "changed", "content": [], "guard": {"session": "wrong"},
            "origin": {"principal": "wrong"}, "intent": "changed", "to": subject.exact(DESTINATION),
            "attempt": {"id": "changed"}, "handover_plan": "unexpected"}
        for state in ("claimed", "unknown"):
            for member in ("sibling", "protected"):
                for key, value in mutations.items():
                    with self.subTest(state=state, member=member, field=key):
                        def mutate(rows, member=member, key=key, value=value):
                            rows[member][key] = deepcopy(value)
                        with self.assertRaises(CheckFailed): await self.grade(state, mutate)


class PauseReadback(unittest.IsolatedAsyncioTestCase):
    async def test_actual_public_gates_not_ended_source_ineligibility_prove_pause(self):
        context = Context({})
        await subject.gate_cut(context, {SOURCE["id"]: False, DESTINATION["id"]: True})
        for wrong in (True, 0, None):
            with self.subTest(source_gate=wrong):
                context.gates[SOURCE["id"]] = wrong
                with self.assertRaises(CheckFailed):
                    await subject.gate_cut(context, {SOURCE["id"]: False, DESTINATION["id"]: True})
