"""Package mapping; caller facts come from the host, never the view."""
import copy
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import provider

A = {"class": "agent", "terminal_id": "term_a", "agent": "pi", "execution": "boot:1:10"}
H = {"class": "human_unverified", "terminal_id": "term_h", "uid": 1000}
C = {"class": "human_unverified", "uid": 1000, "client": {"boot_id": "boot", "pid": 123, "birth": 456, "tty": {"kind": "unix", "device": "1:2:100"}}}


class ViewActionTests(unittest.TestCase):
    def setUp(self):
        self.root = tempfile.TemporaryDirectory()
        self.store = provider.Store(self.root.name)

    def tearDown(self):
        self.store.db.close()
        self.root.cleanup()

    def call(self, payload, caller=C):
        return self.store.invoke({"caller": caller, "payload": payload})

    def task(self, kind="task", op="create", **fields):
        payload = {"action": "decision" if kind == "decision" else "create", "title": "Question or report", "done_when": "Result is recorded.", "operation_id": op, **fields}
        if kind == "decision":
            payload = {"options": [{"key": "go", "label": "Go"}, {"key": "wait", "label": "Wait"}], "default": "wait", "reversible": True, "waiting_cost": "Work waits.", **payload}
        return self.call(payload, A)["task"]

    def report(self, task):
        return self.call({"action": "report", "id": task["id"], "revision": task["revision"], "operation_id": "report-"+task["id"], "text": "> Result is recorded.\nVerified result."}, A)["task"]

    def view(self, task, op="click", **extra):
        return {"action": "view_action", "subject": {"id": task["id"], "revision": task["revision"]}, "operation_id": op, **extra}

    def test_client_binding_is_strict_and_preserves_legacy(self):
        for binding in (C, H, A):
            self.assertEqual(binding, provider.checked_binding(copy.deepcopy(binding)))
        self.assertEqual(C, self.call({"action": "whoami"})["caller"])
        for bad in ({**C, "terminal_id": "invented"}, {**C, "sid": "sid"}, {**C, "uid": True}, {**C, "uid": -1},
                    {**C, "client": {**C["client"], "pid": True}}, {**C, "client": {**C["client"], "pid": 0}},
                    {**C, "client": {**C["client"], "birth": 0}}, {**C, "client": {**C["client"], "birth": 2**64}},
                    {**C, "client": {**C["client"], "boot_id": "bad\nboot"}}, {**C, "client": {**C["client"], "extra": "actor"}},
                    {**C, "client": {**C["client"], "tty": {"kind": "console", "device": "1:2:100"}}},
                    {**C, "client": {**C["client"], "tty": {"kind": "unix", "device": "1:0:0"}}}):
            with self.subTest(bad=bad), self.assertRaises(ValueError):
                provider.checked_binding(bad)

    def test_accept_preserves_original_identity_authorization_and_receipt(self):
        task = self.report(self.task())
        payload = self.view(task)
        accepted = self.call(payload)
        self.assertEqual("accepted", accepted["task"]["state"])
        self.assertEqual(C, accepted["caller"])
        self.assertNotIn("terminal_id", accepted["caller"])
        revision = self.store.document_revision()
        self.assertEqual(accepted, self.call(payload))
        self.assertEqual(revision, self.store.document_revision())
        self.assertFalse(self.call({"action": "op", "operation_id": "click"}, H)["found"])
        found = self.call({"action": "op", "operation_id": "click", "lookup_caller": C}, H)
        self.assertEqual(accepted, found["outcome"])
        self.assertEqual(C, json.loads(self.store.db.execute("SELECT caller FROM transitions ORDER BY seq DESC LIMIT 1").fetchone()[0]))

    def test_answer_requires_listed_choice_and_replay_conflicts_on_changed_choice(self):
        task = self.task("decision")
        payload = self.view(task, option="go")
        answered = self.call(payload)
        self.assertEqual("done", answered["task"]["state"])
        self.assertEqual("go", answered["task"]["decision"]["resolution"]["option"])
        self.assertEqual(answered, self.call(payload))
        with self.assertRaisesRegex(ValueError, "payload conflict"):
            self.call({**payload, "option": "wait"})
        task = self.task("decision", op="another")
        for extra in ({}, {"option": "unknown"}, {"text": "free text"}, {"option": "go", "caller": H}):
            with self.subTest(extra=extra), self.assertRaises(ValueError):
                self.call(self.view(task, op="bad-choice", **extra))
        self.assertEqual("open", self.store.task(task["id"])["state"])

    def test_view_mapping_cannot_bypass_agent_unknown_or_acceptor_policy(self):
        task = self.report(self.task())
        for caller in (A, {"class": "unknown"}):
            with self.subTest(caller=caller), self.assertRaises(ValueError):
                self.call(self.view(task, op="forbidden"), caller)
        decision = self.task("decision", op="decision")
        with self.assertRaisesRegex(ValueError, "human-only"):
            self.call(self.view(decision, op="agent-answer", option="go"), A)
        parent = self.task(op="parent")
        child = self.task(op="child", parent=parent["id"])
        child = self.call({"action": "delegate", "id": child["id"], "revision": child["revision"], "operation_id": "delegate-child", "owner": {**A, "terminal_id": "term_b", "execution": "boot:2:20"}}, A)["task"]
        child = self.call({"action": "report", "id": child["id"], "revision": child["revision"], "operation_id": "report-child", "text": "> Result is recorded.\nVerified."}, H)["task"]
        with self.assertRaisesRegex(ValueError, "acceptor"):
            self.call(self.view(child, op="not-acceptor"))

    def test_stale_does_not_refresh_retry_or_receipt(self):
        task = self.report(self.task())
        stale = self.view({**task, "revision": task["revision"]-1})
        before = self.store.document_revision()
        with self.assertRaises(provider.StaleRevision):
            self.call(stale)
        self.assertEqual(before, self.store.document_revision())
        self.assertEqual("reported", self.store.task(task["id"])["state"])
        self.assertFalse(self.call({"action": "op", "operation_id": "click"})["found"])
        for subject in ({"id": task["id"]}, {"id": task["id"], "revision": True}, {"id": task["id"], "revision": task["revision"], "owner": H}):
            with self.subTest(subject=subject), self.assertRaises(ValueError):
                self.call({**stale, "subject": subject})

    def test_protocol_negotiates_new_method_and_keeps_legacy(self):
        task = self.report(self.task())
        welcome = {"type": "welcome", "protocol": 2, "granted_capabilities": ["action_invoke"], "methods": ["action.invoke.v1", "action.invoke.client.v1"]}
        requests = [
            {"type": "request", "id": "wrong-lane", "method": "action.invoke.v1", "params": {"caller": C, "payload": {"action": "whoami"}}},
            {"type": "request", "id": "legacy", "method": "action.invoke.v1", "params": {"caller": H, "payload": {"action": "whoami"}}},
            {"type": "request", "id": "stale", "method": "action.invoke.client.v1", "params": {"caller": C, "payload": self.view({**task, "revision": task["revision"]-1})}}]
        process = subprocess.run([sys.executable, "-B", str(Path(provider.__file__).resolve()), "--data-dir", self.root.name], input="\n".join(json.dumps(value) for value in [welcome, *requests])+"\n", capture_output=True, text=True, timeout=5)
        self.assertEqual(0, process.returncode, process.stderr)
        lines = [json.loads(line) for line in process.stdout.splitlines()]
        self.assertIn("action.invoke.client.v1", lines[0]["methods"])
        self.assertFalse(lines[1]["result"]["ok"])
        self.assertEqual(H, lines[2]["result"]["value"]["caller"])
        self.assertEqual("stale", lines[3]["result"]["code"])
        self.assertEqual(self.store.document_revision(), lines[3]["result"]["document_revision"])
