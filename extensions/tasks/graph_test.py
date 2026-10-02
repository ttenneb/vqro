"""Graph, evidence, exact acceptance and whole-transaction characterization."""
import json
import tempfile
import threading
import time
import unittest
from concurrent.futures import ThreadPoolExecutor
from unittest import mock

import graph
import provider
from provider_test import A, B, HUMAN, UNKNOWN, FaultCommit


class GraphTests(unittest.TestCase):
    def setUp(self):
        self.root = tempfile.TemporaryDirectory()
        self.store = provider.Store(self.root.name)
        self.sequence = 0
        self.parent = self.call("create", caller=HUMAN, title="Approved work", done_when="Work is verified.")["task"]
        self.parent = self.change(self.parent, "delegate", caller=HUMAN, owner=A)
        self.parent = self.change(self.parent, "start")

    def tearDown(self):
        self.store.db.close()
        self.root.cleanup()

    def call(self, action, caller=A, **values):
        self.sequence += 1
        payload = {"action": action, **values}
        if action not in ("get", "list", "ready", "whoami", "op"):
            payload.setdefault("operation_id", f"graph-{self.sequence}")
        return self.store.invoke({"caller": caller, "payload": payload})

    def change(self, task, action, caller=A, **values):
        return self.call(action, caller=caller, id=task["id"], revision=task["revision"], **values)["task"]

    def child(self, **values):
        return self.call("create", parent=self.parent["id"], title="Child work", done_when="Result is visible.", **values)["task"]

    def counts(self):
        return [self.store.db.execute(f"SELECT COUNT(*) FROM {table}").fetchone()[0] for table in ("tasks", "transitions", "operations", "dependencies")]

    def report(self, task, caller=B):
        return self.change(task, "report", caller=caller, text="> " + task["done_when"] + "\nEvidence: checked the visible result.")

    def test_dependent_waits_for_accepted_not_reported_and_never_starts_early(self):
        first = self.change(self.child(), "delegate", owner=B)
        second = self.child(depends_on=[first["id"]])
        with self.assertRaisesRegex(ValueError, "dependencies"):
            self.change(second, "start")
        first = self.report(first)
        self.assertNotIn(second["id"], [task["id"] for task in self.call("ready")["tasks"]])
        first = self.change(first, "accept")
        self.assertEqual("accepted", first["state"])
        self.assertIn(second["id"], [task["id"] for task in self.call("ready")["tasks"]])
        started = self.change(second, "start")
        self.assertEqual([], started["waiting_on"])

    def test_self_owned_step_direct_done_requires_start_and_each_criterion_evidence(self):
        task = self.child()
        with self.assertRaisesRegex(ValueError, "not allowed"):
            self.change(task, "done", text="> Result is visible.\nVerified")
        task = self.change(task, "start")
        with self.assertRaisesRegex(ValueError, "independent acceptor"):
            self.report(task, caller=A)
        with self.assertRaisesRegex(ValueError, "quote"):
            self.change(task, "done", text="Trust me")
        with self.assertRaisesRegex(ValueError, "not allowed"):
            self.change(task, "done", caller=HUMAN, text="> Result is visible.\nEvidence: not the exact owner")
        completed = self.change(task, "done", text="> Result is visible.\nEvidence: result.txt contains PASS")
        self.assertEqual("done", completed["state"])
        self.assertIsNone(completed["owed_to"])
        self.assertIn("result.txt", completed["detail"])
        delegated = self.change(self.child(), "delegate", owner=B)
        delegated = self.change(delegated, "start", caller=B)
        with self.assertRaisesRegex(ValueError, "not allowed"):
            self.change(delegated, "done", caller=B, text="> Result is visible.\nVerified")

    def test_accept_authority_no_self_accept_replacement_and_parent_recovery(self):
        child = self.change(self.child(), "delegate", owner=B)
        child = self.report(child)
        with self.assertRaisesRegex(ValueError, "own work"):
            self.change(child, "accept", caller=B)
        replacement = {**A, "execution": "boot:11:101"}
        with self.assertRaisesRegex(ValueError, "acceptor"):
            self.change(child, "accept", caller=replacement)
        self.parent = self.change(self.parent, "delegate", caller=HUMAN, owner=replacement)
        accepted = self.change(child, "accept", caller=replacement)
        self.assertEqual("accepted", accepted["state"])
        # Even a delegator/parent-role match does not grant agent self-acceptance.
        other = self.call("create", caller=HUMAN, parent=self.parent["id"], title="shared", done_when="Visible.")["task"]
        other = self.change(other, "delegate", caller=HUMAN, owner=B)
        other = self.change(other, "delegate", caller=B, owner=B)
        other = self.report(other)
        with self.assertRaisesRegex(ValueError, "own work"):
            self.change(other, "accept", caller=B)

    def test_own_step_takeover_preserves_status_but_cannot_erase_handoff_oversight(self):
        own = self.child()
        replacement = {**A, "execution": "boot:11:101"}
        self.parent = self.change(self.parent, "delegate", caller=HUMAN, owner=replacement)
        own = self.change(own, "delegate", caller=replacement, owner=replacement)
        self.assertIsNone(own["delegator"])
        own = self.change(own, "start", caller=replacement)
        self.assertEqual("done", self.change(own, "done", caller=replacement, text="> Result is visible.\nEvidence: verified anew")["state"])
        shared = self.call("create", caller=HUMAN, title="Shared", parent=self.parent["id"], done_when="Visible.")["task"]
        shared = self.change(shared, "delegate", caller=HUMAN, owner=replacement)
        shared = self.change(shared, "delegate", caller=replacement, owner=replacement)
        self.assertEqual(HUMAN, shared["delegator"])
        shared = self.change(shared, "start", caller=replacement)
        with self.assertRaisesRegex(ValueError, "not allowed"):
            self.change(shared, "done", caller=replacement, text="> Visible.\nEvidence: checked")

    def test_reported_stale_delegator_child_recovers_via_active_parent_not_child(self):
        child = self.change(self.child(), "delegate", owner=B)
        replacement = {**A, "execution": "boot:11:101"}
        self.parent = self.change(self.parent, "delegate", caller=HUMAN, owner=replacement)
        child = self.change(child, "delegate", caller=replacement, owner=replacement)
        child = self.report(child, caller=replacement)
        self.assertEqual([A], child["acceptors"])
        with self.assertRaisesRegex(ValueError, "not allowed"):
            self.change(child, "delegate", caller=HUMAN, owner=replacement)
        with self.assertRaisesRegex(ValueError, "acceptor"):
            self.change(child, "accept", caller=HUMAN)
        self.parent = self.change(self.parent, "delegate", caller=HUMAN, owner=HUMAN)
        child = self.change(child, "accept", caller=HUMAN)
        self.assertEqual("accepted", child["state"])
        self.assertEqual(A, child["delegator"])
        self.assertEqual(replacement, child["owner"])

    def test_human_uid_authorization_does_not_normalize_receipt_scope(self):
        root = self.call("create", caller=HUMAN, title="root", done_when="Visible.")["task"]
        root = self.change(root, "delegate", caller=HUMAN, owner=B)
        root = self.report(root)
        different_uid = {**HUMAN, "uid": HUMAN["uid"] + 1}
        with self.assertRaisesRegex(ValueError, "acceptor"):
            self.change(root, "accept", caller=different_uid)
        another_pane = {**HUMAN, "terminal_id": "other-human-pane"}
        accepted = self.call("accept", caller=another_pane, id=root["id"], revision=root["revision"], operation_id="human-accept")
        self.assertEqual("accepted", accepted["task"]["state"])
        self.assertFalse(self.call("op", caller=HUMAN, operation_id="human-accept")["found"])
        self.assertTrue(self.call("op", caller=UNKNOWN, operation_id="human-accept", lookup_caller=another_pane)["found"])

    def test_acceptor_race_commits_once(self):
        child = self.call("create", caller=HUMAN, title="Race", parent=self.parent["id"], done_when="Visible.")["task"]
        child = self.change(child, "delegate", caller=HUMAN, owner=B)
        child = self.report(child)
        barrier = threading.Barrier(2)
        def accept(caller):
            store = provider.Store(self.root.name)
            barrier.wait(timeout=5)
            try:
                return store.invoke({"caller": caller, "payload": {"action": "accept", "id": child["id"], "revision": child["revision"], "operation_id": "race"}})
            except ValueError as error:
                return str(error)
            finally:
                store.db.close()
        with ThreadPoolExecutor(max_workers=2) as pool:
            results = list(pool.map(accept, (A, HUMAN)))
        self.assertEqual(1, sum(isinstance(result, dict) for result in results))
        self.assertEqual(1, sum("stale" in str(result) for result in results))

    def test_cycles_include_ancestors_grandparents_and_cross_branch_completion(self):
        child = self.child()
        grandchild = self.call("create", parent=child["id"], title="Grandchild", done_when="Visible.")["task"]
        before = self.counts()
        for target in (self.parent, child):
            with self.assertRaisesRegex(ValueError, "cycle"):
                self.change(grandchild, "depends", depends_on=[target["id"]])
        self.assertEqual(before, self.counts())
        # Parent depending on its child is redundant, not a cycle.
        self.parent = self.change(self.parent, "depends", depends_on=[child["id"]])
        other = self.child()
        other_leaf = self.call("create", parent=other["id"], title="Other leaf", done_when="Visible.")["task"]
        grandchild = self.change(grandchild, "depends", depends_on=[other["id"]])
        with self.assertRaisesRegex(ValueError, "cycle"):
            self.change(other_leaf, "depends", depends_on=[child["id"]])

    def test_atomic_plan_forward_references_replay_and_subtree_authority(self):
        nodes = [{"key": "leaf", "parent": "root", "title": "leaf", "owner": B, "done_when": "Visible.", "depends_on": ["peer"]},
                 {"key": "root", "title": "root", "owner": A, "done_when": "Children checked."},
                 {"key": "peer", "parent": "root", "title": "peer", "done_when": "Visible."}]
        with self.assertRaisesRegex(ValueError, "human approval"):
            self.call("plan", nodes=nodes)
        plan = self.call("plan", caller=HUMAN, nodes=nodes, operation_id="approved-plan")
        self.assertEqual(plan, self.call("plan", caller=HUMAN, nodes=nodes, operation_id="approved-plan"))
        self.assertEqual({"tasks": 3, "dependencies": 1}, plan["counts"])
        leaf = self.call("get", id=plan["tasks"]["leaf"])["task"]
        self.assertEqual(B, leaf["owner"])
        self.assertEqual([plan["tasks"]["peer"]], leaf["waiting_on"])
        with self.assertRaisesRegex(ValueError, "owned"):
            self.call("plan", parent=leaf["id"], revision=leaf["revision"], nodes=[{"key": "sub", "title": "sub", "done_when": "Visible."}])
        for revision in (self.parent["revision"] - 1, True, float(self.parent["revision"])):
            with self.assertRaisesRegex(ValueError, "stale"):
                self.call("plan", parent=self.parent["id"], revision=revision, nodes=[{"key": "step", "title": "step", "done_when": "Visible."}])
        subtree = self.call("plan", parent=self.parent["id"], revision=self.parent["revision"], nodes=[{"key": "step", "title": "step", "done_when": "Visible."}])
        self.assertEqual(self.parent["id"], self.call("get", id=subtree["tasks"]["step"])["task"]["parent"])

    def test_plan_rejections_are_whole_batch_and_missing_criteria_never_backfilled(self):
        valid = {"key": "a", "title": "A", "done_when": "Visible."}
        bad_plans = [[valid, {"key": "b", "title": "B"}],
                     [valid, {**valid, "key": "b", "depends_on": ["missing"]}],
                     [{**valid, "depends_on": ["b"]}, {**valid, "key": "b", "depends_on": ["a"]}],
                     [valid, {**valid}],
                     [{**valid, "key": self.parent["id"]}],
                     [{**valid, "owner": {"class": "agent"}}]]
        for nodes in bad_plans:
            before = self.counts()
            with self.assertRaises(ValueError):
                self.call("plan", caller=HUMAN, nodes=nodes)
            self.assertEqual(before, self.counts())
        with mock.patch.object(provider, "MAX_RECORDS", self.counts()[1] + 1):
            before = self.counts()
            with self.assertRaisesRegex(ValueError, "quota"):
                self.call("plan", caller=HUMAN, nodes=[valid, {**valid, "key": "b"}])
            self.assertEqual(before, self.counts())
        oversized = [{**valid, "key": f"n{i}", "done_when": "x" * 1000} for i in range(100)]
        with self.assertRaisesRegex(ValueError, "64 KiB"):
            self.call("plan", caller=HUMAN, nodes=oversized)

    def test_criteria_timing_freeze_and_direct_agent_decision(self):
        child = self.child(estimate_work_ms=500, estimate_wait_ms=100, brief_path="brief.md", due_at="2026-10-03T00:00:00Z")
        self.assertIsNone(child["started_at"])
        child = self.change(child, "start")
        first_start = child["started_at"]
        child = self.change(child, "block", text="pause")
        child = self.change(child, "start")
        self.assertEqual(first_start, child["started_at"])
        child = self.change(child, "update", actual_work_ms=10, actual_wait_ms=20)
        child = self.change(child, "done", text="> Result is visible.\nEvidence: checked")
        self.assertGreaterEqual(child["elapsed_ms"], 0)
        self.assertEqual(10, child["actual_work_ms"])
        with self.assertRaisesRegex(ValueError, "frozen"):
            self.change(child, "update", done_when="Changed.")
        decision = self.call("decision", title="Choice?", done_when="Choice A or B is recorded.")["task"]
        with self.assertRaisesRegex(ValueError, "human-only"):
            self.change(decision, "answer", text="A")
        decision = self.change(decision, "answer", caller=HUMAN, text="A")
        self.assertEqual("done", decision["state"])
        with self.assertRaisesRegex(ValueError, "human approval"):
            self.call("create", title="Unapproved root", done_when="Visible.")

    def test_metadata_and_quote_bounds(self):
        for value in ("", "one\ntwo\nthree\nfour", "repeat\nrepeat", "repeat \nrepeat", "\x1bcontrol", "x" * 1025):
            with self.assertRaises(ValueError):
                graph.metadata({"done_when": value}, required=True)
        for field in graph.MILLISECONDS:
            for value in (-1, True, 1.5, 1 << 53):
                with self.assertRaises(ValueError):
                    graph.metadata({field: value})
        for value in ("a\nb", "x" * 513, "a\x00b"):
            with self.assertRaises(ValueError):
                graph.metadata({"brief_path": value})
        for text in ("> One\n> Two\nproof", "> One\nproof\n> One\nproof\n> Two\nproof", "> One\nproof", "proof only"):
            with self.assertRaises(ValueError):
                graph.evidence("One\nTwo", text)
        self.assertEqual("> Two\nproof\n> One\nproof", graph.evidence("One\nTwo", "> Two\nproof\n> One\nproof"))

    def test_max_plan_is_bounded_and_receipt_restart_is_exact(self):
        nodes = [{"key": f"n{i}", "title": str(i), "done_when": "Visible.", "depends_on": [f"n{j}" for j in range(max(0, i-4), i)]} for i in range(100)]
        started = time.monotonic()
        outcome = self.call("plan", caller=HUMAN, nodes=nodes, operation_id="max-plan")
        elapsed = time.monotonic() - started
        self.assertLess(elapsed, 2.5)
        self.assertLess(len(provider.canonical(outcome).encode()), provider.MAX_REPLY_BYTES)
        self.store.db.close()
        self.store = provider.Store(self.root.name)
        self.assertEqual(outcome, self.call("op", operation_id="max-plan", lookup_caller=HUMAN)["outcome"])

    def test_import_uncertain_commit_preserves_atomic_scope(self):
        for after in (False, True):
            with tempfile.TemporaryDirectory() as directory:
                store = provider.Store(directory)
                store.db = FaultCommit(store.db, after)
                with self.assertRaises(provider.OutcomeUnknown):
                    store.invoke({"caller": HUMAN, "payload": {"action": "plan", "operation_id": "uncertain-plan", "nodes": [{"key": "root", "title": "root", "done_when": "Visible."}]}})
                reopened = provider.Store(directory)
                receipt = reopened.lookup({"operation_id": "uncertain-plan", "lookup_caller": HUMAN}, UNKNOWN)
                self.assertEqual(after, receipt["found"])
                self.assertEqual(int(after), reopened.db.execute("SELECT COUNT(*) FROM tasks").fetchone()[0])
                reopened.db.close()


if __name__ == "__main__":
    unittest.main()
