"""Decision-policy characterization, not real host/caller acceptance."""
import datetime as dt
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock
from provider_test import FaultCommit
import provider

H = {"class": "human_unverified", "terminal_id": "human", "uid": 1000}
A = {"class": "agent", "terminal_id": "term_a", "agent": "pi", "execution": "boot:1:10"}
NOW = "2026-10-02T16:00:00.000+00:00"
LATER = "2026-10-02T16:00:10.000+00:00"
FIELDS = {"options": [{"key": "keep", "label": "Keep"}, {"key": "change", "label": "Change"}], "default": "keep", "reversible": True, "waiting_cost": "Work waits.", "auto_at": LATER}


class DecisionTests(unittest.TestCase):
    def setUp(self):
        self.root = tempfile.TemporaryDirectory()
        self.store = provider.Store(self.root.name, decision_delay_seconds=5)
        self.seq = 0
        self.clock = mock.patch.object(provider, "timestamp", return_value=NOW)
        self.clock.start()

    def tearDown(self):
        self.clock.stop()
        try:
            self.store.db.close()
        except Exception:
            pass
        self.root.cleanup()

    def call(self, action, caller=A, **fields):
        self.seq += 1
        return self.store.invoke({"caller": caller, "payload": {"action": action, **({"operation_id": str(self.seq)} if action not in ("get", "status", "list", "decision_log", "decision_config") else {}), **fields}})

    def create(self, **fields):
        return self.call("decision", title="Keep result?", done_when="Question answered.", **{**FIELDS, **fields})["task"]

    def auto(self, at=LATER):
        return self.store.decisions.apply_due(at)

    def test_floor_irreversible_and_structured_field_bounds(self):
        for fields in ({"auto_at": NOW}, {"reversible": False}, {"options": []}, {"default": "missing"}, {"reversible": 1}, {"waiting_cost": "x" * 513}):
            with self.subTest(fields=fields), self.assertRaises(ValueError):
                self.create(**fields)
        self.assertEqual({"revision": 0}, self.call("status"))
        task = self.create(reversible=False, auto_at=None)
        self.assertEqual(0, self.auto())
        self.assertIsNone(task["decision"]["auto_at"])
        self.assertEqual(5, self.call("decision_config")["delay_floor_seconds"])

    def test_exact_policy_log_and_receipt_atomic_once_after_restart(self):
        task = self.create()
        self.assertEqual(0, self.auto(NOW))
        self.assertEqual(1, self.auto())
        answered = self.store.task(task["id"])
        self.assertEqual("done", answered["state"])
        self.assertEqual("keep", answered["decision"]["resolution"]["option"])
        self.assertEqual(A, answered["decision"]["resolution"]["filer"])
        self.assertEqual("decision_policy", answered["last_transition"]["caller"]["class"])
        self.assertIsNone(answered["decision"]["auto_at"])
        self.assertEqual(1, len(self.call("decision_log")["events"]))
        self.assertEqual(2, self.store.db.execute("SELECT COUNT(*) FROM operations").fetchone()[0])
        self.store.db.close()
        self.store = provider.Store(self.root.name, 5)
        self.assertEqual(0, self.auto())
        self.assertEqual(answered["decision"], self.store.task(task["id"])["decision"])

    def test_human_answer_wins_and_is_not_revertible(self):
        task = self.create()
        result = self.call("answer", caller=H, id=task["id"], revision=task["revision"], option="change", text="Human chose.")["task"]
        self.assertEqual("human", result["decision"]["resolution"]["by"])
        self.assertEqual(0, self.auto())
        with self.assertRaisesRegex(ValueError, "policy answer"):
            self.call("decision_revert", caller=H, id=task["id"], revision=result["revision"])

    def test_policy_wins_stale_answer_rejected_revert_cancels_and_replay_is_exact(self):
        task = self.create()
        self.auto()
        with self.assertRaisesRegex(ValueError, "stale"):
            self.call("answer", caller=H, id=task["id"], revision=1, option="change")
        payload = {"action": "decision_revert", "id": task["id"], "revision": 2, "operation_id": "revert-exact"}
        result = self.store.invoke({"caller": H, "payload": payload})
        self.assertEqual("open", result["task"]["state"])
        self.assertIsNone(result["task"]["decision"]["auto_at"])
        self.assertEqual(0, self.auto())
        self.assertEqual(result, self.store.invoke({"caller": H, "payload": payload}))
        with self.assertRaisesRegex(ValueError, "policy answer"):
            self.call("decision_revert", caller=H, id=task["id"], revision=3)
        self.assertEqual(["auto", "revert"], [event["kind"] for event in self.call("decision_log")["events"]])
        scheduled = self.call("decision_timer", id=task["id"], revision=3, auto_at=LATER)["task"]
        self.assertEqual(result, self.store.invoke({"caller": H, "payload": payload}))
        self.assertEqual(scheduled["decision"], self.store.task(task["id"])["decision"])
        self.assertEqual(1, self.auto())

    def test_reschedule_exact_filer_and_every_time_floor_after_revert(self):
        task = self.create()
        with self.assertRaisesRegex(ValueError, "original filer"):
            self.call("decision_timer", caller={**A, "execution": "replacement"}, id=task["id"], revision=1, auto_at=LATER)
        with self.assertRaisesRegex(ValueError, "human window"):
            self.call("decision_timer", id=task["id"], revision=1, auto_at=NOW)
        stopped = self.call("decision_timer", id=task["id"], revision=1, auto_at=None)["task"]
        self.assertEqual(0, self.auto())
        scheduled = self.call("decision_timer", id=task["id"], revision=stopped["revision"], auto_at=LATER)["task"]
        self.assertTrue(scheduled["decision"]["timer_current"])
        self.assertEqual(1, self.auto())

    def test_other_metadata_edits_cancel_timer_without_implicit_terms_change(self):
        task = self.create()
        changed = self.call("update", id=task["id"], revision=1, brief_path="brief.md")["task"]
        self.assertIsNone(changed["decision"]["auto_at"])
        self.assertEqual(0, self.auto())
        with self.assertRaisesRegex(ValueError, "unknown action or field"):
            self.call("update", id=task["id"], revision=2, default="change")

    def test_revert_protects_completed_dependents_and_parent_invariants(self):
        decision = self.create()
        dependent = self.call("create", caller=H, title="dependent", done_when="Proof.", depends_on=[decision["id"]])["task"]
        self.auto()
        dependent = self.call("start", caller=H, id=dependent["id"], revision=1)["task"]
        reverted = self.call("decision_revert", caller=H, id=decision["id"], revision=2)["task"]
        with self.assertRaisesRegex(ValueError, "unfinished dependencies"):
            self.call("report", caller=H, id=dependent["id"], revision=dependent["revision"], text="> Proof.\nEvidence.")
        with mock.patch.object(provider, "timestamp", return_value="2026-10-02T16:00:01.000+00:00"):
            self.call("decision_timer", id=decision["id"], revision=reverted["revision"], auto_at=LATER)
        self.auto()
        dependent = self.call("report", caller=H, id=dependent["id"], revision=dependent["revision"], text="> Proof.\nEvidence.")["task"]
        with self.assertRaisesRegex(ValueError, dependent["id"]):
            self.call("decision_revert", caller=H, id=decision["id"], revision=self.store.task(decision["id"])["revision"])

    def test_revert_rejects_reported_parent_without_reopening_child(self):
        parent = self.call("create", caller=H, title="parent", done_when="Proof.")["task"]
        decision = self.create(parent=parent["id"], caller=H)
        self.auto()
        parent = self.call("start", caller=H, id=parent["id"], revision=1)["task"]
        self.call("report", caller=H, id=parent["id"], revision=parent["revision"], text="> Proof.\nEvidence.")
        with self.assertRaisesRegex(ValueError, parent["id"]):
            self.call("decision_revert", caller=H, id=decision["id"], revision=2)
        self.assertEqual("done", self.store.task(decision["id"])["state"])

    def test_policy_commit_uncertainty_is_not_relabelled_rejection_or_retried(self):
        task = self.create()
        self.store.db = FaultCommit(self.store.db, after=True, close_error=True)
        with self.assertRaises(provider.OutcomeUnknown):
            self.auto()
        self.store = provider.Store(self.root.name, 5)
        self.assertEqual("done", self.store.task(task["id"])["state"])
        self.assertEqual(0, self.auto())
        self.assertEqual(1, len(self.call("decision_log")["events"]))

    def test_open_prerequisite_prevents_auto_apply_until_ready(self):
        prior = self.call("decision", title="Prior?", done_when="Answered.")["task"]
        waiting = self.create(depends_on=[prior["id"]])
        self.assertEqual(0, self.auto())
        self.call("answer", caller=H, id=prior["id"], revision=1, text="Settled.")
        self.assertEqual(1, self.auto())
        self.assertEqual("done", self.store.task(waiting["id"])["state"])

    def test_log_and_list_remain_byte_bounded_with_large_notes_and_options(self):
        for _ in range(32):
            task = self.create(auto_at=None, waiting_cost="w" * 512, options=[{"key": str(i), "label": "l" * 256} for i in range(4)], default="0")
            self.call("answer", caller=H, id=task["id"], revision=1, option="1", text="n" * 4096)
        log = self.call("decision_log")
        self.assertLessEqual(len(provider.canonical(log).encode()), provider.MAX_REPLY_BYTES)
        self.assertIsNotNone(log["next_cursor"])
        listed = self.call("list")
        self.assertLessEqual(len(provider.canonical(listed).encode()), provider.MAX_REPLY_BYTES)
        self.assertIsNotNone(listed["next_cursor"])

    def test_legacy_has_no_invented_timer_and_unknown_cannot_mutate_or_spoof_policy(self):
        task = self.call("decision", title="Legacy?", done_when="Answered.")["task"]
        self.assertIsNone(task["decision"])
        self.assertEqual(0, self.auto())
        for caller in ({"class": "unknown"}, {"class": "decision_policy", "decision_id": task["id"], "revision": 1}):
            with self.assertRaisesRegex(ValueError, "unknown caller"):
                self.call("answer", caller=caller, id=task["id"], revision=1, text="spoof")
        self.assertEqual("done", self.call("answer", caller=H, id=task["id"], revision=1, text="Legacy answer")["task"]["state"])

    def test_idle_overdue_deferral_backs_off_but_future_deadline_is_not_starved(self):
        task = self.create()
        self.assertEqual(30, self.store.decisions.next_delay(LATER))
        self.assertEqual(1, self.store.decisions.next_delay(LATER, applied=8))
        self.create(auto_at="2026-10-02T16:00:15.000+00:00")
        self.assertEqual(5, self.store.decisions.next_delay(LATER))

    def test_late_policy_time_is_recorded_not_fabricated_on_time(self):
        task = self.create()
        self.auto("2026-10-03T16:00:10.000+00:00")
        self.assertEqual(86400000, self.store.task(task["id"])["decision"]["resolution"]["late_ms"])

    def test_worker_deadline_fires_without_pane_poll_or_another_request(self):
        # Protocol-only fixture caller, NOT kernel-caller acceptance. Actual host is spike demo.
        with tempfile.TemporaryDirectory() as directory:
            process = subprocess.Popen([sys.executable, "-B", str(Path(provider.__file__)), "--data-dir", directory, "--decision-delay-seconds", "1"], stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            try:
                self.assertEqual("hello", json.loads(process.stdout.readline())["type"])
                process.stdin.write(json.dumps({"type": "welcome", "granted_capabilities": ["action_invoke"], "methods": ["action.invoke.v1"]}) + "\n")
                deadline = (dt.datetime.now(dt.timezone.utc) + dt.timedelta(seconds=2)).isoformat(timespec="milliseconds")
                process.stdin.write(json.dumps({"type": "request", "id": "one", "method": "action.invoke.v1", "params": {"caller": A, "payload": {"action": "decision", "operation_id": "one", "title": "Idle timer", "done_when": "Answered.", **FIELDS, "auto_at": deadline}}}) + "\n")
                process.stdin.flush()
                task = json.loads(process.stdout.readline())["result"]["value"]["task"]
                import time
                time.sleep(3)
                # Separate readonly Store inspection confirms commit BEFORE sending second request.
                inspected = provider.Store(directory, 1)
                self.assertEqual("done", inspected.task(task["id"])["state"])
                inspected.db.close()
            finally:
                process.stdin.close()
                process.wait(timeout=5)
                process.stdout.close(); process.stderr.close()


if __name__ == "__main__":
    unittest.main()
