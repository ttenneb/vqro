"""State-machine evidence against a fake native ledger, not runtime certification."""
import copy
from pathlib import Path
import sys
import threading
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
from coordination_engine import Coordinator, CoordinationError
from coordination_policy import PolicyError
from coordination_store import MemoryStore, StoreError

OWNER = {"principal": "p:owner", "actor": "p:owner", "author": "client:boot:1", "kind": "human"}
WORKER = {"principal": "p:worker", "actor": "p:worker", "author": "client:boot:2", "kind": "agent", "agent": "r2"}


def resource(agent="r1", state="idle", pull=None):
    native = {"v": 0, "state": state, "lifecycle": "live", "session": {"id": "s1"}, "epoch": "e1",
              "caps": {"rev": 1, "agent.send": {"modes": {"start": {"busy": "reject"}, "admit": {}, "boundary": {}, "settle": {}},
                                               "origin": [{"as": "attributed_user", "role": "user", "frame": "sender_header"}]},
                       "agent.interrupt": {"pending": "unknown"}, "agent.queue": {"actions": ["clear"]}}}
    if pull:
        native["pull"] = pull
    return {"id": agent, "kind": "agent", "incarnation": "i1", "available": True, "data": {"agent": native}}


class EngineTests(unittest.TestCase):
    def setUp(self):
        self.now = 1000
        self.config = {"doc/principals/owner": {"version": 1, "value": "p:owner"}}
        self.resources = {"r1": resource()}
        self.store = MemoryStore()
        self.ledger = []
        def invoke(target, op, args, deadline):
            saved = self.store.get("message/" + args.get("delivery", ""))
            if op == "agent.send":
                self.assertEqual(saved["value"]["state"], "claimed")
                self.assertEqual(saved["value"]["attempt"]["control_epoch"], self.engine.control_epoch)
            self.ledger.append((target, op, copy.deepcopy(args)))
            return {"outcome": "accepted", "data": {"receipt": {"stage": "admitted"}}}
        self.invoke = invoke
        self.engine = self.make_engine()
        self.n = 0

    def make_engine(self):
        return Coordinator(self.store, config=lambda: copy.deepcopy(self.config), resources=lambda: copy.deepcopy(self.resources),
                           clock=lambda: self.now, invoke=self.invoke, core_epoch="core", owner_incarnation="generation-" + str(self.store.revision))

    def op(self, suffix=None):
        self.n += 1
        return self.engine.window()["epoch"] + ":" + (suffix or str(self.n))

    def send(self, mode="start", **extra):
        op = self.op()
        args = {"op": op, "submission": op, "to": "r1", "mode": mode, "content": [{"type": "text", "text": "hello"}], **extra}
        return self.engine.send(args, OWNER)

    def resume(self, scope="workspace", action="resume", context=None):
        return self.engine.scope({"op": self.op(), "action": action, "scope": scope,
                                  "expect": self.engine.scope_preview(scope)["expect"]}, context or OWNER)

    def test_restart_starts_paused_then_first_attempt_is_durable(self):
        sent = self.send()
        self.assertIsNone(self.engine.dispatch_one())
        self.assertEqual(self.ledger, [])
        self.resume()
        self.assertEqual(self.engine.dispatch_one()["state"], "accepted")
        self.assertEqual(len(self.ledger), 1)
        self.assertIsNone(self.engine.dispatch_one())
        self.assertEqual(self.engine.get(sent["delivery"])["state"], "accepted")

    def test_blocked_agent_holds_and_resumes_on_the_fact_change(self):
        # agent-v0 availability: held agent_blocked only while the runtime reports blocked; nothing is resent.
        agent = self.resources["r1"]["data"]["agent"]
        agent["availability"] = {"state": "blocked", "reason": "_retrying", "retry_after_ms": 2000}
        sent = self.send()
        self.resume()
        self.assertIsNone(self.engine.dispatch_one())
        self.assertEqual(self.ledger, [])
        held = self.engine.get(sent["delivery"])
        self.assertEqual((held["state"], held["reason"]), ("held", "agent_blocked"))
        agent["availability"] = {"state": "available"}
        self.assertEqual(self.engine.dispatch_one()["state"], "accepted")
        self.assertEqual([op for _, op, _ in self.ledger], ["agent.send"], "one attempt, no replay")

    def test_absent_or_unknown_availability_is_not_blocked(self):
        # Negative controls: an absent fact, an unknown state, a non-object or an unknown reason without a blocked
        # state never hold; only state == "blocked" does.
        for availability in (None, {"state": "degraded"}, "blocked", {"reason": "rate_limited"}):
            with self.subTest(availability=availability):
                self.setUp()
                agent = self.resources["r1"]["data"]["agent"]
                if availability is not None:
                    agent["availability"] = availability
                self.send()
                self.resume()
                self.assertEqual(self.engine.dispatch_one()["state"], "accepted")

    def test_crash_after_claim_becomes_unknown_and_never_resends(self):
        sent = self.send()
        self.resume()
        work = __import__("coordination_engine").Working(self.engine)
        record = self.engine.get(sent["delivery"])
        record.pop("revision")
        self.engine._claim(work, record)
        work.commit()
        self.engine.fence()
        self.engine = self.make_engine()
        self.assertEqual(self.engine.get(sent["delivery"])["state"], "unknown")
        self.resume()
        self.engine.dispatch_one()
        self.assertEqual(self.ledger, [])

    def test_claim_unconfirmed_does_not_invoke(self):
        sent = self.send()
        self.resume()
        self.store.fault = "after"
        with self.assertRaises(StoreError):
            self.engine.dispatch_one()
        self.assertEqual(self.ledger, [])
        self.assertEqual(self.engine.get(sent["delivery"])["reason"], "claim_unconfirmed")

    def test_second_pending_requires_explicit_send_new(self):
        first = self.send()
        with self.assertRaises(PolicyError) as caught:
            self.send()
        self.assertEqual(caught.exception.kind, "pending_exists")
        self.assertNotEqual(self.send(send_new=True)["delivery"], first["delivery"])

    def test_operation_repeat_and_changed_payload_conflict(self):
        op = self.op()
        args = {"op": op, "submission": op, "to": "r1", "mode": "start", "content": [{"type": "text", "text": "first"}]}
        first = self.engine.send(args, OWNER)
        replay = self.engine.send(args, OWNER)
        self.assertEqual(first["delivery"], replay["delivery"])
        self.assertTrue(replay["historical"])
        self.assertEqual(len(self.engine.records()), 1)
        with self.assertRaises(PolicyError) as caught:
            self.engine.send(dict(args, content=[{"type": "text", "text": "changed"}]), OWNER)
        self.assertEqual(caught.exception.kind, "conflict")

    def test_old_window_cannot_recreate_submission(self):
        old = self.op()
        row = self.store.get("meta")
        meta = row["value"]
        meta["window_epoch"] = "new-window"
        self.store.apply([{"op": "set", "key": "meta", "expect": row["version"], "value": meta}])
        with self.assertRaises(PolicyError) as caught:
            self.engine.send({"op": old, "submission": old, "to": "r1", "mode": "start", "content": [{"type": "text", "text": "old"}]}, OWNER)
        self.assertEqual(caught.exception.kind, "expired_operation")
        self.assertEqual(self.engine.records(), [])

    def test_pause_and_old_resume_lookup_never_reopen(self):
        self.send()
        scope_args = {"op": self.op(), "action": "resume", "scope": "workspace", "expect": self.engine.scope_preview("workspace")["expect"]}
        self.engine.scope(scope_args, OWNER)
        self.resume(action="pause")
        self.engine.scope(scope_args, OWNER)
        self.assertNotIn("r1", self.engine.permits)
        self.engine.dispatch_one()
        self.assertEqual(self.ledger, [])

    def test_scope_membership_cut_and_self_resume_authorization(self):
        args = {"op": self.op(), "action": "resume", "scope": "workspace", "expect": self.engine.scope_preview("workspace")["expect"]}
        self.config["doc/agents/members/r1"] = {"version": 2, "value": {"project": "p", "reports_to": None}}
        with self.assertRaises(PolicyError) as caught:
            self.engine.scope(args, OWNER)
        self.assertEqual(caught.exception.kind, "conflict")
        with self.assertRaises(PolicyError):
            self.resume(context=WORKER)
        self.assertEqual(self.engine.permits, set())

    def test_edit_before_claim_changes_payload_and_after_claim_is_too_late(self):
        sent = self.send()
        row = self.engine.get(sent["delivery"])
        self.engine.edit({"op": self.op(), "delivery": sent["delivery"], "expect": row["revision"],
                          "content": [{"type": "text", "text": "edited"}]}, OWNER)
        self.resume()
        self.engine.dispatch_one()
        self.assertEqual(self.ledger[0][2]["content"][0]["text"], "edited")
        with self.assertRaises(PolicyError) as caught:
            self.engine.edit({"op": self.op(), "delivery": sent["delivery"], "expect": self.engine.get(sent["delivery"])["revision"]}, OWNER, withdraw=True)
        self.assertEqual(caught.exception.kind, "too_late")

    def test_expiry_and_session_guard_are_not_renewed_by_resume(self):
        sent = self.send("boundary", expires_at_ms=2000)
        self.now = 3000
        self.resume()
        self.engine.dispatch_one()
        self.assertEqual(self.engine.get(sent["delivery"])["state"], "expired")
        second = self.send()
        self.resources["r1"]["data"]["agent"]["session"]["id"] = "s2"
        self.engine.dispatch_one()
        self.assertEqual(self.engine.get(second["delivery"])["reason"], "session_changed")
        self.assertEqual(self.ledger, [])

    def test_origin_is_derived_not_quoted_or_body(self):
        sent = self.send(quoted={"principal": "other-owner", "kind": "human"})
        self.assertEqual(self.engine.get(sent["delivery"])["origin"]["principal"], OWNER["principal"])
        with self.assertRaises(PolicyError):
            self.send(origin={"principal": "forged"}, send_new=True)

    def test_held_start_does_not_block_boundary(self):
        self.resources["r1"]["data"]["agent"]["state"] = "running"
        self.send()
        boundary = self.send("boundary", send_new=True)
        self.resume()
        self.engine.dispatch_one()
        self.assertEqual(self.ledger[0][2]["delivery"], boundary["delivery"])

    def setup_pull(self, *, context=False):
        self.resources["r1"]["data"]["agent"]["pull"] = {"points": {point: {"precision": "exact"} for point in ("turn", "settle", "idle", "tool")},
                                                             "insert": "context" if context else "user", "evidence": "none" if context else "entry"}
        self.engine.reconcile({"op": self.op(), "agent": "r1", "consumer": "c1", "records": []}, OWNER)
        self.resume()

    def test_take_is_durable_and_replay_never_returns_content(self):
        self.setup_pull()
        self.send("boundary")
        args = {"op": self.op(), "agent": "r1", "consumer": "c1", "at": "turn", "limit": 16}
        first = self.engine.take(args, OWNER)
        self.assertEqual(len(first["records"]), 1)
        delivery = first["records"][0]["delivery"]
        self.assertEqual(self.engine.get(delivery)["state"], "claimed")
        second = self.engine.take(args, OWNER)
        self.assertEqual(second["records"], [])
        self.assertEqual(second["claims"], first["claims"])
        self.assertEqual(self.ledger, [])

    def test_pull_report_advances_and_old_consumer_is_fenced(self):
        self.setup_pull()
        self.send("boundary")
        taken = self.engine.take({"op": self.op(), "agent": "r1", "consumer": "c1", "at": "turn"}, OWNER)["records"][0]
        self.engine.report({"op": self.op(), "delivery": taken["delivery"], "claim": taken["claim"], "consumer": "c1", "stage": "inserted"}, OWNER)
        self.assertEqual(self.engine.get(taken["delivery"])["state"], "inserted")
        with self.assertRaises(PolicyError):
            self.engine.report({"op": self.op(), "delivery": taken["delivery"], "claim": taken["claim"], "consumer": "old", "stage": "read"}, OWNER)

    def test_context_pull_needs_opted_in_rendering_and_supplied_not_inserted(self):
        self.setup_pull(context=True)
        self.send("boundary")
        first = self.engine.take({"op": self.op(), "agent": "r1", "consumer": "c1", "at": "turn"}, OWNER)
        self.assertEqual(first["records"], [])
        sent = self.send("boundary", send_new=True, render=["context"])
        taken = self.engine.take({"op": self.op(), "agent": "r1", "consumer": "c1", "at": "turn"}, OWNER)["records"][0]
        with self.assertRaises(PolicyError):
            self.engine.report({"op": self.op(), "delivery": sent["delivery"], "claim": taken["claim"], "stage": "inserted"}, OWNER)
        self.engine.report({"op": self.op(), "delivery": sent["delivery"], "claim": taken["claim"], "stage": "supplied"}, OWNER)
        self.assertEqual(self.engine.get(sent["delivery"])["state"], "supplied")

    def test_restart_reconcile_without_entry_is_unknown(self):
        self.setup_pull()
        sent = self.send("boundary")
        self.engine.take({"op": self.op(), "agent": "r1", "consumer": "c1", "at": "turn"}, OWNER)
        self.engine.reconcile({"op": self.op(), "agent": "r1", "consumer": "c2", "records": []}, OWNER)
        self.assertEqual(self.engine.get(sent["delivery"])["state"], "unknown")

    def test_resolve_never_invokes(self):
        self.setup_pull()
        sent = self.send("boundary")
        self.engine.take({"op": self.op(), "agent": "r1", "consumer": "c1", "at": "turn"}, OWNER)
        self.engine.reconcile({"op": self.op(), "agent": "r1", "consumer": "c2", "records": []}, OWNER)
        self.engine.resolve({"op": self.op(), "delivery": sent["delivery"], "as": "not_delivered"}, OWNER)
        self.assertEqual(self.engine.get(sent["delivery"])["state"], "resolved")
        self.assertEqual(self.ledger, [])

    def test_fenced_owner_callback_does_not_invoke(self):
        self.send()
        self.resume()
        self.engine.fence()
        self.assertIsNone(self.engine.dispatch_one())
        self.assertEqual(self.ledger, [])

    def test_expired_submission_lookup_precedes_dynamic_validation(self):
        op = self.op()
        args = {"op": op, "submission": op, "to": "r1", "mode": "boundary", "expires_at_ms": 2000,
                "content": [{"type": "text", "text": "once"}]}
        sent = self.engine.send(args, OWNER)
        self.now = 3000
        repeated = self.engine.send(dict(args, op=self.op()), OWNER)
        self.assertEqual(repeated["delivery"], sent["delivery"])
        self.assertEqual(len(self.engine.records()), 1)

    def test_control_is_available_while_inbox_is_paused_and_replay_never_invokes(self):
        self.send()
        args = {"op": self.op(), "target": "r1", "action": "interrupt", "args": {}}
        result = self.engine.control(args, OWNER)
        self.assertEqual(result["state"], "accepted")
        self.assertEqual(len(self.ledger), 1)
        self.engine.control(args, OWNER)
        self.assertEqual(len(self.ledger), 1)
        self.assertEqual(self.engine.permits, set())

    def test_retire_is_administrative_not_end_evidence(self):
        sent = self.send()
        self.resume()
        self.engine.control({"op": self.op(), "target": "r1", "action": "retire", "args": {"reason": "manual"}}, OWNER)
        self.assertEqual(self.ledger, [])
        self.assertEqual(self.engine.tombstones()["r1"]["lifecycle"], "retired")
        self.assertEqual(self.resources["r1"]["data"]["agent"]["lifecycle"], "live")
        self.assertEqual(self.engine.get(sent["delivery"])["state"], "held")

    def test_handover_pending_cannot_dispatch_until_complete_and_never_opens_gate(self):
        sent = self.send()
        self.resources["r1"]["data"]["agent"]["lifecycle"] = "ended"
        self.resources["r2"] = resource("r2")
        self.resources["r2"]["data"]["agent"]["session"] = {"id": "s2"}
        self.resume()
        transferred = self.engine.handover({"op": self.op(), "from": {"agent": "r1", "incarnation": "i1"},
                                            "to": {"agent": "r2", "incarnation": "i1"}, "deliveries": [sent["delivery"]]}, OWNER)
        self.assertEqual(self.engine.get(sent["delivery"])["reason"], "handover_pending")
        self.engine.dispatch_one("r2")
        self.assertEqual(self.ledger, [])
        self.engine.handover({"op": self.op(), "action": "complete", "plan": transferred["plan"]}, OWNER)
        self.engine.dispatch_one("r2")
        self.assertEqual(len(self.ledger), 1)
        self.assertEqual(self.ledger[0][0], "r2")
        self.assertEqual(self.ledger[0][2]["guard"]["session"], "s2")
        self.assertEqual(self.engine.get(sent["delivery"])["handed_over_from"]["agent"], "r1")

    def test_native_end_evidence_supersedes_an_administrative_retirement(self):
        self.send()
        self.engine.control({"op": self.op(), "target": "r1", "action": "retire", "args": {}}, OWNER)
        self.resources["r1"]["data"]["agent"]["lifecycle"] = "ended"
        self.engine.lifecycle_sweep()
        self.assertEqual(self.engine.tombstones()["r1"]["lifecycle"], "ended")
        self.assertEqual(self.engine.tombstones()["r1"]["last_evidence"], "native_ended")

    def test_live_agent_cannot_be_silently_handed_over(self):
        sent = self.send()
        self.resources["r2"] = resource("r2")
        with self.assertRaises(PolicyError):
            self.engine.handover({"op": self.op(), "from": {"agent": "r1", "incarnation": "i1"},
                                  "to": {"agent": "r2", "incarnation": "i1"}, "deliveries": [sent["delivery"]]}, OWNER)
        self.assertEqual(self.engine.get(sent["delivery"])["to"]["agent"], "r1")

    def test_native_claim_cannot_be_released_by_null_consumer(self):
        sent = self.send()
        self.resume()
        def try_release(target, op, args, deadline):
            self.ledger.append((target, op, args))
            claim = self.engine.get(sent["delivery"])["attempt"]["claim"]
            with self.assertRaises(PolicyError) as caught:
                self.engine.report({"op": self.op(), "delivery": sent["delivery"], "claim": claim, "stage": "released"}, OWNER)
            self.assertEqual(caught.exception.kind, "conflict")
            return {"outcome": "accepted", "data": {"receipt": {"stage": "admitted"}}}
        self.engine.invoke = try_release
        self.engine.dispatch_one()
        self.engine.dispatch_one()
        self.assertEqual(len(self.ledger), 1)

    def test_unknown_predecessor_blocks_only_same_sender_target_lane(self):
        first = self.send("start")
        second = self.send("start", send_new=True)
        boundary = self.send("boundary", send_new=True)
        row = self.store.get("message/" + first["delivery"])
        value = row["value"]
        value["state"] = "unknown"
        self.store.apply([{"op": "set", "key": "message/" + first["delivery"], "value": value, "expect": row["version"]}])
        self.resume()
        self.engine.dispatch_one()
        self.assertEqual(self.ledger[0][2]["delivery"], boundary["delivery"])
        self.engine.dispatch_one()
        self.assertEqual(len(self.ledger), 1)
        self.assertEqual(self.engine.get(second["delivery"])["reason"], "ordering_unknown")

    def test_large_native_result_is_live_unchanged_but_only_compact_evidence_persists(self):
        sent = self.send()
        self.resume()
        native = {"outcome": "accepted", "data": {"receipt": {"stage": "admitted"}, "large": "x" * 100000}}
        self.engine.invoke = lambda *args: copy.deepcopy(native)
        reply = self.engine.dispatch_one()
        self.assertEqual(reply["native"], native)
        persisted = self.engine.get(sent["delivery"])
        self.assertTrue(persisted["result"]["native"]["incomplete"])
        self.assertEqual(persisted["result"]["native"]["bytes"], __import__("coordination_store").encoded(native))
        self.assertNotIn("large", str(persisted["result"]))
        self.assertLess(__import__("coordination_store").encoded(persisted["result"]["native"]), 1024)
        self.assertEqual(len(persisted["result"]["native"]["sha256"]), 64)

    def test_control_large_native_result_replay_is_explicitly_compact(self):
        native = {"outcome": "accepted", "data": {"large": "q" * 50000}}
        self.engine.invoke = lambda *args: copy.deepcopy(native)
        args = {"op": self.op(), "target": "r1", "action": "interrupt", "args": {}}
        first = self.engine.control(args, OWNER)
        self.assertEqual(first["native"], native)
        replay = self.engine.control(args, OWNER)
        self.assertTrue(replay["native"]["incomplete"])
        self.assertNotIn("large", str(replay["native"]))

    def test_pinned_session_switch_at_claim_is_held_not_retargeted(self):
        sent = self.send()
        self.resume()
        calls = 0
        def switching():
            nonlocal calls
            calls += 1
            facts = copy.deepcopy(self.resources)
            if calls >= 3:
                facts["r1"]["data"]["agent"]["session"]["id"] = "s2"
            return facts
        self.engine.resources = switching
        self.engine.dispatch_one()
        self.assertEqual(self.ledger, [])
        self.assertEqual(self.engine.get(sent["delivery"])["to"]["session"], "s1")
        self.assertEqual(self.engine.get(sent["delivery"])["reason"], "session_changed")

    def test_take_without_a_safe_point_is_rejected(self):
        self.setup_pull()
        sent = self.send("boundary")
        for at in (None, "not-a-point"):
            with self.assertRaises(PolicyError) as caught:
                self.engine.take({"op": self.op(), "agent": "r1", "consumer": "c1", "at": at}, OWNER)
            self.assertEqual(caught.exception.kind, "invalid")
        self.assertEqual(self.engine.get(sent["delivery"])["state"], "held")

    def test_growth_edits_cannot_bypass_sender_target_quota(self):
        from coordination_policy import record_charge
        deliveries = [self.send(send_new=True)["delivery"] for _ in range(5)]
        refused = 0
        for delivery in deliveries:
            row = self.engine.get(delivery)
            try:
                self.engine.edit({"op": self.op(), "delivery": delivery, "expect": row["revision"],
                                  "content": [{"type": "text", "text": "x" * 30000}]}, OWNER)
            except PolicyError as error:
                self.assertEqual(error.kind, "quota")
                refused += 1
        self.assertGreater(refused, 0)
        self.assertLessEqual(sum(record_charge(record) for record in self.engine.records()), 128 * 1024)

    def test_pull_round_robins_senders_not_global_fifo(self):
        self.setup_pull()
        self.config["doc/agents/grants/worker"] = {"version": 1, "value": {"who": WORKER["principal"], "can": ["send:boundary"], "on": "agent:r1"}}
        self.send("boundary")
        self.send("boundary", send_new=True)
        op = self.op()
        self.engine.send({"op": op, "submission": op, "to": "r1", "mode": "boundary", "content": [{"type": "text", "text": "worker"}]}, WORKER)
        selected = []
        for _ in range(2):
            row = self.engine.take({"op": self.op(), "agent": "r1", "consumer": "c1", "at": "turn", "limit": 1}, OWNER)["records"][0]
            selected.append(row["from"]["principal"])
            self.engine.report({"op": self.op(), "delivery": row["delivery"], "claim": row["claim"], "stage": "inserted"}, OWNER)
        self.assertEqual(set(selected), {OWNER["principal"], WORKER["principal"]})

    def test_clean_failed_pause_does_not_change_live_or_durable_gates(self):
        self.resume()
        before = copy.deepcopy(self.store.get("control"))
        self.store.fault = "before"
        with self.assertRaises(PolicyError) as caught:
            self.resume(action="pause")
        self.assertEqual(caught.exception.kind, "io")
        self.assertEqual(self.engine.permits, {"r1"})
        self.assertEqual(self.store.get("control"), before)
        self.store.fault = "after"
        with self.assertRaises(StoreError):
            self.resume(action="pause")
        self.assertEqual(self.engine.permits, set())

    def test_archive_verified_export_then_free_returns_locator_without_redelivery(self):
        op = self.op()
        args = {"op": op, "submission": op, "to": "r1", "mode": "start", "content": [{"type": "text", "text": "archive evidence"}]}
        sent = self.engine.send(args, OWNER)
        with tempfile.TemporaryDirectory(prefix="coord-archive-") as directory:
            plan = self.engine.archive({"op": self.op(), "agents": ["r1"], "destination": directory}, OWNER)
            archive = plan["archive"]
            self.engine.archive({"op": self.op(), "action": "export", "archive": archive}, OWNER)
            self.engine.archive({"op": self.op(), "action": "free", "archive": archive}, OWNER)
            lookup = self.engine.get(sent["delivery"])
            self.assertEqual(lookup["state"], "archived")
            repeated = self.engine.send(dict(args, op=self.op()), OWNER)
            self.assertEqual(repeated["delivery"], sent["delivery"])
            self.assertEqual(self.engine.records(), [])
            Path(lookup["locator"]).unlink()
            self.assertEqual(self.engine.get(sent["delivery"])["state"], "unknown")
        self.assertEqual(self.ledger, [])

    def test_archive_late_evidence_change_keeps_original_record(self):
        sent = self.send()
        with tempfile.TemporaryDirectory(prefix="coord-archive-") as directory:
            archive = self.engine.archive({"op": self.op(), "agents": ["r1"], "destination": directory}, OWNER)["archive"]
            self.engine.archive({"op": self.op(), "action": "export", "archive": archive}, OWNER)
            key = "message/" + sent["delivery"]
            row = self.store.get(key)
            changed = dict(row["value"], reason="new evidence arrived")
            self.store.apply([{"op": "set", "key": key, "value": changed, "expect": row["version"]}], control=True)
            result = self.engine.archive({"op": self.op(), "action": "free", "archive": archive}, OWNER)
            self.assertEqual(result["state"], "partial")
            self.assertEqual(result["changed"], [key])
            self.assertIsNotNone(self.store.get(key))

    def test_200_member_scope_is_atomic_with_frozen_paged_results(self):
        self.resources = {"r" + str(n): resource("r" + str(n)) for n in range(1, 201)}
        result = self.resume()
        self.assertEqual(result["selected"], 200)
        self.assertEqual(len(result["targets"]), 64)
        self.assertEqual(len(self.engine.permits), 200)
        all_targets = list(result["targets"])
        cursor = result["next"]
        self.resources["r999"] = resource("r999")
        while cursor:
            page = self.engine.scope_result(result["receipt"], cursor)
            self.assertLessEqual(len(page["targets"]), 64)
            all_targets += page["targets"]
            cursor = page["next"]
        self.assertEqual(len(set(all_targets)), 200)
        self.assertNotIn("r999", all_targets)
        self.assertNotIn("r999", self.engine.permits)

    def test_unassigned_endpoint_added_after_preview_invalidates_scope_cut(self):
        args = {"op": self.op(), "action": "resume", "scope": "workspace", "expect": self.engine.scope_preview("workspace")["expect"]}
        self.resources["r2"] = resource("r2")
        with self.assertRaises(PolicyError) as caught:
            self.engine.scope(args, OWNER)
        self.assertEqual(caught.exception.kind, "conflict")
        self.assertEqual(self.engine.permits, set())

    def test_settled_record_gc_never_recreates_a_current_window_submission(self):
        op = self.op()
        args = {"op": op, "submission": op, "to": "r1", "mode": "start", "content": [{"type": "text", "text": "first"}]}
        first = self.engine.send(args, OWNER)
        self.resume()
        self.engine.dispatch_one()
        for _ in range(270):
            self.send()
            self.engine.dispatch_one()
        self.assertIsNone(self.store.get("message/" + first["delivery"]))
        self.assertEqual(self.engine.get(first["delivery"])["state"], "accepted")
        count = len(self.ledger)
        repeated = self.engine.send(dict(args, op=self.op()), OWNER)
        self.assertEqual(repeated["delivery"], first["delivery"])
        self.engine.dispatch_one()
        self.assertEqual(len(self.ledger), count)
        self.assertLessEqual(len(self.engine.records()), 256)

    def test_window_rollover_and_control_history_do_not_disable_mutations(self):
        old_op = self.op()
        original = {"op": old_op, "target": "r1", "action": "interrupt", "args": {}}
        self.engine.control(original, OWNER)
        for _ in range(1030):
            self.engine.control({"op": self.op(), "target": "r1", "action": "interrupt", "args": {}}, OWNER)
        before = len(self.ledger)
        try:
            self.engine.control(original, OWNER)
        except PolicyError as error:
            self.assertEqual(error.kind, "expired_operation")
        self.assertEqual(len(self.ledger), before)
        externals = [key for key in self.store.snapshot() if key.startswith("external/")]
        self.assertLessEqual(len(externals), 256)
        self.assertNotEqual(self.engine.window()["epoch"], old_op.rsplit(":", 1)[0])

    def test_late_compact_result_fits_after_admission_flood(self):
        first = self.send(content=[{"type": "text", "text": "x" * 30000}])
        self.resume()
        native = {"outcome": "accepted", "data": {"receipt": {"stage": "admitted"}, "large": "n" * 100000}}
        def flooded(*args):
            for _ in range(16):
                try:
                    self.send(send_new=True, content=[{"type": "text", "text": "f" * 15000}])
                except PolicyError as error:
                    self.assertEqual(error.kind, "quota")
                    break
            return native
        self.engine.invoke = flooded
        result = self.engine.dispatch_one()
        self.assertEqual(result["native"], native)
        record = self.engine.get(first["delivery"])
        self.assertEqual(record["state"], "accepted")
        self.assertTrue(record["result"]["native"]["incomplete"])
        self.assertIsNone(self.engine.maintenance_error)

    def test_silence_becomes_unavailable_never_ended(self):
        self.send()
        self.resources["r1"]["available"] = False
        self.engine.lifecycle_sweep()
        self.assertEqual(self.engine.tombstones()["r1"]["lifecycle"], "disconnected")
        self.now += 60001
        forgotten = self.engine.lifecycle_sweep()
        self.assertEqual(forgotten, [("r1", "i1")])
        self.assertEqual(self.engine.tombstones()["r1"]["lifecycle"], "unavailable")


if __name__ == "__main__":
    unittest.main()
