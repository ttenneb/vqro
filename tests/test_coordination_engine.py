"""State-machine evidence against a fake native ledger, not runtime certification."""
import copy
from pathlib import Path
import sys
import threading
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
        self.resources["r1"]["data"]["agent"]["pull"] = {"points": ["turn", "settle", "idle", "tool"],
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


if __name__ == "__main__":
    unittest.main()
