# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Native framing checker inversions, not public qualification."""
from python_interpreter import PYTHON
import unittest
import sys
import tempfile
from pathlib import Path
from agent_conformance.part_b.native_framing_case import (
    first_message, framed, take_point, module_tree_digest, notification_rendered, delivery_segment,
)
from agent_conformance.results import FixtureFailed, HarnessGap


class NativeFrames(unittest.TestCase):
    def test_native_notification_is_not_a_plain_user_row(self):
        row = {"type": "user", "origin": {"kind": "task-notification"},
               "message": {"content": "vqro inbox message d1 token"}}
        self.assertTrue(notification_rendered("● Stop hook feedback", [row], "d1", "token", "Stop hook feedback"))
        self.assertFalse(notification_rendered("● Stop hook feedback\n❯ token", [row], "d1", "token", "Stop hook feedback"))
        self.assertFalse(notification_rendered("● Stop hook feedback", [{**row, "origin": {"kind": "human"}}],
                                              "d1", "token", "Stop hook feedback"))
        self.assertFalse(notification_rendered("ordinary user", [row], "d1", "token", "Stop hook feedback"))
        self.assertFalse(notification_rendered("● Stop hook feedback", [], "d1", "token", "Stop hook feedback"))

    def test_native_module_seal_changes_with_dependency_bytes(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "dependency.js"
            path.write_text("first")
            before = module_tree_digest(root)
            path.write_text("second")
            self.assertNotEqual(before, module_tree_digest(root))

    def test_native_module_seal_rejects_symlinks(self):
        with tempfile.TemporaryDirectory() as root:
            try:
                (Path(root) / "escape").symlink_to(PYTHON)
            except OSError as error:
                if sys.platform == "win32" and getattr(error, "winerror", None) == 1314:
                    self.skipTest("Windows symlink fixture requires Developer Mode or symlink privilege")
                raise
            with self.assertRaises(FixtureFailed):
                module_tree_digest(root)

    def test_actual_wire_not_truncated_summary(self):
        message = {"role": "user", "content": [{"type": "tool_result", "content": [{"type": "text", "text": "token"}]}]}
        self.assertEqual(first_message([{"n": 1, "wire": {"messages": [message]}}], "token")[1], message)
        with self.assertRaises(FixtureFailed):
            first_message([{"messages": [{"text": "token"}]}], "token")

    def test_declared_binding_requires_native_container_and_wire_role(self):
        body = "<system-reminder>\nPostToolBatch hook additional context:\nvqro inbox message d1 [ref abc] from p:owner (mode boundary):\n> token\n</system-reminder>"
        valid = {"role": "user", "content": [{"type": "tool_result", "content": body}]}
        self.assertTrue(framed(valid, "system_reminder", "hook_additional_context", "turn", "token"))
        self.assertFalse(framed({**valid, "role": "system"}, "system_reminder", "hook_additional_context", "turn", "token"))
        self.assertFalse(framed({"role": "user", "content": body}, "system_reminder", "hook_additional_context", "turn", "token"))
        self.assertFalse(framed(valid, "system_reminder", "hook_additional_context", "settle", "token"))

    def test_arbitrary_wire_substring_is_not_a_declared_binding(self):
        message = {"role": "user", "content": "system-reminder"}
        with self.assertRaises(HarnessGap):
            framed(message, "user", "system-reminder", "turn", "token")

    def test_batch_selects_the_header_whose_quoted_body_has_the_token(self):
        body = ("vqro inbox message first from p:owner (mode boundary):\n> unrelated\n"
                "vqro inbox message second [ref abc] from p:owner (mode settle):\n> token\n")
        selected = delivery_segment(body, "token")
        self.assertEqual((selected["delivery"], selected["mode"]), ("second", "settle"))
        with self.assertRaises(FixtureFailed):
            delivery_segment(body.replace("unrelated", "token"), "token")

    def test_exact_point_requires_one_correlated_native_take(self):
        pi = {"event": "taken", "at": "turn", "deliveries": ["d1"]}
        cc = {"event": "taken", "detail": {"at": "idle", "deliveries": ["d2"]}}
        self.assertEqual(take_point([pi, cc], "d1"), "turn")
        self.assertEqual(take_point([pi, cc], "d2"), "idle")
        for journal in ([], [pi, pi]):
            with self.assertRaises(FixtureFailed):
                take_point(journal, "d1")


if __name__ == "__main__":
    unittest.main()
