"""Powerless Windows diagnostic/fixture controls; no core, backend or native API calls."""
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "extensions"))
import vqro_testing as shared
from agent_conformance import evidence, inventory
from agent_conformance.results import HarnessGap
import pi_provider_e2e as e2e


class WindowsDiagnostics(unittest.TestCase):
    def test_plain_client_passes_opaque_pipe_to_shared_connector(self):
        endpoint = r"\\.\pipe\private-fixture"
        handle = Mock()
        with patch.object(shared, "connect_endpoint", return_value=handle) as connect:
            client = shared.PlainClient(endpoint, timeout=3)
        connect.assert_called_once_with(endpoint, timeout=3)
        handle.makefile.assert_called_once_with("rwb", buffering=0)
        client.close()
        handle.close.assert_called_once_with()

    def test_windows_sandbox_refuses_before_any_filesystem_or_process_effect(self):
        with patch.object(shared.sys, "platform", "win32"), patch.object(shared, "Path") as path, \
                patch.object(shared.subprocess, "Popen") as spawn:
            for construct in (lambda: shared.Sandbox("source", {}), lambda: shared.SandboxClient("endpoint", "root")):
                with self.subTest(construct=construct), self.assertRaisesRegex(ValueError, "not supported on native Windows yet"):
                    construct()
            path.assert_not_called()
            spawn.assert_not_called()

    def test_windows_receipt_is_utf8_without_posix_permission_claim(self):
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / "receipt.json"
            with patch.object(shared.sys, "platform", "win32"), \
                    patch.object(shared.os, "fchmod", create=True) as chmod:
                shared.write_receipt({"note": "café λ"}, destination)
            chmod.assert_not_called()
            self.assertIn("café λ".encode("utf-8"), destination.read_bytes())
            self.assertEqual(json.loads(destination.read_bytes()), {"note": "café λ"})

    def test_fake_backend_command_is_portable_and_always_fails(self):
        with tempfile.TemporaryDirectory() as directory, patch.object(shared.sys, "platform", "linux"):
            sandbox = shared.Sandbox(directory, {}, binary=sys.executable, root=Path(directory) / "sandbox")
            self.assertEqual(json.loads(sandbox.env["VQRO_HERDR"]),
                             [sys.executable, "-c", "raise SystemExit(1)"])


class FixtureEncodingAndImports(unittest.TestCase):
    def test_exported_contract_fallback_reads_utf8_explicitly(self):
        read = Path.read_text
        def checked_read(path, *args, **kwargs):
            self.assertEqual(kwargs.get("encoding"), "utf-8")
            return read(path, *args, **kwargs)
        with patch.object(inventory.subprocess, "run", side_effect=OSError("git unavailable")), \
                patch.object(inventory.Path, "read_text", autospec=True, side_effect=checked_read):
            contract = inventory.main_contract()
        self.assertEqual(contract.source, "bundled-unverified")
        self.assertTrue(contract.ids)

    def test_windows_git_helpers_keep_native_tool_path_but_drop_live_environment(self):
        source = (ROOT / inventory.SPEC).read_text(encoding="utf-8")
        replies = [Mock(stdout=b"a" * 40 + b"\n"), Mock(stdout=source.encode("utf-8"))]
        with patch.object(sys, "platform", "win32"), \
                patch.dict(inventory.os.environ, {"PATH": "native-git-directory", "HERDR_ENV": "canary"}), \
                patch.object(inventory.subprocess, "run", side_effect=replies) as run:
            inventory.main_contract()
            for call in run.call_args_list:
                self.assertEqual(call.kwargs["env"], {"PATH": "native-git-directory", "GIT_CONFIG_NOSYSTEM": "1"})
        with patch.object(sys, "platform", "win32"), \
                patch.dict(evidence.os.environ, {"PATH": "native-git-directory", "HERDR_ENV": "canary"}), \
                patch.object(evidence.subprocess, "run", return_value=Mock(stdout=b"fixture")) as run:
            self.assertEqual(evidence.git("fixture", "status"), b"fixture")
            self.assertEqual(run.call_args.kwargs["env"], {"PATH": "native-git-directory", "GIT_CONFIG_NOSYSTEM": "1"})

    def test_cli_diagnostic_capture_decodes_utf8_explicitly(self):
        harness = object.__new__(e2e.E2E)
        harness.root, harness.env, harness.log = Path("private-fixture"), {}, []
        result = Mock(stdout='{"note":"café λ"}', stderr="", returncode=0)
        with patch.object(e2e.subprocess, "run", return_value=result) as run:
            self.assertEqual(harness.command(["fixture"]), {"note": "café λ"})
        self.assertEqual(run.call_args.kwargs["encoding"], "utf-8")

    def test_pty_harness_import_does_not_require_unix_modules_on_windows(self):
        name = "agent_conformance.part_b._windows_launcher_fixture"
        spec = importlib.util.spec_from_file_location(name, ROOT / "tests/agent_conformance/part_b/launcher.py")
        module = importlib.util.module_from_spec(spec)
        with patch.object(sys, "platform", "win32"), \
                patch.dict(sys.modules, {"fcntl": None, "pty": None, "termios": None}):
            spec.loader.exec_module(module)
            with self.assertRaisesRegex(HarnessGap, "Unix-only"):
                module.OwnedPty(None, "fixture", [], None)
