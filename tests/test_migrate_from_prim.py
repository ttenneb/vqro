import importlib.util
import json
import os
from pathlib import Path
import socket
import stat
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "scripts/migrate-from-prim.py"
spec = importlib.util.spec_from_file_location("migration", SCRIPT)
migration = importlib.util.module_from_spec(spec)
spec.loader.exec_module(migration)


class MigrationTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        # Migration deliberately rewrites literal canonical source prefixes.
        self.root = Path(self.tmp.name).resolve()
        self.source, self.target = self.root / "old", self.root / "new"
        self.source.mkdir()
        self.old_ext, self.new_ext = self.root / "old-ext", self.root / "new-ext"
        self.data = {"rev": 7, "next_txn": 11, "entries": {}, "history": []}

    def entry(self, key, value):
        self.data["entries"][key] = {"version": 7, "value": value}

    def prepare(self):
        (self.source / "workspace.json").write_text(json.dumps(self.data))
        (self.source / ".workspace.lock").touch()
        for name in ("resources.json", "runtime-tokens.json"):
            (self.source / name).write_bytes(b'{"id":"r42","opaque":"prim.user-token"}\n')
        return {path.name: path.read_bytes() for path in self.source.iterdir()}

    def run_migration(self):
        return migration.migrate(self.source, self.target, self.old_ext, self.new_ext)

    def migrated(self):
        return json.loads((self.target / "workspace.json").read_text())

    def test_known_state_history_and_identity_files(self):
        self.entry("doc/prim/defaults", {"version": 2})
        self.entry("binding/recovery", {"command": "prim.revert", "args": {"txn": 3}})
        self.entry("menu/recovery", {"items": [{"command": "prim.safe_mode.allow"}]})
        self.entry("doc/keys/profile", {"profile": "prim"})
        self.entry("doc/user", {"text": "prim project: primitive primary", "cwd": "/projects/prim"})
        self.entry("layout/recovery", {"view": "prim-depth-quarantine--"})
        self.entry("view/prim-depth-quarantine--", {"kind": "panel"})
        self.data["history"] = [{"id": 4, "author": "prim.client", "note": "prim.revert", "at_ms": 2,
            "changes": [{"key": "doc/prim/defaults", "before": {"version": 2,
                         "value": {"command": "prim.shutdown", "ref": "doc/prim/other"}},
                         "after": {"version": 7, "value": {"command": "prim.revert"}}}]}]
        before = self.prepare()
        report = self.run_migration()
        result = self.migrated()
        self.assertEqual(result["rev"], 7)
        self.assertEqual(result["next_txn"], 11)
        self.assertEqual(result["entries"]["doc/vqro/defaults"], {"version": 7, "value": {"version": 2}})
        self.assertEqual(result["entries"]["binding/recovery"]["value"]["command"], "vqro.revert")
        self.assertEqual(result["entries"]["doc/keys/profile"]["value"]["profile"], "vqro")
        self.assertEqual(result["entries"]["doc/user"], self.data["entries"]["doc/user"])
        self.assertEqual(result["entries"]["layout/recovery"]["value"]["view"], "vqro-depth-quarantine--")
        self.assertIn("view/vqro-depth-quarantine--", result["entries"])
        txn = result["history"][0]
        self.assertEqual((txn["id"], txn["author"], txn["note"], txn["at_ms"]), (4, "prim.client", "prim.revert", 2))
        self.assertEqual(txn["changes"][0]["key"], "doc/vqro/defaults")
        self.assertEqual(txn["changes"][0]["before"]["value"], {"command": "vqro.shutdown", "ref": "doc/vqro/other"})
        self.assertEqual(txn["changes"][0]["after"]["value"]["command"], "vqro.revert")
        for name, data in before.items():
            self.assertEqual((self.source / name).read_bytes(), data)
            if name not in {"workspace.json", ".workspace.lock"}:
                self.assertEqual((self.target / name).read_bytes(), data)
        self.assertEqual(report["symlinks_created"], 0)
        self.assertEqual(set(report["unchanged_identity_files"]), {"resources.json", "runtime-tokens.json"})
        if os.name != "nt":
            self.assertEqual(stat.S_IMODE(self.target.stat().st_mode), 0o700)
            self.assertEqual(stat.S_IMODE((self.target / "workspace.json").stat().st_mode), 0o600)

    def test_literal_executables_sdk_paths_env_and_profile_arguments(self):
        self.entry("extension/herdr", {"argv": ["/old/bin/prim-herdr", "--hc-arg", "a b"]})
        self.entry("extension/tmux", {"argv": ["prim-tmux"]})
        self.entry("extension/sdk", {"argv": ["python3", str(self.old_ext / "prim_ext.py")],
            "env": {"PRIM_SOCKET": str(self.source / "prim.sock"), "USER_VAR": "unchanged",
                    "PYTHONPATH": str(self.old_ext) + os.pathsep + "/user/python"}})
        self.entry("doc/settings/pi", {"runtimes": [{"bridge_argv": ["python3", "-m", "prim_provider"],
                                                       "bridge_env": {"PRIM_CONTROL_INSTANCE": "opaque"}}]})
        self.entry("binding/profile", {"command": "keys.profile", "args": {"value": "prim"}})
        self.entry("doc/user", {"file": str(self.source / "prim_ext.py"), "text": "prim.revert please"})
        self.prepare()
        self.run_migration()
        entries = {key: row["value"] for key, row in self.migrated()["entries"].items()}
        self.assertEqual(entries["extension/herdr"]["argv"], ["vqro-herdr", "--hc-arg", "a b"])
        self.assertEqual(entries["extension/tmux"]["argv"], ["vqro-tmux"])
        self.assertEqual(entries["extension/sdk"]["argv"], ["python3", str(self.new_ext / "vqro_ext.py")])
        self.assertEqual(entries["extension/sdk"]["env"], {"VQRO_SOCKET": str(self.target / "vqro.sock"),
            "USER_VAR": "unchanged", "PYTHONPATH": str(self.new_ext) + os.pathsep + "/user/python"})
        runtime = entries["doc/settings/pi"]["runtimes"][0]
        self.assertEqual(runtime["bridge_argv"], ["python3", "-m", "vqro_provider"])
        self.assertEqual(runtime["bridge_env"], {"VQRO_CONTROL_INSTANCE": "opaque"})
        self.assertEqual(entries["binding/profile"]["args"]["value"], "vqro")
        self.assertEqual(entries["doc/user"], {"file": str(self.target / "prim_ext.py"), "text": "prim.revert please"})

    def test_existing_target_even_empty_is_refused(self):
        self.prepare()
        self.target.mkdir()
        with self.assertRaisesRegex(migration.MigrationError, "already exists"):
            self.run_migration()
        self.assertEqual(list(self.target.iterdir()), [])

    def test_workspace_collision_is_refused_before_copy(self):
        self.entry("doc/prim/x", 1)
        self.entry("doc/vqro/x", 2)
        self.prepare()
        with self.assertRaisesRegex(migration.MigrationError, "collision"):
            self.run_migration()
        self.assertFalse(self.target.exists())

    def test_nested_dictionary_collision_is_refused_before_copy(self):
        self.entry("menu/x", {"prim.revert": 1, "vqro.revert": 2})
        self.prepare()
        with self.assertRaisesRegex(migration.MigrationError, "collision"):
            self.run_migration()
        self.assertFalse(self.target.exists())

    def test_environment_collision_is_refused(self):
        self.entry("extension/x", {"env": {"PRIM_SOCKET": "old", "VQRO_SOCKET": "new"}})
        self.prepare()
        with self.assertRaisesRegex(migration.MigrationError, "collision"):
            self.run_migration()
        self.assertFalse(self.target.exists())

    @unittest.skipIf(os.name == "nt", "Unix state-lock implementation")
    def test_locked_source_is_refused_without_contacting_core(self):
        import fcntl
        self.prepare()
        with (self.source / ".workspace.lock").open("r+b") as handle:
            fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
            with self.assertRaisesRegex(migration.MigrationError, "old core is running"):
                self.run_migration()
        self.assertFalse(self.target.exists())

    @unittest.skipIf(os.name == "nt", "Unix socket fixture")
    def test_transient_files_are_not_copied(self):
        self.prepare()
        (self.source / "serve.pid").write_text("1")
        with socket.socket(socket.AF_UNIX) as endpoint:
            endpoint.bind(str(self.source / "prim.sock"))
            report = self.run_migration()
        self.assertEqual(set(report["skipped_transient_files"]), {"serve.pid", ".workspace.lock", "prim.sock"})
        self.assertFalse((self.target / "serve.pid").exists())
        self.assertFalse((self.target / ".workspace.lock").exists())
        self.assertFalse((self.target / "prim.sock").exists())

    @unittest.skipIf(os.name == "nt", "symlink fixture permissions vary on Windows")
    def test_symlinks_are_refused_not_followed_or_created(self):
        self.prepare()
        outside = self.root / "outside"
        outside.write_text("private")
        (self.source / "link").symlink_to(outside)
        with self.assertRaisesRegex(migration.MigrationError, "symlink"):
            self.run_migration()
        self.assertFalse(self.target.exists())
        self.assertEqual(outside.read_text(), "private")

    def test_malformed_workspace_refuses_before_target_creation(self):
        self.prepare()
        (self.source / "workspace.json").write_text("{")
        with self.assertRaises(ValueError):
            self.run_migration()
        self.assertFalse(self.target.exists())

    def test_target_inside_source_is_refused(self):
        self.prepare()
        self.target = self.source / "nested"
        with self.assertRaisesRegex(migration.MigrationError, "outside"):
            self.run_migration()

    def test_cli_prints_counts_not_credentials(self):
        self.entry("binding/x", {"command": "prim.revert"})
        self.prepare()
        result = subprocess.run([sys.executable, str(SCRIPT), "--source", str(self.source),
                                 "--target", str(self.target)], capture_output=True, text=True, check=True)
        report = json.loads(result.stdout)
        self.assertTrue(report["old_state_untouched"])
        self.assertEqual(report["rewrites"]["commands"], 1)
        self.assertNotIn("prim.user-token", result.stdout)


if __name__ == "__main__":
    unittest.main()
