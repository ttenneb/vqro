#!/usr/bin/env python3
"""Offline projection tests; no model/server or installed configuration."""
import importlib.util
from pathlib import Path
import unittest


def provider(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).parent / name / "service.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class PublicFactsTests(unittest.TestCase):
    def test_activity_label_comes_from_explicit_public_metadata(self):
        snapshot = {"generation": 7, "revision": 8, "activities": [{
            "activity": "idle", "terminal": {"terminal_id": "term_a"}, "state_change_seq": 1,
            "source": "vqro:pi", "labels": {"display": "Pi", "state_idle": "role loaded at Pi hook"},
        }]}
        document = provider("public-activity").project(snapshot)
        self.assertEqual(document["namespace"], "example.public-activity")
        self.assertEqual(document["generation"], 7)
        self.assertEqual(document["items"][1]["label"], "Pi · role loaded at Pi hook")
        self.assertEqual(document["items"][1]["terminal"], snapshot["activities"][0]["terminal"])
        self.assertEqual(document["actions"], [])
        snapshot["activities"][0]["labels"]["display"] = "é" * 1000
        self.assertLessEqual(len(provider("public-activity").project(snapshot)["items"][1]["label"].encode()), 240)

    def test_relationships_are_fixture_owned_not_runtime_delegations(self):
        terminal = {"terminal_id": "term_a", "placement": {"pane_id": "pane_1", "workspace_id": "ws_1", "tab_id": "tab_1"}}
        snapshot = {"generation": 1, "revision": 2, "scope_id": "tab_1",
                    "ungrouped": [{"terminal": terminal, "lifecycle": "live", "label": "irrelevant"}],
                    "relationships": [{"relationship_id": "host-secret", "purpose": "Do not make this host policy"}]}
        document = provider("public-relationships").project(snapshot)
        self.assertEqual(document["namespace"], "example.public-relationships")
        self.assertEqual(document["items"][1]["parent_id"], "context:task.parent:fixture-task")
        self.assertEqual(document["items"][1]["terminal"], terminal)
        self.assertEqual(document["items"][1]["label"], "Owner context")
        self.assertEqual(document["target"], {"kind": "tab", "tab_id": "tab_1"})
        self.assertNotIn("host-secret", str(document))
        self.assertNotIn("Do not make this host policy", str(document))
        self.assertEqual(document["actions"], [])
        snapshot["ungrouped"][0]["lifecycle"] = "archived"
        self.assertEqual(provider("public-relationships").project(snapshot)["items"], [])


if __name__ == "__main__":
    unittest.main()
