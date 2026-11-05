import io
import json
import sqlite3
import tempfile
import unittest
from unittest.mock import patch

import cli
import ids
import provider

H = {"class": "human_unverified", "terminal_id": "human", "uid": 1000}
A = {"class": "agent", "terminal_id": "a", "agent": "pi", "execution": "boot:1:1"}


class IdTests(unittest.TestCase):
    def test_exact_unique_ambiguous_short_bare_and_prefixed_for_both_kinds(self):
        db = sqlite3.connect(":memory:")
        self.addCleanup(db.close)
        for table, prefix in ids.KINDS.items():
            db.execute(f"CREATE TABLE {table}(id TEXT PRIMARY KEY)")
            values = [prefix + "abcdef123456", prefix + "abcdef999999", prefix + "123456abcdef", prefix + "abc"]
            db.executemany(f"INSERT INTO {table} VALUES (?)", [(value,) for value in values])
            for value in values:
                self.assertEqual(value, ids.resolve(db, table, value))
            for bare in ("123456", "123456ABCDEF", "abcdef1"):
                for token in (bare, prefix + bare):
                    self.assertEqual(values[2] if bare.startswith("123") else values[0], ids.resolve(db, table, token))
            for token in ("abcdef", prefix + "abcdef"):
                with self.assertRaisesRegex(ValueError, "ambiguous") as caught:
                    ids.resolve(db, table, token)
                for candidate in values[:2]:
                    self.assertIn(candidate, str(caught.exception))
            for token in ("12345", prefix + "12345"):
                with self.assertRaisesRegex(ValueError, "at least 6 hex"):
                    ids.resolve(db, table, token)
            for token in ("ffffff", prefix + "ffffff", "%", prefix + "a_", "wrong_123456"):
                with self.assertRaises(ValueError):
                    ids.resolve(db, table, token)
            db.execute(f"INSERT INTO {table} VALUES (?)", (prefix + "abcdef",))
            self.assertEqual(prefix + "abcdef", ids.resolve(db, table, prefix + "abcdef"))

    def test_short_forms_use_the_whole_namespace_and_keep_exact_ids(self):
        values = ["task_abcdef123456", "task_abcdef129999", "task_123456abcdef", "legacy"]
        forms = ids.short_forms(values)
        self.assertEqual("task_abcdef123", forms[values[0]])
        self.assertEqual("task_abcdef129", forms[values[1]])
        self.assertEqual("task_123456ab", forms[values[2]])
        self.assertNotIn("legacy", forms)
        self.assertEqual({"task_abcdef": "task_abcdef", "task_abcdef1": "task_abcdef1"}, ids.short_forms(["task_abcdef", "task_abcdef1"]))


class StoreIdTests(unittest.TestCase):
    def setUp(self):
        root = tempfile.TemporaryDirectory()
        self.addCleanup(root.cleanup)
        self.store = provider.Store(root.name)
        self.addCleanup(self.store.db.close)

    def invoke(self, payload, caller=H):
        return self.store.invoke({"caller": caller, "payload": payload})

    def create(self, hex_id, op):
        with patch("provider.uuid.uuid4") as uuid:
            uuid.return_value.hex = hex_id
            return self.invoke({"action": "create", "title": "Work", "done_when": "Result recorded.", "operation_id": op})["task"]

    def test_task_reads_writes_revisions_and_replay_after_collision(self):
        task = self.create("abcdef123456", "create")
        self.assertEqual(task, self.invoke({"action": "get", "id": "abcdef"})["task"])
        payload = {"action": "start", "id": "task_abcdef", "revision": 1, "operation_id": "start"}
        started = self.invoke(payload)
        self.assertEqual(task["id"], started["task"]["id"])
        with self.assertRaisesRegex(ValueError, "revision"):
            self.invoke({**payload, "operation_id": "stale"})
        self.create("abcdef999999", "collision")
        self.assertEqual(started, self.invoke(payload))
        with self.assertRaisesRegex(ValueError, "ambiguous"):
            self.invoke({**payload, "operation_id": "new"})
        with self.assertRaisesRegex(ValueError, "revision"):
            self.invoke({**payload, "id": task["id"], "revision": None, "operation_id": "no-rev"})
        self.assertEqual([], self.invoke({"action": "list", "after": "abcdef9"})["tasks"])

    def test_parent_dependencies_and_plan_attachment_resolve_but_local_keys_win(self):
        parent = self.create("111111abcdef", "parent")
        dependency = self.create("222222abcdef", "dependency")
        child = self.invoke({"action": "create", "title": "child", "done_when": "Done.", "parent": "111111", "depends_on": ["task_222222"], "operation_id": "child"})["task"]
        self.assertEqual(parent["id"], child["parent"])
        self.assertEqual([dependency["id"]], child["depends_on"])
        result = self.invoke({"action": "plan", "parent": "task_111111", "revision": 1, "operation_id": "plan", "nodes": [
            {"key": "222222", "title": "local", "done_when": "Done."},
            {"key": "child", "title": "second", "done_when": "Done.", "depends_on": ["222222", "task_222222"]}]})
        planned = self.store.task(result["tasks"]["child"])
        self.assertEqual(sorted([result["tasks"]["222222"], dependency["id"]]), planned["depends_on"])

    def test_message_edit_reply_revision_authority_and_replay(self):
        with patch("messages.uuid.uuid4") as uuid:
            uuid.return_value.hex = "abcdef123456"
            sent = self.invoke({"action": "message_send", "recipient": A, "body": "first", "operation_id": "send"})["message"]
        edit = {"action": "message_edit", "id": "abcdef", "revision": 1, "body": "edited", "operation_id": "edit"}
        edited = self.invoke(edit)
        self.assertEqual(sent["id"], edited["message"]["id"])
        with self.assertRaisesRegex(ValueError, "revision"):
            self.invoke({**edit, "operation_id": "stale"})
        with self.assertRaisesRegex(ValueError, "original sender"):
            self.invoke({**edit, "revision": 2, "operation_id": "other"}, A)
        reply = self.invoke({"action": "message_send", "recipient": A, "body": "reply", "reply_to": "msg_abcdef", "operation_id": "reply"})
        self.assertEqual(sent["id"], reply["message"]["reply_to"])
        with patch("messages.uuid.uuid4") as uuid:
            uuid.return_value.hex = "abcdef999999"
            self.invoke({"action": "message_send", "recipient": A, "body": "collision", "operation_id": "collision"})
        self.assertEqual(edited, self.invoke(edit))
        with self.assertRaisesRegex(ValueError, "ambiguous"):
            self.invoke({**edit, "operation_id": "ambiguous"})

    def test_cli_prefixes_reach_provider_and_writes_still_require_rev(self):
        task = self.create("abcdef123456", "create")
        def call(payload):
            try:
                value = self.invoke(payload)
            except ValueError as error:
                return {"result": {"type": "extension_action_invoked", "value": {"ok": False, "error": str(error)}}}
            return {"result": {"type": "extension_action_invoked", "value": {"ok": True, "value": value}}}
        payload, mutation = cli.parse_payload(["start", "abcdef", "--rev", "1"])
        status, result = cli.execute(payload, mutation, call=call)
        self.assertEqual("success", status)
        self.assertEqual(task["id"], result["task"]["id"])
        with self.assertRaisesRegex(ValueError, "--rev"):
            cli.parse_payload(["start", "abcdef"])
        with self.assertRaisesRegex(ValueError, "--rev"):
            cli.parse_message(["edit", "abcdef", "body"])
        with patch("messages.uuid.uuid4") as uuid:
            uuid.return_value.hex = "123456abcdef"
            sent = self.invoke({"action": "message_send", "recipient": A, "body": "first", "operation_id": "send"})["message"]
        payload, mutation = cli.parse_message(["edit", "msg_123456", "edited", "--rev", "1"])
        status, result = cli.execute(payload, mutation, call=call)
        self.assertEqual("success", status)
        self.assertEqual(sent["id"], result["message"]["id"])

    def test_tty_list_shortens_ids_json_does_not(self):
        class Tty(io.StringIO):
            def isatty(self):
                return True
        task = self.create("abcdef123456", "create")
        for output, flags, json_output in ((Tty(), [], False), (io.StringIO(), [], True), (Tty(), ["--json"], True)):
            with patch("sys.stdout", output), patch("cli.execute", return_value=("success", {"tasks": [task]})), patch("cli.tree_read", return_value={"tasks": [task]}) as read:
                self.assertEqual(0, cli.main(["task", "list", *flags]))
                self.assertEqual(not json_output, read.called)
            if json_output:
                self.assertEqual(task["id"], json.loads(output.getvalue())["tasks"][0]["id"])
            else:
                self.assertIn("task_abcdef12 [", output.getvalue())
                self.assertNotIn(task["id"], output.getvalue())
