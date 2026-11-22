"""Offline mocks and private file fixtures for the integration harness."""
import json
import os
from pathlib import Path
import socket
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
import vqro_testing as harness


class SandboxTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="vqi-")
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = self.root / "source"
        self.source.mkdir()
        (self.source / "example.py").write_text("# fixture\n")
        self.snapshot = {"entries": {"extension/example": {"version": 1, "value": {
            "argv": ["python3", str(self.source / "example.py")], "enabled": True}}}}

    def sandbox(self, **kwargs):
        sandbox = harness.Sandbox(self.source, self.snapshot, root=self.root / "run", **kwargs)
        self.addCleanup(sandbox.cleanup)
        return sandbox

    def test_copy_rebases_launch_paths_and_scrubs_environment(self):
        with patch.dict(os.environ, {"VQRO_SOCKET": "/live/socket", "HC_SOCKET": "/live/hc", "HERDR_PANE_ID": "real", "TMUX": "real"}):
            sandbox = self.sandbox().prepare()
        value = sandbox.declarations["extension/example"]
        self.assertEqual(value["argv"][1], str(sandbox.ext / "example.py"))
        self.assertEqual(value["cwd"], str(sandbox.ext))
        self.assertEqual(sandbox.info["canary"]["status"], "pass")
        self.assertFalse(any(key.startswith(("HERDR_", "TMUX")) for key in sandbox.env))
        self.assertNotEqual(sandbox.env["HC_SOCKET"], "/live/hc")
        self.assertFalse(sandbox.info["started"])
        self.assertEqual((self.source / "example.py").read_text(), "# fixture\n")

    def test_credential_entries_contributions_and_resources_are_excluded(self):
        for key, value in {"doc/settings/x": {"token": "sensitive"}, "doc/auth": {},
                           "doc/owned": {"owner": "example"}, "view/x": {"kind": "panel"},
                           "layout/x": {}, "doc/good": {"value": 4}}.items():
            self.snapshot["entries"][key] = {"version": 1, "value": value}
        sandbox = self.sandbox().prepare()
        self.assertEqual(set(sandbox.values), {"extension/example", "doc/good"})
        self.assertIn("doc/settings/x", sandbox.info["excluded"])
        self.assertIn("doc/owned", sandbox.info["excluded"])

    def test_copy_excludes_external_symlinks_and_credential_files(self):
        external = self.root / "outside"
        external.write_text("private")
        (self.source / "link").symlink_to(external)
        (self.source / "token.json").write_text("private")
        sandbox = self.sandbox().prepare()
        self.assertFalse((sandbox.ext / "link").exists())
        self.assertFalse((sandbox.ext / "token.json").exists())

    def test_fake_disables_providers(self):
        self.snapshot["entries"]["extension/example"]["value"].update(kind="provider")
        sandbox = self.sandbox().prepare()
        self.assertFalse(sandbox.declarations["extension/example"]["enabled"])
        self.assertEqual(sandbox.env["VQRO_HERDR"], '["/bin/false"]')
        self.assertEqual(sandbox.enabled(), [])

    def test_external_path_kept_off_unless_explicitly_allowed(self):
        self.snapshot["entries"]["extension/example"]["value"]["argv"][1] = str(self.root / "external.py")
        sandbox = self.sandbox().prepare()
        self.assertEqual(sandbox.audit_rows[0]["status"], "unverified")
        self.assertFalse(sandbox.declarations["extension/example"]["enabled"])
        sandbox.cleanup()
        allowed = self.sandbox(allow_external=True).prepare()
        self.assertTrue(allowed.declarations["extension/example"]["enabled"])

    def test_socket_bound_fails_before_core_start(self):
        sandbox = harness.Sandbox(self.source, self.snapshot, root=self.root / ("deep" * 30))
        self.addCleanup(sandbox.cleanup)
        with patch.object(harness.subprocess, "Popen") as launch:
            with self.assertRaisesRegex(ValueError, "socket path"):
                sandbox.prepare()
            launch.assert_not_called()
        self.assertEqual(sandbox.info["canary"]["status"], "fail")
        self.assertFalse(sandbox.info["started"])

    def test_existing_nonempty_root_is_never_removed(self):
        root = self.root / "run"
        root.mkdir()
        marker = root / "keep"
        marker.write_text("user file")
        sandbox = self.sandbox()
        with self.assertRaisesRegex(ValueError, "absent or empty"):
            sandbox.prepare()
        sandbox.cleanup()
        self.assertTrue(marker.exists())

    def test_candidate_overlay_and_ops_are_audited(self):
        candidate = self.root / "candidate"
        candidate.mkdir()
        (candidate / "example.py").write_text("# candidate\n")
        sandbox = self.sandbox(candidate_ext=candidate, candidate_ops=[
            {"op": "set", "key": "doc/test", "value": {"candidate": True}}]).prepare()
        self.assertEqual((sandbox.ext / "example.py").read_text(), "# candidate\n")
        self.assertEqual(sandbox.values["doc/test"], {"candidate": True})
        self.assertEqual((self.source / "example.py").read_text(), "# fixture\n")

    def test_candidate_cannot_seed_credentials_or_resources(self):
        sandbox = self.sandbox(candidate_ops=[{"op": "set", "key": "doc/x", "value": {"password": "private"}}])
        with self.assertRaisesRegex(ValueError, "credential-free"):
            sandbox.prepare()

    def test_sandbox_client_rejects_external_socket_before_connect(self):
        with patch.object(harness.socket, "socket") as factory:
            with self.assertRaisesRegex(ValueError, "outside root"):
                harness.SandboxClient(self.root / "external", self.root / "run")
            factory.assert_not_called()

    def test_apply_fills_compare_and_set(self):
        client = object.__new__(harness.SandboxClient)
        client.call = Mock(side_effect=[{"entries": {"doc/x": {"version": 8}}}, {"txn": 9}])
        result = client.apply([{"op": "set", "key": "doc/x", "value": 4}, {"op": "set", "key": "doc/new", "value": 2}])
        self.assertEqual(result, {"txn": 9})
        self.assertEqual([op["expect"] for op in client.call.call_args.args[1]["ops"]], [8, 0])

    def test_lifetime_survivors_reported_then_only_owned_group_signalled(self):
        sandbox = self.sandbox().prepare()
        sandbox.groups = [54321]
        survivor = [{"pid": 54322, "command": "uncooperative fixture"}]
        with patch.object(harness, "process_group_members", return_value=survivor), patch.object(harness.time, "monotonic", side_effect=[0, 20]), patch.object(harness.time, "sleep"), patch.object(harness.os, "killpg") as kill:
            sandbox.stop()
        self.assertEqual(sandbox.leftovers, survivor)
        self.assertTrue(sandbox.leftovers)
        self.assertEqual([call.args[0] for call in kill.call_args_list], [54321, 54321])

    def test_process_facts_exclude_other_groups_and_zombies(self):
        output = "12 12 S core\n13 12 S extension\n14 12 Z zombie\n21 21 S unrelated\n"
        with patch.object(harness.subprocess, "run", return_value=Mock(stdout=output)):
            self.assertEqual([p["pid"] for p in harness.process_group_members(12, {})], [12, 13])

    def test_smoke_checks_registration_views_recovery_without_extension_commands(self):
        sandbox = self.sandbox().prepare()
        fact = {"display_status": "running", "crashes": 0, "generation": 1,
                "dependencies": {"status": "ok"}, "manifest": {"provides": ["command/example.show", "view/example.panel"]}}
        sandbox.wait_running = Mock(return_value={"example": fact})
        sandbox.stop = Mock()
        sandbox.start = Mock()
        client = Mock()
        sandbox.client = client
        client.call.side_effect = lambda method: {
            "extension.list": {"extensions": {"example": fact}},
            "workspace.snapshot": {"entries": {"extension/example": {"value": sandbox.declarations["extension/example"]},
                                               "view/example.panel": {"value": {}}}, "live": {}},
            "command.list": {"commands": [{"name": "example.show", "available": True}]}}[method]
        client.apply.return_value = {"txn": 42}
        rows = harness.smoke(sandbox, window=0)
        indexed = {r["id"]: r for r in rows}
        self.assertEqual(indexed["start/example"]["status"], "pass")
        self.assertEqual(indexed["commands/example"]["status"], "pass")
        self.assertEqual(indexed["views/example"]["status"], "pass")
        self.assertEqual(indexed["recovery/restart"]["status"], "pass")
        self.assertEqual([c.args[0] for c in client.invoke.call_args_list], ["vqro.safe_mode", "vqro.safe_mode", "vqro.revert"])
        fact["manifest"]["provides"].append("command/not.registered")
        indexed = {r["id"]: r for r in harness.smoke(sandbox, window=0)}
        self.assertEqual(indexed["commands/example"]["status"], "fail")
        sandbox.stop = Mock()

    def test_smoke_zero_enabled_is_failure(self):
        sandbox = self.sandbox().prepare()
        sandbox.declarations["extension/example"]["enabled"] = False
        self.assertEqual(harness.smoke(sandbox)[0]["status"], "fail")


if __name__ == "__main__":
    unittest.main()
