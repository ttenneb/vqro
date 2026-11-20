# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Native framing checker inversions, not public qualification."""
import unittest
import tempfile
from pathlib import Path
from agent_conformance.part_b.native_framing_case import first_message, framed, take_point, module_tree_digest
from agent_conformance.results import FixtureFailed
from coord_harness import pull_declaration


class NativeFrames(unittest.TestCase):
    def test_native_module_seal_changes_with_dependency_bytes(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "dependency.js"
            path.write_text("first")
            before = module_tree_digest(root)
            path.write_text("second")
            self.assertNotEqual(before, module_tree_digest(root))
            (Path(root) / "escape").symlink_to("/usr/bin/python3")
            with self.assertRaises(FixtureFailed): module_tree_digest(root)

    def test_actual_wire_not_truncated_summary(self):
        message = {"role": "user", "content": [{"type": "tool_result", "content": [{"type": "text", "text": "token"}]}]}
        self.assertEqual(first_message([{"n": 1, "wire": {"messages": [message]}}], "token")[1], message)
        with self.assertRaises(FixtureFailed): first_message([{"messages": [{"text": "token"}]}], "token")

    def test_header_and_role_and_added_wrapper_are_all_required(self):
        body = "<system-reminder>vqro inbox message d1 from p:owner (mode boundary): token</system-reminder>"
        valid = {"role": "user", "content": body}
        self.assertTrue(framed(valid, "user", "system-reminder"))
        self.assertFalse(framed({**valid, "role": "system"}, "user", "system-reminder"))
        self.assertFalse(framed({**valid, "content": "token"}, "user", "system-reminder"))
        self.assertFalse(framed(valid, "user", "NOT USER INPUT"))

    def test_exact_point_requires_one_correlated_native_take(self):
        pi = {"event": "taken", "at": "turn", "deliveries": ["d1"]}
        cc = {"event": "taken", "detail": {"at": "idle", "deliveries": ["d2"]}}
        self.assertEqual(take_point([pi, cc], "d1"), "turn")
        self.assertEqual(take_point([pi, cc], "d2"), "idle")
        for journal in ([], [pi, pi]):
            with self.assertRaises(FixtureFailed): take_point(journal, "d1")

    def test_declarations_do_not_infer_exact_idle_for_claude(self):
        pi, cc = pull_declaration("pi"), pull_declaration("claude")
        self.assertEqual(pi["points"]["idle"]["precision"], "exact")
        self.assertEqual(cc["points"]["idle"]["precision"], "approximate")
        self.assertIn("NOT USER INPUT", cc["framing"]["idle"]["frame"])
        self.assertNotEqual(cc["framing"]["turn"], cc["framing"]["idle"])
        with self.assertRaises(ValueError): pull_declaration("unknown")


if __name__ == "__main__":
    unittest.main()
