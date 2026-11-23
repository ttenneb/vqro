"""Private HOME controls for the read-only installer preflight."""
import importlib.util
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / "scripts/install_preflight.py"
spec = importlib.util.spec_from_file_location("install_preflight", SCRIPT)
preflight = importlib.util.module_from_spec(spec)
spec.loader.exec_module(preflight)


class PreflightTests(unittest.TestCase):
    def test_private_legacy_layout_and_real_prim_process(self):
        with tempfile.TemporaryDirectory() as temp:
            home = Path(temp)
            state = home / ".local/state/vqro"
            (state / "client-shell").mkdir(parents=True)
            (state / "plugins").mkdir()
            legacy = home / ".config/vqro"
            legacy.mkdir(parents=True)
            sock = socket.socket(socket.AF_UNIX)
            sock.bind(str(legacy / "p"))
            prim = home / "prim"
            prim.symlink_to("/bin/bash")
            (home / "serve").write_text("read -t 60 unused\n")
            # Bash waits in a builtin, retaining the exact private `prim serve` argv.
            process = subprocess.Popen([str(prim), "serve"], cwd=home, stdin=subprocess.PIPE)
            try:
                with patch.dict(os.environ, {"HOME": temp}, clear=True):
                    notices = preflight.warnings(home / "new")
                self.assertTrue(any("client-shell" in n for n in notices))
                self.assertTrue(any("plugins" in n for n in notices))
                self.assertTrue(any("old-layout socket" in n for n in notices))
                self.assertTrue(any(f"legacy core process {process.pid}:" in n for n in notices))
                self.assertIsNone(process.poll())
                self.assertTrue((state / "plugins").is_dir())
            finally:
                process.terminate()
                process.wait()
                process.stdin.close()
                sock.close()

    def test_portable_ps_and_older_current_core(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "serve.pid").write_text("123")
            binary = root / "vqro"
            binary.touch()
            ps = subprocess.CompletedProcess([], 0, "123 Mon Nov 23 10:00:00 2020 /local/bin/vqro serve\n124 Mon Nov 23 10:00:00 2020 /local/bin/prim serve\n125 Mon Nov 23 10:00:00 2020 target/release/vqro server\n")
            with patch.dict(os.environ, {"HOME": temp, "VQRO_STATE_DIR": temp}, clear=True), patch.object(subprocess, "run", return_value=ps):
                notices = preflight.warnings(binary)
            self.assertTrue(any("older build" in n for n in notices))
            self.assertEqual(sum("legacy core process" in n for n in notices), 2)

    def test_clean_home_is_quiet(self):
        with tempfile.TemporaryDirectory() as temp:
            with patch.dict(os.environ, {"HOME": temp}, clear=True), patch.object(subprocess, "run", return_value=subprocess.CompletedProcess([], 0, "")):
                self.assertEqual(preflight.warnings(Path(temp) / "missing"), [])
