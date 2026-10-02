"""Extension-only characterization for durable messages; host/Pi acceptance separate."""
import json
from pathlib import Path
import tempfile
import unittest

import provider
from messages import MAX_BODY_BYTES

A = {"class": "agent", "terminal_id": "term_a", "agent": "pi", "execution": "boot:10:100"}
B = {"class": "agent", "terminal_id": "term_b", "agent": "pi", "execution": "boot:20:200"}
C = {"class": "agent", "terminal_id": "term_c", "agent": "pi", "execution": "boot:30:300"}
HUMAN = {"class": "human_unverified", "terminal_id": "term_human", "uid": 1000}


class MessagesTest(unittest.TestCase):
    def setUp(self):
        self.root = tempfile.mkdtemp()
        self.store = provider.Store(self.root)

    def invoke(self, payload, caller=A):
        return self.store.invoke({"caller": caller, "payload": payload})

    def send(self, caller=A, recipient=B, body="hello", op="op1", **extra):
        return self.invoke({"action": "message_send", "operation_id": op, "recipient": recipient, "body": body, **extra}, caller)

    def test_send_edit_claim_reply_thread_and_provenance(self):
        sent = self.send(body="first draft", sender_address="alpha")
        message = sent["message"]
        self.assertEqual(message["state"], "waiting")
        self.assertEqual(message["sender_address"], "alpha")
        self.assertEqual(json.loads(message["sender"]), A)

        edited = self.invoke({"action": "message_edit", "operation_id": "edit1", "id": message["id"], "revision": message["revision"], "body": "final draft"}, A)
        self.assertEqual(edited["message"]["body"], "final draft")
        self.assertEqual(edited["message"]["revision"], 2)

        # Only the exact addressed execution can claim; C sees nothing.
        self.assertEqual(self.invoke({"action": "message_claim", "operation_id": "c-claim"}, C)["claimed"], [])
        claimed = self.invoke({"action": "message_claim", "operation_id": "b-claim"}, B)["claimed"]
        self.assertEqual([item["id"] for item in claimed], [message["id"]])
        self.assertEqual(claimed[0]["state"], "claimed")
        self.assertEqual(claimed[0]["body"], "final draft")

        # Claimed messages cannot be edited, and a second claim is empty.
        with self.assertRaises(ValueError):
            self.invoke({"action": "message_edit", "operation_id": "late", "id": message["id"], "revision": claimed[0]["revision"], "body": "too late"}, A)
        self.assertEqual(self.invoke({"action": "message_claim", "operation_id": "again"}, B)["claimed"], [])

        # Threaded reply back to A, with B's host-attributed sender binding.
        reply = self.invoke({"action": "message_send", "operation_id": "reply1", "recipient": A, "body": "ack", "reply_to": message["id"]}, B)
        self.assertEqual(reply["message"]["reply_to"], message["id"])
        inbox = self.invoke({"action": "message_list"}, A)
        bodies = [item["body"] for item in inbox["messages"]]
        self.assertIn("ack", bodies)
        self.assertIn("final draft", bodies)
        self.assertEqual(json.loads(reply["message"]["sender"]), B)

    def test_edit_rejections_and_message_guards(self):
        message = self.send(body="x")["message"]
        with self.assertRaises(ValueError):
            self.invoke({"action": "message_edit", "operation_id": "not-sender", "id": message["id"], "revision": 1, "body": "y"}, B)
        with self.assertRaises(ValueError):
            self.invoke({"action": "message_edit", "operation_id": "stale", "id": message["id"], "revision": 9, "body": "y"}, A)
        with self.assertRaises(ValueError):
            self.send(body="", op="empty")
        with self.assertRaises(ValueError):
            self.send(body="x" * (MAX_BODY_BYTES + 1), op="huge")
        with self.assertRaises(ValueError):
            self.send(op="bad-reply", reply_to="msg_missing")
        with self.assertRaises(ValueError):
            self.invoke({"action": "message_send", "operation_id": "human-recipient", "recipient": HUMAN, "body": "hi"}, A)

    def test_activation_is_one_recorded_human_setting(self):
        self.assertFalse(self.invoke({"action": "message_list"}, A)["activation"])
        with self.assertRaises(ValueError):
            self.invoke({"action": "message_activation", "operation_id": "agent-set", "activation": True}, A)
        self.assertTrue(self.invoke({"action": "message_activation", "operation_id": "human-on", "activation": True}, HUMAN)["activation"])
        self.assertTrue(self.invoke({"action": "message_list"}, A)["activation"])
        log = self.store.db.execute("SELECT activation, caller FROM message_policy_log ORDER BY seq").fetchall()
        self.assertEqual([(row["activation"], json.loads(row["caller"])) for row in log], [(1, HUMAN)])

    def test_operation_dedupe_and_provider_restart_durability(self):
        sent = self.send(body="durable")
        again = self.send(body="durable")
        self.assertEqual(again["message"]["id"], sent["message"]["id"])
        with self.assertRaises(ValueError):
            self.send(body="different body, same op")
        self.store.db.close()
        reopened = provider.Store(self.root)
        waiting = reopened.invoke({"caller": B, "payload": {"action": "message_claim", "operation_id": "b2"}})["claimed"]
        self.assertEqual([item["body"] for item in waiting], ["durable"])


if __name__ == "__main__":
    unittest.main()
