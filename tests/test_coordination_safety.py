"""Additional fake-ledger safety cases; not public-core/runtime qualification."""
import copy
import unittest
import test_coordination_engine as fixtures

from coordination_engine import CoordinationError
from coordination_policy import PolicyError


class SafetyTests(unittest.TestCase):
    def setUp(self):
        self.f = fixtures.EngineTests()
        self.f.setUp()

    def request(self, **fields):
        op = self.f.op()
        return {"op": op, "submission": op, "to": "r1", "mode": "start",
                "content": [{"type": "text", "text": "hello"}], **fields}

    def test_refused_operation_does_not_become_admitted_after_new_grant(self):
        f = self.f
        args = self.request()
        with self.assertRaises(PolicyError) as first:
            f.engine.send(args, fixtures.WORKER)
        f.config["doc/agents/grants/worker"] = {"version": 1, "value": {"who": "p:worker", "can": ["send:start"], "on": "agent:r1"}}
        with self.assertRaises(PolicyError) as repeated:
            f.engine.send(args, fixtures.WORKER)
        self.assertEqual(first.exception.kind, "no_grant")
        self.assertEqual(str(first.exception), str(repeated.exception))
        self.assertEqual(first.exception.data, repeated.exception.data)
        self.assertEqual(f.engine.records(), [])
        self.assertEqual(f.engine.send(self.request(), fixtures.WORKER)["state"], "recorded")

    def test_quota_refusal_is_cached_and_repeat_does_not_evaluate_policy(self):
        f = self.f
        args = self.request(content=[{"type": "text", "text": "x" * 40000}])
        with self.assertRaises(PolicyError) as first:
            f.engine.send(args, fixtures.OWNER)
        self.assertEqual(first.exception.kind, "quota")
        f.engine.policy = lambda: self.fail("a repeat must not evaluate current policy")
        with self.assertRaises(PolicyError) as repeated:
            f.engine.send(args, fixtures.OWNER)
        self.assertEqual(repeated.exception.data, first.exception.data)
        self.assertEqual(f.engine.operation(fixtures.OWNER["principal"], args["op"])["state"], "rejected")

    def test_no_reply_slot_closes_window_busy_before_evaluation(self):
        f = self.f
        original = f.engine._preflight_reservations
        def preflight(rows, before=None, **kwargs):
            if any("reserved" in entry.get("result", {}) for key, row in rows.items()
                   if key.startswith("ops/") for entry in row["value"].values()):
                raise CoordinationError("quota", "test reply budget is full")
            return original(rows, before, **kwargs)
        f.engine._preflight_reservations = preflight
        op = f.op()
        epoch = f.engine.window()["epoch"]
        with self.assertRaises(PolicyError) as failure:
            f.engine._mutate("probe", {"op": op}, fixtures.OWNER, lambda *_: self.fail("evaluated without a slot"))
        self.assertEqual(failure.exception.kind, "busy")
        self.assertNotEqual(f.engine.window()["epoch"], epoch)
        with self.assertRaises(PolicyError) as expired:
            f.engine._mutate("probe", {"op": op}, fixtures.OWNER, lambda *_: self.fail("recreated a closed-window operation"))
        self.assertEqual(expired.exception.kind, "expired_operation")
        self.assertEqual(f.ledger, [])

    def test_malformed_operation_never_consumes_window_slot(self):
        f = self.f
        before = f.store.snapshot()
        with self.assertRaises(PolicyError):
            f.engine.send({"op": None}, fixtures.OWNER)
        self.assertEqual(f.store.snapshot(), before)

    def test_native_receipt_during_invoke_never_allows_second_inflight_input(self):
        f = self.f
        f.resources["r1"]["data"]["agent"]["caps"]["agent.send"]["correlation"] = "insertion"
        first, second = f.send(), f.send(send_new=True)
        f.resume()
        def invoke(*args):
            reply = f.invoke(*args)
            if args[2].get("delivery") == first["delivery"]:
                self.assertTrue(f.engine.observe_receipts("r1", "i1", [{"epoch": "items1", "seq": 1,
                    "receipt": {"delivery": first["delivery"], "stage": "inserted", "run": "run1"}}]))
                self.assertIsNone(f.engine.dispatch_one())
            return reply
        f.engine.invoke = invoke
        reply = f.engine.dispatch_one()
        self.assertEqual(reply["state"], "observed")
        self.assertEqual(f.engine.get(first["delivery"])["state"], "inserted")
        self.assertEqual(len(f.ledger), 1)
        f.engine.dispatch_one()
        self.assertEqual([row[2]["delivery"] for row in f.ledger], [first["delivery"], second["delivery"]])

    def test_stale_native_rejection_latches_review_even_when_following_current_session(self):
        f = self.f
        sent = f.send(follow_current_session=True)
        f.resume()
        calls = []
        def invoke(*args):
            calls.append(args)
            if len(calls) == 1:
                return {"outcome": "rejected", "error": {"kind": "stale"}}
            return f.invoke(*args)
        f.engine.invoke = invoke
        f.resources["r1"]["data"]["agent"]["session"]["id"] = "s2"
        f.engine.dispatch_one()
        f.resources["r1"]["data"]["agent"]["session"]["id"] = "s3"
        self.assertIsNone(f.engine.dispatch_one())
        self.assertEqual(len(calls), 1)
        record = f.engine.get(sent["delivery"])
        self.assertEqual(record["review_required"], "stale_guard")
        f.engine.edit({"op": f.op(), "delivery": sent["delivery"], "expect": record["revision"]}, fixtures.OWNER)
        f.engine.dispatch_one()
        self.assertEqual(len(f.ledger), 1)
        self.assertEqual(f.ledger[0][2]["guard"]["session"], "s3")

    def test_uncorrelated_completion_receipt_cannot_advance_delivery(self):
        f = self.f
        sent = f.send()
        self.assertFalse(f.engine.observe_receipts("r1", "i1", [{"epoch": "items1", "seq": 1,
            "receipt": {"delivery": sent["delivery"], "stage": "completed"}}]))
        self.assertEqual(f.engine.get(sent["delivery"])["state"], "held")

    def test_known_native_end_is_visible_even_when_admission_was_already_paused(self):
        f = self.f
        sent = f.send()
        self.assertEqual(f.engine.get(sent["delivery"])["reason"], "paused")
        f.resources["r1"]["data"]["agent"]["lifecycle"] = "ended"
        self.assertIsNone(f.engine.dispatch_one())
        record = f.engine.get(sent["delivery"])
        self.assertEqual(record["state"], "held")
        self.assertEqual(record["reason"], "agent_ended")
        self.assertEqual(f.ledger, [])

    def test_identity_summary_keeps_the_core_canonical_backend_reference(self):
        f = self.f
        f.resources["r1"].update(provider="native-fixture", instance="instance1", backend_ref="opaque/native/reference")
        f.send()
        identity = f.engine.tombstones()["r1"]["identity"]
        self.assertEqual(identity, {"provider": "native-fixture", "instance": "instance1", "ref": "opaque/native/reference", "incarnation": "i1"})

    def test_scope_materializes_two_hundred_reserved_identity_summaries_atomically(self):
        f = self.f
        f.resources = {"r" + str(n): fixtures.resource("r" + str(n)) for n in range(1, 201)}
        f.resume()
        self.assertEqual(len(f.engine.tombstones()), 200)
        self.assertLessEqual(sum(key.startswith("agents/") for key in f.store.snapshot()), 16)
        self.assertEqual(len(f.engine.permits), 200)

    def test_known_ended_identity_cannot_be_resurrected_by_late_live_facts(self):
        f = self.f
        sent = f.send()
        f.resume()
        f.resources["r1"]["data"]["agent"]["lifecycle"] = "ended"
        f.engine.lifecycle_sweep()
        f.resources["r1"]["data"]["agent"]["lifecycle"] = "live"
        f.engine.lifecycle_sweep()
        self.assertEqual(f.engine.tombstones()["r1"]["lifecycle"], "ended")
        f.resume()
        self.assertIsNone(f.engine.dispatch_one())
        self.assertEqual(f.engine.get(sent["delivery"])["reason"], "agent_ended")
        with self.assertRaises(PolicyError) as rejected:
            f.send(send_new=True)
        self.assertEqual(rejected.exception.kind, "ended")
        self.assertEqual(f.ledger, [])

    def test_native_run_guard_is_forwarded_when_summary_has_no_run_field(self):
        f = self.f
        f.resources["r1"]["data"]["agent"]["caps"]["agent.send"]["guards"] = {"run": "runtime"}
        f.send(guard={"run": "native-run"})
        f.resume()
        f.engine.dispatch_one()
        self.assertEqual(f.ledger[0][2]["guard"], {"session": "s1", "run": "native-run"})

    def test_recipe_requires_every_step_right_before_any_step(self):
        f = self.f
        f.config["doc/agents/grants/worker"] = {"version": 1, "value": {"who": "p:worker", "can": ["send:interrupt_then_start", "queue", "interrupt"], "on": "agent:r1"}}
        with self.assertRaises(PolicyError) as failure:
            f.engine.send(self.request(mode="preempt", strategy="interrupt_then_start"), fixtures.WORKER)
        self.assertEqual(failure.exception.kind, "no_grant")
        self.assertEqual(f.engine.records(), [])
        self.assertEqual(f.ledger, [])

    def test_pull_filters_before_claim_and_can_insert_supported_later_input(self):
        f = self.f
        f.setup_pull()
        unsupported = f.send("boundary", content=[{"type": "image", "mime": "image/png", "data": "YQ=="}])
        supported = f.send("boundary", send_new=True)
        result = f.engine.take({"op": f.op(), "agent": "r1", "consumer": "c1", "at": "turn", "accept": ["text"]}, fixtures.OWNER)
        self.assertEqual([row["delivery"] for row in result["records"]], [supported["delivery"]])
        record = f.engine.get(unsupported["delivery"])
        self.assertEqual(record["reason"], "unsupported_content")
        self.assertEqual(record["reason_consumer"], "c1")
        self.assertIsNone(record.get("attempt"))
        self.assertEqual(result["unsupported"], 1)

    def test_pull_unsupported_is_never_claimed_then_released_repeatedly(self):
        f = self.f
        f.setup_pull()
        sent = f.send("boundary", content=[{"type": "audio", "mime": "audio/wav", "data": "YQ=="}])
        for _ in range(3):
            result = f.engine.take({"op": f.op(), "agent": "r1", "consumer": "c1", "at": "turn"}, fixtures.OWNER)
            self.assertEqual(result["records"], [])
            self.assertEqual(result["held"], "unsupported_content")
        self.assertIsNone(f.engine.get(sent["delivery"]).get("attempt"))
        f.engine.reconcile({"op": f.op(), "agent": "r1", "consumer": "c2", "records": []}, fixtures.OWNER)
        capable = f.engine.take({"op": f.op(), "agent": "r1", "consumer": "c2", "at": "turn", "accept": ["audio"]}, fixtures.OWNER)
        self.assertEqual(capable["records"][0]["delivery"], sent["delivery"])
        self.assertNotIn("reason_consumer", f.engine.get(sent["delivery"]))

    def test_pull_byte_filter_skips_oversized_head_without_claim_release_loop(self):
        f = self.f
        f.setup_pull()
        large = f.send("boundary", content=[{"type": "text", "text": "large" * 1000}])
        small = f.send("boundary", send_new=True)
        result = f.engine.take({"op": f.op(), "agent": "r1", "consumer": "c1", "at": "turn", "accept": ["text"], "max_bytes": 256}, fixtures.OWNER)
        self.assertEqual([row["delivery"] for row in result["records"]], [small["delivery"]])
        held = f.engine.get(large["delivery"])
        self.assertEqual(held["reason"], "too_large_for_consumer")
        self.assertIsNone(held.get("attempt"))
        self.assertEqual(result["oversized"], 1)
        f.engine.report({"op": f.op(), "delivery": small["delivery"], "claim": result["records"][0]["claim"], "consumer": "c1", "stage": "inserted"}, fixtures.OWNER)
        again = f.engine.take({"op": f.op(), "agent": "r1", "consumer": "c1", "at": "turn", "max_bytes": 256}, fixtures.OWNER)
        self.assertEqual(again["records"], [])
        self.assertEqual(again["held"], "too_large_for_consumer")
        self.assertIsNone(f.engine.get(large["delivery"]).get("attempt"))
        f.engine.reconcile({"op": f.op(), "agent": "r1", "consumer": "c2", "records": []}, fixtures.OWNER)
        capable = f.engine.take({"op": f.op(), "agent": "r1", "consumer": "c2", "at": "turn", "max_bytes": 8192}, fixtures.OWNER)
        self.assertEqual(capable["records"][0]["delivery"], large["delivery"])

    def test_native_dispatcher_does_not_erase_current_pull_consumer_byte_hold(self):
        f = self.f
        f.setup_pull()
        f.resources["r1"]["kind"] = "terminal"
        f.resources["r1"]["data"]["agent"]["caps"] = {}
        f.config["doc/agents/terminals/r1"] = {"version": 1, "value": {"pull": f.resources["r1"]["data"]["agent"]["pull"]}}
        sent = f.send("boundary", content=[{"type": "text", "text": "x" * 5000}])
        f.engine.take({"op": f.op(), "agent": "r1", "consumer": "c1", "at": "turn", "max_bytes": 256}, fixtures.OWNER)
        self.assertEqual(f.engine.get(sent["delivery"])["reason"], "too_large_for_consumer")
        self.assertIsNone(f.engine.dispatch_one())
        self.assertEqual(f.engine.get(sent["delivery"])["reason"], "too_large_for_consumer")
        f.now += 300001
        f.engine.dispatch_one()
        self.assertEqual(f.engine.get(sent["delivery"])["state"], "expired")

    def test_pull_byte_budget_uses_encoded_utf8_content_and_validates_bounds(self):
        f = self.f
        f.setup_pull()
        sent = f.send("boundary", content=[{"type": "text", "text": "é" * 80}])
        result = f.engine.take({"op": f.op(), "agent": "r1", "consumer": "c1", "at": "turn", "max_bytes": 100}, fixtures.OWNER)
        self.assertEqual(result["records"], [])
        self.assertEqual(f.engine.get(sent["delivery"])["reason"], "too_large_for_consumer")
        for budget in (0, -1, True, "100", 262145):
            with self.assertRaises(PolicyError) as invalid:
                f.engine.take({"op": f.op(), "agent": "r1", "consumer": "c1", "at": "turn", "max_bytes": budget}, fixtures.OWNER)
            self.assertEqual(invalid.exception.kind, "invalid")

    def test_take_bounds_reason_updates_and_claims_by_c1_wire_budget(self):
        f = self.f
        f.setup_pull()
        f.config["doc/agents/grants/senders"] = {"version": 1, "value": {"who": "*", "can": ["send:boundary"], "on": "agent:r1"}}
        for n in range(7):
            principal = "p:sender" + str(n)
            context = {"principal": principal, "actor": principal, "author": "client:test:" + str(n)}
            f.engine.send(self.request(mode="boundary", content=[{"type": "text", "text": "x" * 30000}]), context)
        small = f.send("boundary")
        original = f.store.apply
        payloads = []
        def apply(ops, **kwargs):
            from coordination_engine import encoded
            wire = [dict(op, key="operational/coord/" + op["key"]) for op in ops]
            payloads.append(encoded({"domain": "coord", "ops": wire}))
            return original(ops, **kwargs)
        f.store.apply = apply
        result = f.engine.take({"op": f.op(), "agent": "r1", "consumer": "c1", "at": "turn", "max_bytes": 256}, fixtures.OWNER)
        self.assertEqual([row["delivery"] for row in result["records"]], [small["delivery"]])
        self.assertEqual(result["oversized"], 7)
        self.assertTrue(payloads)
        self.assertLess(max(payloads), 256 * 1024)

    def test_pull_accept_types_are_bounded_and_deterministic(self):
        f = self.f
        f.setup_pull()
        for accept in ([], ["text", "text"], ["x" * 65], [True], ["x"] * 17):
            with self.assertRaises(PolicyError) as failure:
                f.engine.take({"op": f.op(), "agent": "r1", "consumer": "c1", "at": "turn", "accept": accept}, fixtures.OWNER)
            self.assertEqual(failure.exception.kind, "invalid")

    def test_unsupported_stop_closes_admission_without_fabricating_native_result(self):
        f = self.f
        f.resume()
        result = f.engine.control({"op": f.op(), "target": "r1", "action": "stop"}, fixtures.OWNER)
        self.assertEqual(result["state"], "partial")
        self.assertIsNone(result["native"])
        self.assertNotIn("r1", f.engine.permits)
        self.assertEqual(f.ledger, [])


if __name__ == "__main__":
    unittest.main()
