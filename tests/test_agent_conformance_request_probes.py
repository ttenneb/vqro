"""Self-tests for the independent R3/R4 request probes: a simulated Pi-like adapter plus an independent native
ledger, with knobs for the adversarial behaviours the probes must refuse."""
import asyncio
import itertools
from types import SimpleNamespace
import unittest

from agent_conformance.request_probes import answer_acknowledgement, positive_relative_timeout
from agent_conformance.results import CheckFailed, HarnessGap


class Ledger:
    """The independent native ledger (as the fixture writes it) plus the private control surface."""
    def __init__(self, adapter):
        self.adapter, self.rows, self.seq = adapter, [], 0

    def record(self, kind, **details):
        self.seq += 1
        self.rows.append({"seq": self.seq, "kind": kind, **details})

    async def call(self, verb, *, timeout=2, **params):
        return await self.call_fields(verb, params, timeout=timeout)

    async def call_fields(self, verb, params, *, timeout=2):
        assert verb == "request"
        event = {"type": "extension_ui_request", "id": params["request"], "method": params["method"]}
        if "timeout" in params and not self.adapter.omit_timeout_witness: event["timeout"] = params["timeout"]
        self.record("request", request=params["request"], method=params["method"], native_event=event,
                    **({"timeout": params["timeout"]} if "timeout" in event else {}))
        await self.adapter.native_request(dict(params))
        return {}

    def records(self, kind=None, *, after=0, operation=None):
        return [row for row in self.rows if row["seq"] > after and (kind is None or row["kind"] == kind)]

    def mark(self):
        return self.seq

    def writes(self, after=0):
        return []

    async def wait(self, predicate, *, timeout=2):
        for _ in range(int(timeout / .01)):
            if predicate(self.records()):
                return True
            await asyncio.sleep(.01)
        raise CheckFailed("native ledger condition did not occur within fixture deadline")


class Adapter:
    """A Pi-shaped adapter: operation-scoped dialogs, answers dispatched without a native acknowledgement."""
    def __init__(self, *, ack="dispatched", native_timeout="unrepresentable_positive", outcome=None, handoffs=1,
                 ignore_timeout=False, auto_approve_timed=False, cancels=1, invent_expiry=False, keep_timed_pending=False,
                 accept_repeat=False, native_ack=False, transient_approvable=False, omit_timeout_witness=False):
        self.ack, self.native_timeout, self.outcome = ack, native_timeout, outcome or ack
        self.handoffs, self.ignore_timeout, self.auto_approve_timed = handoffs, ignore_timeout, auto_approve_timed
        self.cancels, self.invent_expiry, self.keep_timed_pending = cancels, invent_expiry, keep_timed_pending
        self.accept_repeat, self.native_ack = accept_repeat, native_ack
        self.transient_approvable, self.omit_timeout_witness = transient_approvable, omit_timeout_witness
        self.native = Ledger(self)
        self.requests, self.answered, self.ids = {}, set(), itertools.count(1)
        self.on_change = lambda: None  # the item stream pushes entity changes to live views

    def cancel(self, native_id, times=1):
        for _ in range(times):
            self.native.record("answer", request=native_id, normalized_response={"action": "cancel"}, pending=True)

    async def native_request(self, row):
        native_id, method = row["request"], row["method"]
        public = {"request": f"q{next(self.ids)}", "revision": 1, "method": method, "title": row.get("title", "t"),
                  "params": {}, "scope": {"operation": "op-" + native_id}, "status": "pending", "incomplete": False,
                  "native": native_id}
        if method == "choice":
            public["choices"] = [{"id": "c1", "label": "one"}]
        timed = isinstance(row.get("timeout"), int) and row["timeout"] > 0 and not self.ignore_timeout
        try:
            if timed and self.transient_approvable:
                self.requests[public["request"]] = dict(public)
                self.on_change()
                del self.requests[public["request"]]
            self.native_dialog(public, row, native_id, timed)
        finally:
            self.on_change()

    def native_dialog(self, public, row, native_id, timed):
        if timed:
            if self.invent_expiry:  # a visible, cancelled entity that claims an absolute deadline
                public.update(expires_at_ms=123, status="cancelled")
            if self.keep_timed_pending or self.invent_expiry:
                self.requests[public["request"]] = public
            if self.auto_approve_timed:
                self.native.record("answer", request=native_id, normalized_response={"value": True}, pending=True)
            else:
                self.cancel(native_id, self.cancels)
            return
        self.requests[public["request"]] = public

    async def respond(self, args):
        request = self.requests.get(args["request"])
        if request is None or (request["request"] in self.answered and not self.accept_repeat):
            return {"outcome": "rejected", "error": {"data": {"kind": "stale"}}}
        response = args["response"]
        if request["status"] != "pending" or request.get("incomplete"):
            return {"outcome": "rejected", "error": {"data": {"kind": "invalid"}}}
        self.answered.add(request["request"])
        for _ in range(self.handoffs):
            self.native.record("answer", request=request["native"], normalized_response=dict(response), pending=True)
        if self.native_ack:
            self.native.record("answer_ack", request=request["native"])
        return {"outcome": self.outcome}


class Context:
    def __init__(self, adapter):
        self.adapter, self.native, self.evidence, self.serial = adapter, adapter.native, [], 0
        adapter.on_change = self.refresh

    def delivery(self):
        self.serial += 1
        return f"d{self.serial}"

    def pending(self):
        return [dict(q) for q in self.adapter.requests.values()
                if q["request"] not in self.adapter.answered and q["status"] == "pending"]

    async def facts(self):
        return {"caps": {"agent.respond": {"v": 0, "methods": ["confirm", "choice", "text"], "scopes": ["operation"],
                                           "background_requests": False, "ack": self.adapter.ack,
                                           "native_timeout": self.adapter.native_timeout}},
                "requests": self.pending()}

    async def wait_facts(self, predicate, rid=None, *, timeout=2):
        for _ in range(200):
            facts = await self.facts()
            if predicate(facts):
                return facts
            await asyncio.sleep(.01)
        raise CheckFailed("facts did not reach the required state")

    async def viewer(self):
        return SimpleNamespace(close=self.noop)

    async def noop(self):
        pass

    async def attach(self, *, rid=None, peer=None, after=None, seed=None):
        reducer = SimpleNamespace(entities={}, snapshots=1, sequence=0, trace=[], trace_dropped=0)
        view = SimpleNamespace(reducer=reducer, initial="reset", peer=peer)
        self.views = getattr(self, "views", []) + [view]
        self.refresh()
        return view

    def refresh(self):
        for view in getattr(self, "views", []):
            current = {}
            for q in self.adapter.requests.values():
                key, value = ("request", q["request"]), dict(q)
                current[key] = {"value": value}
                if view.reducer.entities.get(key) != current[key]:
                    view.reducer.sequence += 1
                    view.reducer.trace.append({"op": "put", "kind": "request", "id": q["request"],
                                               "seq": view.reducer.sequence, "value": value})
            for kind, ident in view.reducer.entities.keys() - current.keys():
                view.reducer.sequence += 1
                view.reducer.trace.append({"op": "remove", "kind": kind, "id": ident, "seq": view.reducer.sequence})
            view.reducer.entities = current

    async def wait_view(self, view, predicate, *, timeout=2):
        self.refresh()
        if not predicate(view.reducer):
            raise CheckFailed("item stream did not reach expected state")

    async def invoke(self, op, args=None, **kwargs):
        assert op == "agent.respond"
        result = await self.adapter.respond(args)
        self.refresh()
        return result


def run(probe, **knobs):
    ctx = Context(Adapter(**knobs))
    asyncio.run(probe(ctx))
    return ctx


class AnswerAcknowledgementTests(unittest.TestCase):
    def test_dispatched_with_exactly_one_native_handoff_passes_with_witness(self):
        ctx = run(answer_acknowledgement)
        witness = next(row for row in ctx.evidence if row["kind"] == "answer_acknowledgement")
        self.assertEqual((witness["declared"], witness["outcome"], witness["native_acks"]), ("dispatched", "dispatched", 0))
        self.assertTrue(witness["native_answer_seq"] > 0)
        na = next(row for row in ctx.evidence if row.get("case") == "R3.accepted_ack_boundary")
        self.assertEqual((na["status"], na["exclusion"]["path"]), ("not-applicable", "agent.respond.ack"))

    def test_no_native_handoff_fails_even_with_matching_outcome(self):
        with self.assertRaisesRegex(CheckFailed, "no native handoff"):
            run(answer_acknowledgement, handoffs=0)

    def test_false_accepted_without_native_ack_fails(self):
        with self.assertRaisesRegex(CheckFailed, "accepted without a native acknowledgement"):
            run(answer_acknowledgement, outcome="accepted")

    def test_duplicate_native_responses_fail(self):
        with self.assertRaisesRegex(CheckFailed, "2 times"):
            run(answer_acknowledgement, handoffs=2)

    def test_repeat_answer_reaching_native_fails(self):
        with self.assertRaises(CheckFailed):
            run(answer_acknowledgement, accept_repeat=True)

    def test_declared_accepted_is_a_harness_gap_not_a_fake_ack(self):
        with self.assertRaises(HarnessGap):
            run(answer_acknowledgement, ack="accepted", native_ack=True)

    def test_unknown_ack_declaration_is_a_harness_gap(self):
        with self.assertRaises(HarnessGap):
            run(answer_acknowledgement, ack="maybe")


class PositiveRelativeTimeoutTests(unittest.TestCase):
    def test_untimed_answerable_and_timed_cancelled_once(self):
        ctx = run(positive_relative_timeout)
        witness = next(row for row in ctx.evidence if row["kind"] == "positive_relative_timeout")
        self.assertEqual(witness["native_responses"], 1)
        self.assertGreater(witness["timed_native_cancel_seq"], witness["untimed_native_answer_seq"])
        timed = [r for r in ctx.native.rows if r.get("request") == witness["timed_native_request"] and r["kind"] == "answer"]
        self.assertEqual([r["normalized_response"] for r in timed], [{"action": "cancel"}])

    def test_other_declaration_is_an_explicit_na_subcase_without_native_calls(self):
        ctx = run(positive_relative_timeout, native_timeout="absolute")
        self.assertEqual(ctx.native.rows, [])
        na = ctx.evidence[0]
        self.assertEqual((na["status"], na["exclusion"]["path"], na["exclusion"]["actual"]),
                         ("not-applicable", "agent.respond.native_timeout", "absolute"))

    def test_missing_native_timeout_witness_is_a_fixture_failure(self):
        with self.assertRaisesRegex(CheckFailed, "fixture did not emit") as caught:
            run(positive_relative_timeout, omit_timeout_witness=True)
        self.assertEqual(caught.exception.component, "fixture")

    def test_omitted_timeout_handling_fails(self):
        with self.assertRaisesRegex(CheckFailed, "never answered|approvable"):
            run(positive_relative_timeout, ignore_timeout=True)

    def test_auto_approval_fails(self):
        with self.assertRaisesRegex(CheckFailed, "answered, not cancelled"):
            run(positive_relative_timeout, auto_approve_timed=True)

    def test_double_cancel_fails(self):
        with self.assertRaisesRegex(CheckFailed, "2 native responses"):
            run(positive_relative_timeout, cancels=2)

    def test_invented_absolute_expiry_fails(self):
        with self.assertRaisesRegex(CheckFailed, "absolute expiry"):
            run(positive_relative_timeout, invent_expiry=True)

    def test_transient_approvable_entity_cannot_hide_behind_final_removal(self):
        with self.assertRaisesRegex(CheckFailed, "complete pending request"):
            run(positive_relative_timeout, transient_approvable=True)

    def test_timed_dialog_left_pending_fails(self):
        with self.assertRaisesRegex(CheckFailed, "approvable"):
            run(positive_relative_timeout, keep_timed_pending=True)


if __name__ == "__main__":
    unittest.main()
