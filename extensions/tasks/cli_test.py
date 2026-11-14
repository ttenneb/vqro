import contextlib
import copy
import io
import json
import os
from pathlib import Path
import shlex
import socket
import subprocess
import sys
import threading
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

    @unittest.skipIf(os.name == "nt", "private Unix socket transport fixture")
    def test_socket_ack_loss_after_claim_commit_preserves_original_recovery_scope(self):
        import provider
        sender = {**CALLER, "terminal_id": "sender", "execution": "boot:9:9"}
        with tempfile.TemporaryDirectory() as root:
            store = provider.Store(root)
            sent = store.invoke({"caller": sender, "payload": {"action": "message_send", "operation_id": "send",
                                 "recipient": CALLER, "body": "durable before ACK loss"}})["message"]
            store.db.close()
            failures = []
            with socket.socket(socket.AF_UNIX) as listener:
                path = str(Path(root) / "api.sock")
                listener.bind(path); listener.listen(); listener.settimeout(5)
                def serve():
                    observed = None
                    try:
                        observed = provider.Store(root)
                        for index in range(2):
                            peer, _ = listener.accept()
                            with peer:
                                peer.settimeout(5)
                                with peer.makefile("rb") as stream:
                                    request = json.loads(stream.readline(1024 * 1024))
                                value = observed.invoke({"caller": CALLER, "payload": request["params"]["payload"]})
                                if index == 0:
                                    peer.sendall((json.dumps(success(value)) + "\n").encode())
                                # Second call committed. Close without its admission ACK.
                    except Exception as error:
                        failures.append(error)
                    finally:
                        if observed is not None: observed.db.close()
                worker = threading.Thread(target=serve, daemon=True)
                worker.start()
                with patch.dict(os.environ, {"VQRO_SOCKET_PATH": path}):
                    status, text = cli.execute({"action": "message_claim", "operation_id": "original-claim",
                                                "expected_caller": CALLER}, True, call=cli.invoke)
                worker.join(timeout=6)
                self.assertFalse(worker.is_alive())
                self.assertEqual([], failures)
            self.assertEqual("unknown", status)
            self.assertIn("op original-claim", text)
            self.assertIn(cli.encoded(CALLER), text)
            reopened = provider.Store(root)
            try:
                receipt = reopened.invoke({"caller": CALLER, "payload": {"action": "op",
                                          "operation_id": "original-claim", "lookup_caller": CALLER}})
                self.assertTrue(receipt["authoritative"])
                self.assertEqual([sent["id"]], [row["id"] for row in receipt["outcome"]["claimed"]])
                self.assertEqual([], reopened.invoke({"caller": CALLER, "payload": {"action": "message_list"}})["waiting"])
            finally:
                reopened.db.close()

    @unittest.skipIf(os.name == "nt", "private Unix socket transport fixture")
    def test_outer_mutation_budget_leaves_inner_timeout_recovery_and_one_claim(self):
        import provider
        sender = {**CALLER, "terminal_id": "sender", "execution": "boot:9:9"}
        with tempfile.TemporaryDirectory() as root:
            store = provider.Store(root)
            sent = store.invoke({"caller": sender, "payload": {"action": "message_send", "operation_id": "send",
                                 "recipient": CALLER, "body": "committed once before inner timeout"}})["message"]
            store.db.close()
            release, failures = threading.Event(), []
            with socket.socket(socket.AF_UNIX) as listener:
                path = str(Path(root) / "api.sock")
                listener.bind(path); listener.listen(); listener.settimeout(10)
                def serve():
                    observed = None
                    try:
                        observed = provider.Store(root)
                        for index in range(2):
                            peer, _ = listener.accept()
                            with peer:
                                with peer.makefile("rb") as stream:
                                    request = json.loads(stream.readline(1024 * 1024))
                                result = observed.invoke({"caller": CALLER, "payload": request["params"]["payload"]})
                                if index == 0:
                                    peer.sendall((json.dumps(success(result)) + "\n").encode())
                                else:
                                    release.wait(16)  # Commit, but withhold ACK beyond the inner6s deadline.
                    except Exception as error:
                        failures.append(error)
                    finally:
                        if observed is not None: observed.db.close()
                worker = threading.Thread(target=serve, daemon=True)
                worker.start()
                try:
                    # The Tasks consumer uses this bounded outer15s budget. The
                    # real CLI must finish its inner timeout and print scope first.
                    result = subprocess.run([sys.executable, "-B", str(Path(cli.__file__)), "message", "claim", "--json"],
                                            env={**os.environ, "VQRO_SOCKET_PATH": path}, capture_output=True, text=True, timeout=15)
                finally:
                    release.set(); worker.join(timeout=3)
                self.assertFalse(worker.is_alive())
                self.assertEqual([], failures)
            self.assertEqual(1, result.returncode)
            self.assertEqual("", result.stdout)
            self.assertIn("outcome unknown; op msg-op-", result.stderr)
            self.assertIn("lookup first", result.stderr)
            self.assertIn(cli.encoded(CALLER), result.stderr)
            reopened = provider.Store(root)
            try:
                receipts = reopened.db.execute("SELECT operation_id,outcome FROM operations WHERE operation_id LIKE 'msg-op-%'").fetchall()
                self.assertEqual(1, len(receipts))
                self.assertIn(receipts[0]["operation_id"], result.stderr)
                self.assertEqual([sent["id"]], [row["id"] for row in json.loads(receipts[0]["outcome"])["claimed"]])
                self.assertEqual(1, reopened.db.execute("SELECT COUNT(*) FROM messages WHERE state='claimed'").fetchone()[0])
            finally:
                reopened.db.close()

    @unittest.skipIf(os.name == "nt", "Unix API deadline; Windows has its own absolute deadline")
    def test_api_uses_one_deadline_across_connect_send_and_trickled_reads(self):
        def exercise(step):
            clock, timeouts = [0.0], []
            class Stream:
                def __enter__(self): return self
                def __exit__(self, *_): pass
                def settimeout(self, value): timeouts.append(value)
                def connect(self, path): clock[0] += step
                def sendall(self, frame): clock[0] += step
                def recv(self, size):
                    clock[0] += step
                    return next(blocks)
            blocks = iter((b"{", b"}", b"\n"))
            with patch.dict(os.environ, {"VQRO_SOCKET_PATH": "/private/fake.sock"}), \
                 patch.object(cli.socket, "socket", return_value=Stream()), \
                 patch.object(cli.time, "monotonic", side_effect=lambda: clock[0]):
                result = cli.api("fixture.read", timeout=0.1)
            return result, timeouts
        result, timeouts = exercise(0.01)
        self.assertEqual({}, result)
        self.assertTrue(all(0 < right < left for left, right in zip(timeouts, timeouts[1:])))
        with self.assertRaisesRegex(TimeoutError, "API deadline elapsed"):
            exercise(0.03)

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
        self.assertEqual(cli.parse_message(["send", "task-b", "body", "--op", "id", "--caller", cli.encoded(CALLER)])[0]["target"], "task-b")
        self.assertTrue(cli.parse_message(["claim", "--idle"])[0]["idle"])
        with self.assertRaises(ValueError): cli.parse_message(["edit", "id", "body"])
        self.assertFalse(cli.parse_message(["activation", "off"])[0]["activation"])

    def test_explicit_message_operations_require_original_binding_for_every_write(self):
        commands = [["send", "recipient", "body"], ["claim"],
                    ["edit", "message", "body", "--rev", "1"], ["activation", "off"]]
        for command in commands:
            with self.subTest(command=command):
                with self.assertRaisesRegex(ValueError, "explicit --op requires --caller"):
                    cli.parse_message([*command, "--op", "original"])
                payload, mutation = cli.parse_message([*command, "--op", "original", "--caller", cli.encoded(CALLER)])
                self.assertTrue(mutation)
                self.assertEqual(CALLER, payload["expected_caller"])
                implicit, mutation = cli.parse_message(command)
                self.assertTrue(mutation)
                self.assertNotIn("operation_id", implicit)

    def test_replacement_cannot_rescope_an_explicit_message_operation(self):
        import provider
        recipient = {**CALLER, "terminal_id": "recipient", "execution": "boot:3:3"}
        current = CALLER
        calls = []
        with tempfile.TemporaryDirectory() as root:
            store = provider.Store(root)
            try:
                def call(payload):
                    calls.append(copy.deepcopy(payload))
                    return success(store.invoke({"caller": current, "payload": payload}))
                args = ["send", json.dumps(recipient), "one durable message", "--op", "original",
                        "--caller", cli.encoded(CALLER)]
                payload, mutation = cli.parse_message(args)
                status, original = cli.execute(payload, mutation, call=call)
                self.assertEqual("success", status)
                receipt = store.db.execute("SELECT outcome FROM operations WHERE operation_id='original'").fetchone()[0]
                payload, mutation = cli.parse_message(args)
                self.assertEqual(("success", original), cli.execute(payload, mutation, call=call))
                current = {**CALLER, "execution": "boot:1:replacement"}
                before = list(store.db.iterdump()), len(calls)
                with self.assertRaisesRegex(ValueError, "explicit --op requires --caller"):
                    cli.parse_message(args[:-2])
                self.assertEqual(before, (list(store.db.iterdump()), len(calls)))
                payload, mutation = cli.parse_message(args)
                status, text = cli.execute(payload, mutation, call=call)
                self.assertEqual("rejected", status)
                self.assertIn("original caller changed", text)
                self.assertEqual("whoami", calls[-1]["action"])
                lookup = store.invoke({"caller": current, "payload": {"action": "op", "operation_id": "original",
                                      "lookup_caller": CALLER}})
                self.assertTrue(lookup["authoritative"])
                self.assertEqual(original, lookup["outcome"])
                self.assertEqual(receipt, store.db.execute("SELECT outcome FROM operations WHERE operation_id='original'").fetchone()[0])
                self.assertEqual(1, store.db.execute("SELECT COUNT(*) FROM messages").fetchone()[0])
            finally:
                store.db.close()

    def test_message_send_target_can_be_an_exact_agent_binding(self):
        binding = {"class": "agent", "terminal_id": "term_a", "agent": "pi", "execution": "boot:1:2"}
        self.assertEqual(cli.resolve_recipient(json.dumps(binding)), binding)
        # The provider, not the CLI, rejects a malformed binding before any write.
        with self.assertRaises(ValueError):
            cli.resolve_recipient("{not json")

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

    def test_message_list_off_notice_preserves_json_and_shows_exact_enable_command(self):
        class Tty(io.StringIO):
            def isatty(self): return True
        for activation in (False, True, None):
            value = {"activation": activation, "waiting": [], "messages": []}
            for output, flags, json_output in ((Tty(), [], False), (io.StringIO(), [], True), (Tty(), ["--json"], True)):
                errors = io.StringIO()
                with patch("sys.stdout", output), patch("sys.stderr", errors), patch("cli.execute", return_value=("success", value)):
                    self.assertEqual(cli.main(["message", "list", *flags]), 0)
                if json_output:
                    self.assertEqual(value, json.loads(output.getvalue()))
                else:
                    self.assertEqual("(no messages)\n", output.getvalue())
                if activation is not False:
                    self.assertEqual("", errors.getvalue())
                else:
                    self.assertIn("Automatic message delivery is off", errors.getvalue())
                    self.assertIn("Waiting messages will not start agent turns", errors.getvalue())
                    self.assertIn("human shell: vqro message activation on", errors.getvalue())

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
