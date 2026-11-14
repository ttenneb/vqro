"""Private SQLite crash cuts and explicit original-record recovery."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import cli
import provider
from messages import MAX_ATTEMPTS
from messages_test import A, B, C


class DeliveryTest(unittest.TestCase):
    def setUp(self):
        self.root = tempfile.TemporaryDirectory()
        self.addCleanup(self.root.cleanup)
        self.store = provider.Store(self.root.name)
        self.addCleanup(lambda: self.store.db.close())

    def invoke(self, action, caller=B, **payload):
        if action not in ("message_get", "message_list"):
            payload.setdefault("operation_id", os.urandom(8).hex())
        return self.store.invoke({"caller": caller, "payload": {"action": action, **payload}})

    def claim(self):
        message = self.invoke("message_send", A, recipient=B, body="original body")["message"]
        return self.invoke("message_claim")["claimed"][0]

    def outcome(self, message, outcome, caller=B, **extra):
        return self.invoke("message_outcome", caller, id=message["id"], attempt=message["attempt"]["id"], outcome=outcome, **extra)["message"]

    def recover(self, message, mode, caller=A, **extra):
        return self.invoke("message_recover", caller, id=message["id"], revision=message["revision"], mode=mode, **extra)["message"]

    def test_actual_claim_commit_process_crash_reopens_as_attention_never_reclaims(self):
        self.invoke("message_send", A, recipient=B, body="crash-cut")
        # Actual provider commit then process death, without closing SQLite or
        # invoking any adapter. No inherited host environment enters the child.
        script = "import os,sys,provider,json; s=provider.Store(sys.argv[1]); s.invoke({'caller':json.loads(sys.argv[2]),'payload':{'action':'message_claim','operation_id':'crash'}}); os._exit(23)"
        env = {"HOME": self.root.name, "PATH": os.environ["PATH"], "PYTHONPATH": str(Path(provider.__file__).parent)}
        result = subprocess.run([sys.executable, "-B", "-c", script, self.root.name, json.dumps(B)], env=env, capture_output=True)
        self.assertEqual(23, result.returncode, result.stderr)
        self.store.db.close()
        self.store = provider.Store(self.root.name)
        listed = self.invoke("message_list")
        self.assertEqual([], listed["waiting"])
        message = listed["messages"][0]
        self.assertTrue(message["attention"])
        self.assertEqual("pending", message["attempt"]["outcome"])
        self.assertEqual([], self.invoke("message_claim")["claimed"])
        lookup = self.invoke("op", operation_id="crash", lookup_caller=B)
        self.assertEqual(message["attempt"], lookup["outcome"]["claimed"][0]["attempt"])

    def test_claim_and_attempt_and_receipt_are_one_transaction(self):
        self.invoke("message_send", A, recipient=B, body="atomic")
        with patch("messages.uuid.uuid4", side_effect=RuntimeError("attempt creation fault")):
            with self.assertRaisesRegex(RuntimeError, "attempt creation"):
                self.invoke("message_claim", operation_id="failed-claim")
        self.assertEqual(0, self.store.db.execute("SELECT COUNT(*) FROM message_attempts").fetchone()[0])
        self.assertEqual(0, self.store.db.execute("SELECT COUNT(*) FROM operations WHERE operation_id='failed-claim'").fetchone()[0])
        self.assertEqual("waiting", self.invoke("message_list")["waiting"][0]["state"])

    def test_no_effect_explicit_retry_keeps_original_record_and_history(self):
        first = self.claim()
        failed = self.outcome(first, "no_effect")
        self.assertTrue(failed["attention"])
        retry = self.recover(failed, "retry")
        self.assertEqual("waiting", retry["state"])
        with self.assertRaisesRegex(ValueError, "previously claimed"):
            self.invoke("message_edit", A, id=retry["id"], revision=retry["revision"], body="not the original")
        second = self.invoke("message_claim")["claimed"][0]
        self.assertNotEqual(first["attempt"]["id"], second["attempt"]["id"])
        for key in ("id", "sender", "recipient", "recipient_execution", "body", "reply_to", "created_at", "claimed_at"):
            self.assertEqual(first[key], second[key], key)
        got = self.invoke("message_get", A, id=first["id"])
        self.assertEqual(["no_effect", "pending"], [item["outcome"] for item in got["attempts"]])
        self.assertEqual([("retry", "sender")], [(item["mode"], item["caller_role"]) for item in got["recoveries"]])
        with self.assertRaisesRegex(ValueError, "stale delivery"):
            self.outcome(first, "submitted")

    def test_uncertainty_requires_explicit_duplicate_warning_and_revision(self):
        for outcome in ("pending", "uncertain", "context_prepared", "submitted"):
            with self.subTest(outcome=outcome):
                message = self.claim()
                if outcome != "pending":
                    message = self.outcome(message, outcome)
                self.assertEqual(outcome != "submitted", message["attention"])
                with self.assertRaisesRegex(ValueError, "may-duplicate"):
                    self.recover(message, "retry")
                with self.assertRaisesRegex(ValueError, "stale message revision"):
                    self.recover({**message, "revision": 1}, "retry", may_duplicate=True)
                retry = self.recover(message, "retry", may_duplicate=True)
                self.assertEqual(message["recipient"], retry["recipient"])
                self.assertEqual(message["id"], retry["id"])
                # Remove this retry from the next iteration's pickup.
                next_message = self.invoke("message_claim")["claimed"][0]
                self.recover(next_message, "resolve")

    def test_exact_binding_and_stale_execution_cannot_report_or_recover(self):
        message = self.claim()
        for caller in (C, {**B, "terminal_id": "reused"}, {**B, "agent": "other"}, {**B, "execution": "replacement"}):
            with self.assertRaises(ValueError):
                self.invoke("message_get", caller, id=message["id"])
            with self.assertRaises(ValueError):
                self.outcome(message, "submitted", caller)
            with self.assertRaises(ValueError):
                self.recover(message, "retry", caller, may_duplicate=True)
        with self.assertRaisesRegex(ValueError, "only the exact recipient"):
            self.outcome(message, "submitted", A)
        recovered = self.recover(message, "retry", may_duplicate=True)
        self.assertEqual([], self.invoke("message_claim", {**B, "execution": "replacement"})["claimed"])
        self.assertEqual(message["recipient"], recovered["recipient"])

    def test_explicit_resolve_never_replays_and_late_outcome_cannot_erase_it(self):
        message = self.claim()
        resolved = self.recover(message, "resolve")
        self.assertFalse(resolved["attention"])
        self.assertIsNotNone(resolved["resolved_at"])
        self.assertEqual("claimed", resolved["state"])
        self.assertEqual([], self.invoke("message_claim")["claimed"])
        with self.assertRaisesRegex(ValueError, "stale delivery"):
            self.outcome(message, "submitted")
        self.assertEqual(1, len(self.invoke("message_get", A, id=message["id"])["attempts"]))

    def test_legacy_claim_migrates_without_fabricating_delivery_or_recipient(self):
        message = self.claim()
        self.store.db.execute("DROP TABLE message_attempts")
        self.store.db.execute("ALTER TABLE messages DROP COLUMN resolved_at")
        self.store.db.execute("PRAGMA user_version=4")
        self.store.db.close()
        self.store = provider.Store(self.root.name)
        legacy = self.invoke("message_get", A, id=message["id"])["message"]
        self.assertIsNone(legacy["attempt"])
        self.assertTrue(legacy["attention"])
        self.assertEqual(message["recipient"], legacy["recipient"])
        with self.assertRaisesRegex(ValueError, "may-duplicate"):
            self.recover(legacy, "retry")
        self.assertFalse(self.recover(legacy, "resolve")["attention"])

    def test_bounded_batch_outcomes_are_atomic_and_receipted_once(self):
        first, second = self.claim(), self.claim()
        pairs = [{"id": item["id"], "attempt": item["attempt"]["id"]} for item in (first, second)]
        before = list(self.store.db.iterdump())
        with self.assertRaisesRegex(ValueError, "stale delivery"):
            self.invoke("message_outcomes", attempts=[pairs[0], {**pairs[1], "attempt": "wrong"}], outcome="submitted")
        self.assertEqual(before, list(self.store.db.iterdump()))
        payload, mutation = cli.parse_message(["outcomes", json.dumps(pairs), "submitted"])
        self.assertTrue(mutation)
        got = self.invoke("message_outcomes", attempts=payload["attempts"], outcome=payload["outcome"], operation_id="batch-outcome")
        self.assertEqual(["submitted", "submitted"], [item["attempt"]["outcome"] for item in got["outcomes"]])
        self.assertEqual(got, self.invoke("message_outcomes", attempts=pairs, outcome="submitted", operation_id="batch-outcome"))
        for bad in ([], pairs * 5, [pairs[0], pairs[0]], [{"id": []}], [{"id": [], "attempt": "a"}]):
            with self.assertRaises(ValueError):
                self.invoke("message_outcomes", attempts=bad, outcome="submitted")

    def test_outcome_and_recovery_faults_roll_back_every_write(self):
        message = self.claim()
        for operation in (lambda: self.outcome(message, "submitted"), lambda: self.recover(message, "resolve")):
            before = list(self.store.db.iterdump())
            with patch.object(self.store.messages, "_record", side_effect=RuntimeError("post-write fault")):
                with self.assertRaisesRegex(RuntimeError, "post-write"):
                    operation()
            self.assertEqual(before, list(self.store.db.iterdump()))

    def test_lost_outcome_ack_receipt_replays_without_new_attempt_or_revision(self):
        message = self.claim()
        payload = dict(id=message["id"], attempt=message["attempt"]["id"], outcome="submitted", operation_id="lost-outcome")
        original = self.invoke("message_outcome", **payload)
        self.store.db.close()
        self.store = provider.Store(self.root.name)
        before = list(self.store.db.iterdump())
        self.assertEqual(original, self.invoke("message_outcome", **payload))
        self.assertEqual(before, list(self.store.db.iterdump()))
        self.assertFalse(self.invoke("message_get", id=message["id"])["message"]["attention"])
        with self.assertRaisesRegex(ValueError, "transition"):
            self.outcome(message, "no_effect")

    def test_v4_migration_failure_preserves_original_rows_receipts_and_version(self):
        message = self.claim()
        self.store.db.execute("DROP TABLE message_attempts")
        self.store.db.execute("DROP TABLE message_recoveries")
        self.store.db.execute("ALTER TABLE messages DROP COLUMN resolved_at")
        self.store.db.execute("PRAGMA user_version=4")
        before = list(self.store.db.iterdump())
        create = self.store.messages.create_schema
        def fail_after_schema():
            create()
            raise RuntimeError("migration crash cut")
        with patch.object(self.store.messages, "create_schema", side_effect=fail_after_schema):
            with self.assertRaisesRegex(RuntimeError, "migration crash cut"):
                self.store.migrate()
        self.assertEqual(before, list(self.store.db.iterdump()))
        self.store.migrate()
        self.assertTrue(self.invoke("message_get", A, id=message["id"])["message"]["attention"])

    def test_attempt_history_and_inspection_are_bounded(self):
        message = self.claim()
        for _ in range(MAX_ATTEMPTS - 1):
            message = self.outcome(message, "no_effect")
            message = self.recover(message, "retry")
            message = self.invoke("message_claim")["claimed"][0]
        message = self.outcome(message, "no_effect")
        with self.assertRaisesRegex(ValueError, "limit"):
            self.recover(message, "retry")
        got = self.invoke("message_get", A, id=message["id"])
        self.assertEqual(MAX_ATTEMPTS, len(got["attempts"]))
        self.assertLess(len(provider.canonical(got).encode()), provider.MAX_REPLY_BYTES)
        self.assertFalse(self.recover(message, "resolve")["attention"])

    def test_cli_original_get_outcome_recovery_and_exact_target_grammar(self):
        message = self.claim()
        def execute(args, caller):
            payload, mutation = cli.parse_message(args)
            return cli.execute(payload, mutation, lambda p: {"result": {"type": "extension_action_invoked", "value": {"ok": True, "value": self.store.invoke({"caller": caller, "payload": p})}}})
        status, got = execute(["get", message["id"]], A)
        self.assertEqual("success", status)
        human = cli.human_output(got)
        self.assertIn("ATTENTION", human)
        self.assertIn("Original recipient (never retargeted)", human)
        self.assertIn(B["execution"], human)
        status, got = execute(["outcome", message["id"], message["attempt"]["id"], "no_effect"], B)
        self.assertEqual("success", status)
        status, got = execute(["recover", message["id"], "retry", "--rev", str(got["message"]["revision"])], A)
        self.assertEqual("success", status)
        self.assertEqual(message["id"], got["message"]["id"])
        for args in (["recover", message["id"], "retry"], ["recover", message["id"], "retry", "--rev", "2", "--target", "new"]):
            with self.assertRaises(ValueError):
                cli.parse_message(args)
        for args in (["outcome", message["id"], "attempt", "no_effect"], ["recover", message["id"], "resolve", "--rev", "2"]):
            with self.assertRaisesRegex(ValueError, "original attempt"):
                cli.parse_message([*args, "--op", "explicit"])
            self.assertTrue(cli.parse_message([*args, "--op", "explicit", "--caller", json.dumps(A)])[1])
        self.assertEqual((["get", message["id"]], True), cli.output_options("message", ["get", message["id"], "--json"]))
        self.assertEqual((["outcome", message["id"], "attempt", "no_effect"], True), cli.output_options("message", ["outcome", message["id"], "attempt", "no_effect", "--json"]))

    def test_attention_is_prioritized_over_newer_history(self):
        message = self.claim()
        for i in range(40):
            self.invoke("message_send", A, recipient=C, body=str(i))
        self.assertEqual(message["id"], self.invoke("message_list", A)["messages"][0]["id"])


if __name__ == "__main__":
    unittest.main()
