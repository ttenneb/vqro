# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Pull checker regressions, not public-Core qualification."""
import unittest
from agent_conformance.part_b.cases import user_pull
from agent_conformance.part_b.pull_followup_cases import target_wakes


class WakeWitness(unittest.TestCase):
    def test_only_actual_target_publications_are_selected(self):
        content = {"wake": {"seq": 2, "pending": {"r1": 1}}, "control_revision": 3}
        event = {"type": "view.content", "data": {"view": "coord-status", "content": content}}
        self.assertEqual(target_wakes([event], "r1"), [content])
        self.assertEqual(target_wakes([event], "r2"), [])
        self.assertEqual(target_wakes([{**event, "type": "effect"}], "r1"), [])
        self.assertEqual(target_wakes([{**event, "data": {**event["data"], "view": "other"}}], "r1"), [])

    def test_absent_wake_never_becomes_an_observation(self):
        self.assertEqual(target_wakes([{}, {"type": "view.content", "data": {"view": "coord-status", "content": {}}}], "r1"), [])

    def test_profile_uses_independent_declared_point_maps(self):
        pull = user_pull()
        self.assertEqual(set(pull["points"]), {"turn", "settle", "idle", "tool"})
        self.assertTrue(all(point == {"precision": "exact"} for point in pull["points"].values()))
        self.assertEqual(set(pull["framing"]), set(pull["points"]))
        pull["points"]["idle"]["precision"] = "approximate"
        self.assertEqual(pull["points"]["turn"]["precision"], "exact")


if __name__ == "__main__":
    unittest.main()
