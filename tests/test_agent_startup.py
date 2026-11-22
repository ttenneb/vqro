"""Startup is retained native metadata, not a fabricated conversation turn or inventory scan."""
import json
import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "extensions"))
import agent_transcript as transcript


class StartupCard(unittest.TestCase):
    def rows(self, startup=None, messages=None, revision=1):
        entities = {("message", "u"): (1, {"position": 1, "role": "user", "final": True,
                                             "content": [{"type": "text", "text": "hello"}]})}
        if startup is not None:
            entities[("startup", "current")] = (revision, startup)
        entities.update(messages or {})
        return transcript.rows(entities, transcript.Names({"r1": {"name": "Pi"}}, "r1"), "all_retained")

    def test_first_expanded_card_updates_in_place_and_absence_is_absence(self):
        first = self.rows({"runtime": {"name": "Pi", "version": "1.5.2", "version_source": "cli:--version"},
                           "model": "model-a"})
        self.assertEqual(first[0]["id"], "startup:current")
        self.assertTrue(first[0]["expanded"])
        self.assertIn("Pi 1.5.2", first[0]["text"])
        updated = self.rows({"runtime": {"name": "Pi"}, "skills": [{"name": "review"}],
                             "skills_scope": "loaded"}, revision=2)
        self.assertEqual(updated[0]["id"], first[0]["id"])
        self.assertEqual(sum(r.get("id") == "startup:current" for r in updated), 1)
        for absent in (None, {}, {"native": {"secret": "must never be rendered"}}, "bad"):
            self.assertFalse(any(r.get("id") == "startup:current" for r in self.rows(absent)))

    def test_user_collapse_survives_updates_and_is_local_to_the_pane(self):
        sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "extensions" / "clients"))
        import vqro_client_model as client
        first = {"items": self.rows({"model": "first-model"})}
        self.assertTrue(client.rows(first)[0]["expanded"], "a fresh pane starts open")
        collapsed = {"startup:current": False}  # existing client's per-pane user choice
        self.assertFalse(client.rows(first, open=collapsed)[0]["expanded"])
        updated = {"items": self.rows({"model": "updated-model", "skills": [{"name": "late"}]}, revision=2)}
        kept = client.rows(updated, open=collapsed)
        self.assertFalse(kept[0]["expanded"], "republishing the expanded default never overrides a user's collapse")
        self.assertFalse(any(r["item"].get("text") == "Model at start: updated-model" for r in kept))
        self.assertTrue(client.rows(updated, open={})[0]["expanded"], "another pane has its own expansion state")

    def test_retained_stream_updates_one_card_and_new_epoch_replaces_it(self):
        from vqro_items import ItemView
        stream = ItemView()
        def feed(*records):
            stream.feed("".join(json.dumps(r) + "\n" for r in records).encode())
        names = transcript.Names({"r1": {"name": "Pi"}}, "r1")
        feed({"op": "snapshot", "phase": "begin", "epoch": "e1", "coverage": "all_retained"},
             {"op": "put", "kind": "startup", "id": "current", "rev": 1, "value": {"model": "first"}},
             {"op": "snapshot", "phase": "end", "seq": 0})
        first = transcript.rows(stream.entities, names)[0]
        feed({"op": "put", "seq": 1, "kind": "startup", "id": "current", "rev": 2,
              "value": {"model": "first", "skills": [{"name": "late"}]}})
        later = transcript.rows(stream.entities, names)
        self.assertEqual(len(later), 1)
        self.assertEqual(later[0]["id"], first["id"])
        self.assertIn("1 skill", later[0]["text"])
        feed({"op": "snapshot", "phase": "begin", "epoch": "e2", "coverage": "all_retained"},
             {"op": "snapshot", "phase": "end", "seq": 0})
        self.assertEqual(transcript.rows(stream.entities, names), [], "no previous-epoch startup survives")

    def test_pi_sections_sources_and_paths_do_not_guess_missing_inventory(self):
        card = self.rows({"runtime": {"name": "Pi", "version": "1.5.2", "version_source": "cli:--version"},
                          "model": "Luna", "cwd": "/remote/project", "skills_scope": "loaded",
                          "skills": [{"name": "review", "description": "Review changes", "path": "/remote/skills/review.md"}],
                          "prompts": [{"name": "check", "path": "/remote/prompts/check.md"}],
                          "commands": [{"name": "notes", "kind": "extension", "path": "/remote/ext/notes.ts"}],
                          "sources": {"cwd": "vqro:launch", "skills": "rpc:get_commands"}})[0]
        encoded = json.dumps(card, ensure_ascii=False)
        self.assertIn("Launch folder", encoded)
        self.assertIn("vqro launch record", encoded)
        self.assertIn("--version", encoded)
        self.assertIn("Loaded skills", encoded)
        self.assertIn("Prompt templates", encoded)
        self.assertIn("Extension commands", encoded)
        self.assertNotIn("Loaded instruction", encoded)
        self.assertNotIn("Themes", encoded)
        self.assertNotIn("links.open", encoded, "a remote-looking native path is not a local open route")
        paths = [r for r in card["children"] if r.get("style") == "link"]
        self.assertTrue(any(r.get("copy") == "/remote/skills/review.md" for r in paths))
        self.assertTrue(all(a["command"] == "client.copy" for r in paths for a in r.get("actions", [])))
        self.assertIn("Ctrl-g", encoded)

    def test_codex_inventory_labels_are_not_loaded_claims(self):
        card = self.rows({"runtime": {"name": "Codex", "version": "native identity string", "version_source": "rpc:initialize"},
                          "instructions": [{"path": "/remote/AGENTS.md"}],
                          "skills": [{"name": "review"}], "skills_scope": "available",
                          "plugins": [{"name": "enabled-plugin", "enabled": True, "version": "1.3"},
                                      {"name": "disabled-plugin", "enabled": False}],
                          "mcp": [{"name": "docs", "status": "connected"}, {"name": "files", "status": "native_future_status"}],
                          "sources": {"plugins": "rpc:plugin/installed"}})[0]
        encoded = json.dumps(card, ensure_ascii=False)
        self.assertIn("Loaded instruction sources", encoded)
        self.assertIn("Available skills", encoded)
        self.assertNotIn("Loaded skills", encoded)
        self.assertIn("Installed plugins", encoded)
        self.assertIn("disabled", encoded)
        self.assertIn("native_future_status", encoded, "MCP status is not reclassified")
        self.assertIn("native identity string", encoded)
        self.assertNotIn("Loaded plugins", encoded)

    def test_partial_or_failed_inventory_is_not_reported_as_zero(self):
        card = self.rows({"skills_scope": "available", "incomplete": ["skills", "plugins"],
                          "commands": [{"name": "one"}], "native": {"secret": "must not leak"}})[0]
        encoded = json.dumps(card, ensure_ascii=False)
        self.assertIn("incomplete", encoded.lower())
        self.assertIn("skills", encoded.lower())
        self.assertNotIn("0 skills", encoded)
        self.assertNotIn("secret", encoded)
        self.assertNotIn("must not leak", encoded)

    def test_startup_and_transcript_share_the_panel_budget(self):
        huge = {"skills": [{"name": "x" * 128, "description": "d" * 1024,
                            "path": "/" + "p" * 4095} for _ in range(256)]}
        messages = {("message", str(i)): (1, {"position": i + 2, "role": "assistant", "final": True,
                     "content": [{"type": "text", "text": "body " * 1000}]}) for i in range(50)}
        rows = self.rows(huge, messages)
        self.assertEqual(rows[0]["id"], "startup:current")
        self.assertLessEqual(len(json.dumps(rows, ensure_ascii=False).encode()), transcript.BUDGET)
        self.assertIn("pane limit", json.dumps(rows[0]).lower())
        self.assertTrue(any("body" in r.get("text", "") for r in rows[1:]), "startup never crowds out all conversation")

    def test_overbound_native_paths_are_omitted_never_shortened_into_copy_targets(self):
        oversized = "/" + "x" * 4096
        card = self.rows({"cwd": oversized, "sources": {"cwd": "vqro:launch"},
                          "instructions": [{"path": oversized}],
                          "skills": [{"name": "retained-name", "path": oversized}]})[0]
        encoded = json.dumps(card)
        self.assertNotIn("x" * 100, encoded, "not even a misleading truncated path prefix is published")
        self.assertIn("retained-name", encoded)
        self.assertIn("Incomplete", encoded)
        self.assertFalse(any("copy" in row for row in card["children"]))
        exact = "/" + "界" * 1000
        valid = self.rows({"instructions": [{"path": exact}]})[0]
        copied = next(row["copy"] for row in valid["children"] if "copy" in row)
        self.assertEqual(copied, exact, "display may clip, but copy target is the entire exact native path")
        unknown = json.dumps(self.rows({"cwd": "/native/path"})[0])
        self.assertIn("source not reported", unknown)
        self.assertNotIn("vqro launch record", unknown)

    def test_overbound_names_descriptions_and_status_fields_are_omitted(self):
        card = self.rows({"skills": [
            {"name": "界" * 43, "path": "/must-not-survive-name-rejection"},
            {"name": "retained", "description": "d" * 1025, "path": "/exact/native/path"}],
            "commands": [{"name": "cmd", "kind": "k" * 65}],
            "mcp": [{"name": "server", "status": "s" * 65}]})[0]
        encoded = json.dumps(card, ensure_ascii=False)
        self.assertNotIn("must-not-survive", encoded)
        self.assertNotIn("d" * 100, encoded)
        self.assertNotIn("s" * 64, encoded)
        self.assertIn("retained", encoded)
        self.assertIn("/exact/native/path", encoded)
        self.assertIn("skills", encoded)
        self.assertIn("incomplete", encoded.lower())
        original = {"skills": [{"name": "retained", "description": "d" * 1025}]}
        transcript.startup_row(original)
        self.assertEqual(len(original["skills"][0]["description"]), 1025, "projection never mutates native facts")

    def test_malformed_optional_fields_do_not_break_the_conversation(self):
        for value in ({"skills": [None, 1, {"name": []}], "runtime": []},
                      {"sources": [], "incomplete": [[], 3]}, {"model": {}, "cwd": False}):
            rows = self.rows(value)
            self.assertTrue(any(r.get("text") == "hello" or r.get("body") == "hello" for r in rows))
