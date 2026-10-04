import contextlib
import copy
import io
import json
import os
from pathlib import Path
import shlex
import tempfile
import unittest
from unittest.mock import patch

import cli

CALLER = {"class": "agent", "terminal_id": "term_a", "agent": "pi", "execution": "boot:1:2"}

def success(value):
    return {"result": {"type": "extension_action_invoked", "value": {"ok": True, "value": value}}}

class CliTests(unittest.TestCase):
    def test_named_owners_resolve_only_to_exact_bindings_before_writing(self):
        payload = {"action": "plan", "nodes": [{"key": "root"}, {"owner": "task-a"}, {"owner": CALLER}]}
        lookups = []
        cli.resolve_owner_names(payload, lambda name: lookups.append(name) or CALLER)
        self.assertEqual(lookups, ["task-a"])
        self.assertEqual(payload["nodes"][1]["owner"], CALLER)
        writes = []
        payload = {"action": "plan", "nodes": [{"owner": "ok"}, {"owner": "missing"}]}
        def resolve(name):
            if name == "missing": raise ValueError("missing")
            return CALLER
        self.assertEqual(cli.execute(payload, True, lambda p: writes.append(p), resolve)[0], "rejected")
        self.assertEqual(writes, [])

    def test_delegate_to_name_requires_revision_and_conflicts_with_binding(self):
        self.assertEqual(cli.parse_payload(["delegate", "id", "--to", "task-b", "--rev", "1"])[0]["owner"], "task-b")
        for args in (["delegate", "id", "--to", "task-b"], ["delegate", "id", "--to", "task-b", "--owner", "{}", "--rev", "1"]):
            with self.assertRaises(ValueError): cli.parse_payload(args)

    def test_task_writes_require_explicit_revision_and_delegation_binding(self):
        for args in (["start", "id"], ["accept", "id", "--rev", "0"], ["delegate", "id", "--rev", "1"], ["start", "id", "--rev", "1", "--actor", "human"]):
            with self.assertRaises(ValueError): cli.parse_payload(args)
        for action in ("start", "accept", "done"):
            self.assertEqual(cli.parse_payload([action, "id", "--rev", "2"])[0]["revision"], 2)

    def test_explicit_operation_requires_original_binding_not_current_preflight(self):
        with self.assertRaises(ValueError): cli.parse_payload(["create", "retry", "--op", "old"])
        payload, mutation = cli.parse_payload(["create", "retry", "--op", "old", "--caller", cli.encoded(CALLER)])
        self.assertTrue(mutation)
        self.assertEqual(payload["expected_caller"], CALLER)
        writes = []
        replacement = dict(CALLER, execution="replacement")
        status, _ = cli.execute(payload, mutation, lambda p: writes.append(copy.deepcopy(p)) or success({"caller": replacement}))
        self.assertEqual(status, "rejected")
        self.assertEqual(writes, [{"action": "whoami"}])

    def task(self, task_id, parent=None):
        return {"id": task_id, "parent": parent, "kind": "task", "state": "reported", "revision": 1, "title": "proof", "owner": CALLER, "creator": dict(CALLER, execution="creator_exec"), "owed_to": {"class": "human_unverified"}, "owed_action": "accept", "owed_since": "2026-10-02"}

    def test_task_tree_projects_extension_supplied_debts_and_rejects_bad_graphs(self):
        root, child = self.task("root"), self.task("child", "root")
        text = cli.render_tree([child, root])
        self.assertLess(text.index("root ["), text.index("  child ["))
        for field in ("creator_exec", "human_unverified", "2026-10-02"): self.assertIn(field, text)
        for rows in ([child], [root, root], [self.task("a", "b"), self.task("b", "a")]):
            with self.assertRaises(ValueError): cli.render_tree(rows)

    def test_deep_tree_indent_is_bounded_and_control_text_is_escaped(self):
        tasks = [dict(self.task(str(i), str(i - 1) if i else None), title="unsafe\x1b[31m\ntext\u202e") for i in range(20)]
        text = cli.render_tree(tasks)
        self.assertIn("(depth 19)", text)
        self.assertNotIn("\x1b", text)
        self.assertNotIn("\u202e", text)
        self.assertFalse(any(line.startswith(" " * 35) for line in text.splitlines()))

    def test_structured_decision_filing_and_option_answer_stay_extension_payloads(self):
        with self.assertRaises(ValueError): cli.parse_payload(["decision", "Question?", "--done-when", "Choice recorded"])
        payload, mutation = cli.parse_payload(["decision", "Question?", "--done-when", "Choice", "--options", '[{"key":"yes","label":"Proceed"},{"key":"no","label":"Wait"}]', "--default", "yes", "--reversible", "true", "--waiting-cost", "Work waits."])
        self.assertTrue(mutation)
        self.assertEqual(len(payload["options"]), 2)
        self.assertIs(payload["reversible"], True)
        self.assertEqual(cli.parse_payload(["answer", "id", "yes", "--rev", "1", "--note", "Checked"])[0]["note"], "Checked")

    def test_digest_reads_never_ack_and_seen_requires_explicit_sequence(self):
        self.assertEqual(cli.parse_payload(["decisions"]), ({"action": "decision_digest"}, False))
        with self.assertRaises(ValueError): cli.parse_payload(["decisions-seen"])
        self.assertEqual(cli.parse_payload(["decisions-seen", "--through", "17"])[0]["through"], 17)

    def test_timer_and_revert_require_current_revision(self):
        with self.assertRaises(ValueError): cli.parse_payload(["revert", "id"])
        self.assertIsNone(cli.parse_payload(["decision-timer", "id", "--rev", "3", "--auto-at", ""])[0]["auto_at"])

    def test_receipt_lookup_selector_is_not_a_caller_authority_flag(self):
        payload, mutation = cli.parse_payload(["op", "op-1", "--caller", cli.encoded(CALLER)])
        self.assertFalse(mutation)
        self.assertEqual(payload["lookup_caller"], CALLER)
        self.assertNotIn("caller", payload)

    def test_graph_metadata_and_ready_commands_remain_extension_payloads(self):
        payload, _ = cli.parse_payload(["create", "child", "--parent", "root", "--done-when", "Verified", "--estimate-work-ms", "100", "--actual-wait-ms", "0", "--depends-on", '["proof"]'])
        self.assertEqual(payload["estimate_work_ms"], 100)
        self.assertEqual(payload["actual_wait_ms"], 0)
        self.assertEqual(payload["depends_on"], ["proof"])
        self.assertEqual(cli.parse_payload(["ready", "--after", "id"])[0]["after"], "id")
        for value in ("-1", "true", "1.5", "18446744073709551616"):
            with self.assertRaises(ValueError): cli.parse_payload(["update", "id", "--rev", "3", "--actual-work-ms", value])

    def test_plan_file_is_bounded_and_attachment_requires_revision(self):
        with tempfile.TemporaryDirectory() as root:
            file = Path(root) / "plan.json"
            file.write_text('{"nodes":[{"key":"proof","title":"Proof","done_when":"File exists"}]}')
            self.assertEqual(cli.parse_payload(["plan", str(file)])[0]["nodes"][0]["key"], "proof")
            for flags in (["--parent", "id"], ["--rev", "1"]):
                with self.assertRaises(ValueError): cli.parse_payload(["plan", str(file), *flags])
            file.write_text('{"nodes":[],"actor":"human"}')
            with self.assertRaises(ValueError): cli.read_plan(file)
            file.write_bytes(b" " * (64 * 1024 + 1))
            with self.assertRaisesRegex(ValueError, "64 KiB"): cli.read_plan(file)

    def test_lost_or_malformed_reply_never_becomes_definite_rejection(self):
        for response in ({"error": {"code": "outcome_unknown", "message": "lost"}}, {}, {"result": {"type": "extension_action_invoked", "value": {"ok": True}}}, {"error": "invalid"}):
            self.assertEqual(cli.decode_reply(response)[0], "unknown")
        for response in ({"error": {"code": "action_rejected", "message": "not admitted"}}, {"result": {"type": "extension_action_invoked", "value": {"ok": False, "error": "stale"}}}):
            self.assertEqual(cli.decode_reply(response)[0], "rejected")

    def test_uncertain_write_preserves_original_caller_op_and_never_retries(self):
        payload, mutation = cli.parse_payload(["report", "id", "Evidence", "--rev", "3", "--op", "original", "--caller", cli.encoded(CALLER)])
        calls = []
        def call(value):
            calls.append(copy.deepcopy(value))
            return success({"caller": CALLER}) if value["action"] == "whoami" else {"error": {"code": "outcome_unknown", "message": "lost"}}
        status, message = cli.execute(payload, mutation, call)
        self.assertEqual(status, "unknown")
        self.assertEqual(len(calls), 2)
        self.assertEqual(calls[1]["expected_caller"], CALLER)
        self.assertIn("op original", message)
        self.assertIn(cli.encoded(CALLER), message)
        self.assertIn("lookup first", message)

    def test_entropy_and_unknown_caller_fail_before_write(self):
        payload = {"action": "create", "title": "proof"}
        calls = []
        with patch("cli.os.urandom", side_effect=OSError("entropy unavailable")):
            with self.assertRaises(OSError): cli.execute(payload, True, lambda p: calls.append(p))
        self.assertEqual(calls, [])
        status, _ = cli.execute(dict(payload), True, lambda p: calls.append(p) or success({"caller": {"class": "unknown"}}))
        self.assertEqual(status, "rejected")
        self.assertEqual(calls, [{"action": "whoami"}])

    def test_message_binding_revision_and_claim_payloads(self):
        self.assertEqual(cli.parse_message(["send", "task-b", "body", "--op", "id"])[0]["target"], "task-b")
        self.assertTrue(cli.parse_message(["claim", "--idle"])[0]["idle"])
        with self.assertRaises(ValueError): cli.parse_message(["edit", "id", "body"])
        self.assertFalse(cli.parse_message(["activation", "off"])[0]["activation"])

    def test_message_send_target_can_be_an_exact_agent_binding(self):
        binding = {"class": "agent", "terminal_id": "term_a", "agent": "pi", "execution": "boot:1:2"}
        self.assertEqual(cli.resolve_recipient(json.dumps(binding)), binding)
        for bad in (
            json.dumps({"class": "human_unverified", "terminal_id": "t", "agent": "pi", "execution": "e"}),
            json.dumps({**binding, "extra": "x"}),
            json.dumps({**binding, "execution": ""}),
            json.dumps({"class": "agent", "terminal_id": "t", "agent": "pi"}),
            "{not json",
        ):
            with self.assertRaises(ValueError):
                cli.resolve_recipient(bad)

    def test_message_grammar_and_help_are_local_without_api(self):
        for args in ([], ["activation", "maybe"], ["send", "only-target"], ["edit", "id", "text"]):
            with self.assertRaises(ValueError): cli.parse_message(args)
        output = io.StringIO()
        with contextlib.redirect_stdout(output), patch("cli.invoke", side_effect=AssertionError("help must not call API")):
            self.assertEqual(cli.main(["message", "--help"]), 0)
        for word in ("send", "list", "claim", "edit", "activation"):
            self.assertIn(word, output.getvalue())

    def test_output_flags_never_consume_literal_values(self):
        self.assertEqual(cli.output_options("message", ["send", "agent", "--json", "--json"]), (["send", "agent", "--json"], True))
        self.assertEqual(cli.output_options("task", ["create", "title", "--done-when", "--json"]), (["create", "title", "--done-when", "--json"], False))

    def test_tty_readable_redirected_json_and_forced_json(self):
        class Tty(io.StringIO):
            def isatty(self): return True
        for output, flags, expected_json in ((Tty(), [], False), (io.StringIO(), [], True), (Tty(), ["--json"], True)):
            with patch("sys.stdout", output), patch("cli.execute", return_value=("success", {"task": self.task("proof")})):
                self.assertEqual(cli.main(["task", "get", "proof", *flags]), 0)
            text = output.getvalue()
            if expected_json: self.assertEqual(json.loads(text)["task"]["id"], "proof")
            else: self.assertIn("proof [task / reported", text)

    def test_shipped_skill_task_examples_parse_with_current_cli(self):
        skill = (Path(__file__).resolve().parents[2] / "skills/vqro/SKILL.md").read_text()
        with tempfile.TemporaryDirectory() as root:
            plan = Path(root) / "plan.json"; plan.write_text('{"nodes":[]}')
            values = {"$PARENT_ID": "parent", "$TASK_ID": "child", "$REV": "3", "$PARENT_REV": "2", "$OWNER": cli.encoded(CALLER), "$CALLER": cli.encoded(CALLER), "$CRITERIA": "File exists", "$EVIDENCE": "> File exists\nEvidence: checked", "$CURSOR": "cursor", "$OP_ID": "original", "$PLAN_FILE": str(plan)}
            actions = set()
            for line in skill.splitlines():
                if not line.startswith("vqro task "): continue
                args = [values.get(word, word.replace("PREREQUISITE_ID", "proof")) for word in shlex.split(line[10:])]
                args, _ = cli.output_options("task", args)
                payload, _ = cli.parse_payload(args)
                actions.add(payload["action"])
            self.assertTrue({"create", "decision", "plan", "report", "done", "op", "delegate"} <= actions)

if __name__ == "__main__": unittest.main()
