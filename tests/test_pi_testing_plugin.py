"""Model-free plugin hooks and explicit per-launch asset selection, never a native/model session."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "extensions"))
from pi_testing import testing_args, testing_environment
from vqro_ext import VqroError


class PiTestingPluginTests(unittest.TestCase):
    def test_only_managed_endpoint_and_state_are_forwarded(self):
        class Ext:
            info = {"socket": "opaque:selected-endpoint", "state_dir": "/private/state", "extension_token": "private"}
        self.assertEqual(testing_environment(Ext()), {"VQRO_TESTING_SOCKET": "opaque:selected-endpoint",
                                                      "VQRO_TESTING_STATE_DIR": "/private/state"})
        self.assertEqual(testing_environment(object()), {})

    def test_checkout_and_installed_asset_selection(self):
        args = testing_args(ROOT / "extensions")
        self.assertEqual(args[::2], ["-e", "--skill"])
        self.assertTrue(all(Path(path).is_file() for path in args[1::2]))
        with tempfile.TemporaryDirectory(prefix="pi-testing-assets-") as temporary:
            extensions = Path(temporary) / "extensions"
            destination = extensions / "tier1"
            destination.mkdir(parents=True)
            shutil.copyfile(ROOT / "integrations/pi/vqro-testing.ts", destination / "vqro-testing.ts")
            shutil.copytree(ROOT / "integrations/pi/skills", destination / "skills")
            installed = testing_args(extensions)
            self.assertTrue(all(Path(path).is_relative_to(extensions) for path in installed[1::2]))
            Path(installed[-1]).unlink()
            with self.assertRaisesRegex(VqroError, "missing"):
                testing_args(extensions)

    @unittest.skipUnless(shutil.which("bun"), "Bun unavailable: model-free TypeScript controls not executed")
    def test_model_free_hook_controls(self):
        with tempfile.TemporaryDirectory(prefix="pi-testing-home-") as home:
            result = subprocess.run([shutil.which("bun"), "test", "tests/pi_testing_plugin.test.ts"], cwd=ROOT,
                                    env={"HOME": home, "PATH": os.environ.get("PATH", os.defpath)},
                                    capture_output=True, text=True, timeout=60)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)


if __name__ == "__main__":
    unittest.main()
