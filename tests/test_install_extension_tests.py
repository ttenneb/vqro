"""Rehearse only the installer copy step, never builds or the user's install."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("install_extension_tests", ROOT / "scripts/install_extension_tests.py")
installer = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(installer)


class InstallExtensionTests(unittest.TestCase):
    def test_mapping_names_and_sources(self):
        mapping = installer.test_files(ROOT)
        self.assertTrue(mapping)
        declarations = {entry["name"] for entry in json.loads((ROOT / "extensions/defaults.json").read_text())["extensions"]}
        for name, files in mapping.items():
            self.assertIn(name, declarations)
            for file in files:
                self.assertTrue((ROOT / file).is_file(), file)

    def test_reinstall_drops_removed_tests_but_preserves_unmapped_suites(self):
        with tempfile.TemporaryDirectory(prefix="vqro-test-copy-") as temporary:
            destination = Path(temporary) / "extensions"
            installer.install_tests(ROOT, destination)
            stale = destination / "tests/agents/test_removed.py"
            stale.write_text("raise AssertionError('stale test')\n")
            custom = destination / "tests/custom/test_custom.py"
            custom.parent.mkdir()
            custom.write_text("# third-party suite\n")
            installer.install_tests(ROOT, destination)
            self.assertFalse(stale.exists())
            self.assertTrue(custom.exists())
            self.assertEqual(json.loads((destination / "tests.json").read_text()),
                             json.loads((ROOT / "extensions/tests.json").read_text()))
            for name, files in installer.test_files(ROOT).items():
                self.assertEqual({file.name for file in (destination / "tests" / name).iterdir()},
                                 {Path(file).name for file in files})

    def test_private_installed_offline_cli_and_broken_test_control(self):
        with tempfile.TemporaryDirectory(prefix="vqro-test-install-") as temporary:
            home = Path(temporary)
            destination = home / ".local/share/vqro/extensions"
            shutil.copytree(ROOT / "extensions", destination, ignore=shutil.ignore_patterns("__pycache__"))
            (destination / "tier1").mkdir(exist_ok=True)
            shutil.copyfile(ROOT / "integrations/pi/vqro-pull.ts", destination / "tier1/vqro-pull.ts")
            env = {"HOME": str(home), "PATH": os.environ.get("PATH", os.defpath)}
            copied = subprocess.run([sys.executable, "-B", str(ROOT / "scripts/install_extension_tests.py"), str(destination)],
                                    env=env, cwd=home, capture_output=True, text=True, timeout=10)
            self.assertEqual(copied.returncode, 0, copied.stderr)
            mapping = installer.test_files(ROOT)
            # Only declare mapped extensions, all enabled; no binaries or live core are needed offline.
            declarations = json.loads((destination / "defaults.json").read_text())["extensions"]
            state = home / "state"
            state.mkdir()
            snapshot = {"entries": {"extension/" + entry["name"]: {"version": 1, "value": dict(entry, enabled=True)}
                                    for entry in declarations if entry["name"] in mapping}}
            (state / "workspace.json").write_text(json.dumps(snapshot))
            command = [sys.executable, "-B", str(destination / "testing_unit.py"), "--offline", "--json",
                       "--extensions-dir", str(destination), "--state-dir", str(state)]

            def run_cli():
                completed = subprocess.run(command, env=env, cwd=home, capture_output=True, text=True, timeout=120)
                result = json.loads(completed.stdout)
                return completed, result, {row["subject"]: row for row in result["rows"] if row["id"].startswith("tests/")}

            completed, result, rows = run_cli()
            self.assertEqual(set(rows), set(mapping))
            for name, row in rows.items():
                self.assertEqual(row["status"], "pass", f"{name}: {row['evidence']}")
            self.assertNotIn(completed.returncode, (1, 2), result)
            broken = destination / "tests/agents/test_broken.py"
            broken.write_text("import unittest\nclass Broken(unittest.TestCase):\n def test_control(self): self.fail('deliberately broken copied test')\n")
            completed, result, rows = run_cli()
            self.assertEqual(completed.returncode, 1, result)
            self.assertEqual(rows["agents"]["status"], "fail")
            self.assertIn("deliberately broken copied test", rows["agents"]["evidence"])


if __name__ == "__main__":
    unittest.main()
