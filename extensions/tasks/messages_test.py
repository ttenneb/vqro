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
        sent = self.send(body="first draft")
        message = sent["message"]
        self.assertEqual(message["state"], "waiting")
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
        self.assertEqual([item["body"] for item in inbox["waiting"]], ["ack"])
        self.assertIn("final draft", [item["body"] for item in inbox["messages"]])
        self.assertEqual(json.loads(reply["message"]["sender"]), B)

    def test_list_history_and_claim_require_the_full_exact_recipient_binding(self):
        incoming = self.send()["message"]
        human_message = self.send(caller=HUMAN, op="human-send")["message"]
        lookalikes = [{**B, "terminal_id": "term_reused"}, {**B, "agent": "other"}, {**B, "execution": "boot:20:new"}]
        for index, lookalike in enumerate(lookalikes):
            own = self.send(caller=lookalike, recipient=C, op=f"own-{index}")["message"]
            listed = self.invoke({"action": "message_list"}, lookalike)
            self.assertEqual([], listed["waiting"])
            self.assertEqual([own["id"]], [message["id"] for message in listed["messages"]])
            claimed = self.invoke({"action": "message_claim", "operation_id": f"wrong-{index}"}, lookalike)
            self.assertEqual([], claimed["claimed"])
        # Extra host facts normalize to the same exact four-field agent binding.
        exact_with_host_metadata = {**B, "name": "reusable-display-name", "pane_id": "w9:p9"}
        listed = self.invoke({"action": "message_list"}, exact_with_host_metadata)
        self.assertEqual({incoming["id"], human_message["id"]}, {message["id"] for message in listed["waiting"]})
        claimed = self.invoke({"action": "message_claim", "operation_id": "right"}, exact_with_host_metadata)["claimed"]
        self.assertEqual({incoming["id"], human_message["id"]}, {message["id"] for message in claimed})
        self.assertEqual({incoming["id"], human_message["id"]}, {message["id"] for message in self.invoke({"action": "message_list"}, B)["messages"]})
        for lookalike in lookalikes:
            listed = self.invoke({"action": "message_list"}, lookalike)
            self.assertNotIn(incoming["id"], [message["id"] for message in listed["messages"]])
        # Sender-owned history (including human history) is unchanged.
        self.assertIn(incoming["id"], [message["id"] for message in self.invoke({"action": "message_list"}, A)["messages"]])
        self.assertIn(human_message["id"], [message["id"] for message in self.invoke({"action": "message_list"}, HUMAN)["messages"]])

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

    def test_only_an_exact_agent_binding_can_be_addressed(self):
        # The provider is the validator for an exact binding, including one passed
        # straight through by the CLI for a reply.
        for index, bad in enumerate((
            {"class": "agent", "terminal_id": "t", "agent": "pi"},
            {"class": "agent", "terminal_id": "t", "agent": "pi", "execution": ""},
            {"class": "human_unverified", "terminal_id": "t", "uid": 1},
            {"class": "agent", "terminal_id": "t", "agent": "pi", "execution": "e", "extra": "x"},
        )):
            with self.assertRaises(ValueError):
                self.invoke({"action": "message_send", "operation_id": f"bad-{index}", "recipient": bad, "body": "hi"}, A)

    def test_a_large_inbox_is_claimed_in_bounded_batches(self):
        body = "x" * (MAX_BODY_BYTES - 2)
        for index in range(5):
            self.send(body=f"{index}{body}", op=f"big-{index}")
        first = self.invoke({"action": "message_claim", "operation_id": "c1"}, B)["claimed"]
        self.assertTrue(0 < len(first) < 5)
        second = self.invoke({"action": "message_claim", "operation_id": "c2"}, B)["claimed"]
        self.assertTrue(second)
        self.assertEqual({item["id"] for item in first} & {item["id"] for item in second}, set())

    def test_a_large_utf8_body_stays_within_encoded_bounds(self):
        body = "\u0800" * 10_800
        self.send(body=body, op="wide")
        listed = self.invoke({"action": "message_list"}, B)
        self.assertEqual([item["body"] for item in listed["waiting"]], [body])
        encoded = len(json.dumps(listed, sort_keys=True, separators=(",", ":")).encode())
        self.assertLess(encoded, 128 * 1024)
        claimed = self.invoke({"action": "message_claim", "operation_id": "wide-claim"}, B)["claimed"]
        self.assertEqual([item["body"] for item in claimed], [body])

    def test_serialized_claim_budget_and_oversized_request_do_not_lose_waiting_records(self):
        body = "\u0800" * 10_800
        for index in range(6):
            self.send(body=body, op=f"encoded-{index}")
        claimed_ids = set()
        for batch in range(6):
            outcome = self.invoke({"action": "message_claim", "operation_id": f"encoded-claim-{batch}"}, B)
            self.assertLessEqual(len(provider.canonical(outcome).encode()), provider.MAX_REPLY_BYTES)
            ids = {row["id"] for row in outcome["claimed"]}
            self.assertFalse(ids & claimed_ids)
            claimed_ids.update(ids)
        self.assertEqual(6, len(claimed_ids))
        before = list(self.store.db.iterdump())
        # Raw body is allowed, but escaped wire JSON is not: fail before a receipt
        # or durable send, rather than committing an unreturnable outcome.
        with self.assertRaisesRegex(ValueError, "64 KiB"):
            self.send(body="\u0001" * MAX_BODY_BYTES, op="escaped-too-large")
        self.assertEqual(before, list(self.store.db.iterdump()))

    def test_a_fresh_store_defaults_activation_on_and_keeps_the_stored_value(self):
        self.assertTrue(self.invoke({"action": "message_list"}, A)["activation"])
        self.invoke({"action": "message_activation", "operation_id": "human-off", "activation": False}, HUMAN)
        self.store.db.close()
        reopened = provider.Store(self.root)
        # A restart never re-derives the default or flips the recorded setting.
        self.assertFalse(reopened.invoke({"caller": HUMAN, "payload": {"action": "message_list"}})["activation"])
        reopened.db.close()

    def test_idle_claim_is_gated_on_the_current_activation_policy(self):
        self.send(body="wake")
        listed = self.invoke({"action": "message_list"}, B)
        self.assertEqual([item["body"] for item in listed["waiting"]], ["wake"])
        self.invoke({"action": "message_activation", "operation_id": "off", "activation": False}, HUMAN)
        idle_off = self.invoke({"action": "message_claim", "operation_id": "idle-off", "idle": True}, B)
        self.assertEqual(idle_off["claimed"], [])
        self.assertFalse(idle_off["activation"])
        self.invoke({"action": "message_activation", "operation_id": "on", "activation": True}, HUMAN)
        idle_on = self.invoke({"action": "message_claim", "operation_id": "idle-on", "idle": True}, B)
        self.assertEqual([item["body"] for item in idle_on["claimed"]], ["wake"])

    def test_activation_is_one_recorded_human_setting(self):
        self.assertTrue(self.invoke({"action": "message_list"}, A)["activation"])
        with self.assertRaises(ValueError):
            self.invoke({"action": "message_activation", "operation_id": "agent-set", "activation": False}, A)
        self.assertFalse(self.invoke({"action": "message_activation", "operation_id": "human-off", "activation": False}, HUMAN)["activation"])
        self.assertFalse(self.invoke({"action": "message_list"}, A)["activation"])
        row = self.store.db.execute("SELECT activation, updated_by FROM message_policy WHERE id=1").fetchone()
        self.assertEqual((row["activation"], json.loads(row["updated_by"])), (0, HUMAN))

    def test_lost_claim_ack_recovers_original_batch_after_restart_without_reclaim(self):
        first = self.send(body="original delivery")["message"]
        payload = {"action": "message_claim", "operation_id": "lost-claim", "expected_caller": B}
        original = self.invoke(payload, B)  # Commit happened; pretend its ACK was lost.
        self.send(body="later delivery", op="later")
        self.store.db.close()
        self.store = provider.Store(self.root)
        before = list(self.store.db.iterdump())
        lookup = self.invoke({"action": "op", "operation_id": "lost-claim", "lookup_caller": B}, B)
        self.assertTrue(lookup["authoritative"])
        self.assertEqual(original, lookup["outcome"])
        self.assertEqual(original, self.invoke(payload, B))
        self.assertEqual(before, list(self.store.db.iterdump()))
        self.assertEqual([first["id"]], [row["id"] for row in original["claimed"]])
        waiting = self.invoke({"action": "message_list"}, B)["waiting"]
        self.assertEqual(["later delivery"], [row["body"] for row in waiting])
        replacement = {**B, "execution": "boot:20:replacement"}
        self.assertEqual([], self.invoke({"action": "message_list"}, replacement)["waiting"])
        with self.assertRaisesRegex(ValueError, "original operation scope"):
            self.invoke(payload, replacement)

    def test_document_version_moves_for_each_message_not_only_the_largest_revision(self):
        versions = [self.store.document_revision()]
        first = self.send(body="first", recipient=C)["message"]
        versions.append(self.store.document_revision())
        for revision in range(1, 5):
            self.invoke({"action": "message_edit", "operation_id": f"edit-{revision}",
                         "id": first["id"], "revision": revision, "body": f"draft {revision}"})
            versions.append(self.store.document_revision())
        second = self.send(body="second", op="second")["message"]
        versions.append(self.store.document_revision())
        self.invoke({"action": "message_claim", "operation_id": "claim-lower-revision"}, B)
        versions.append(self.store.document_revision())
        self.send(caller=B, recipient=A, body="reply", reply_to=second["id"], op="reply")
        versions.append(self.store.document_revision())
        self.assertTrue(all(after > before for before, after in zip(versions, versions[1:])), versions)
        before = self.store.document_revision()
        self.invoke({"action": "message_list"}, B)
        self.invoke({"action": "message_claim", "operation_id": "claim-lower-revision"}, B)
        self.invoke({"action": "message_claim", "operation_id": "empty-claim"}, B)
        self.send(body="second", op="second")
        self.assertEqual(before, self.store.document_revision())
        self.store.db.close()
        self.store = provider.Store(self.root)
        self.assertEqual(before, self.store.document_revision())

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
