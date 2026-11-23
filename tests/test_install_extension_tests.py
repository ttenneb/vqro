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
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("install_extension_tests", ROOT / "scripts/install_extension_tests.py")
installer = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(installer)


def symlink_or_skip(test_case, link, target, *, target_is_directory=False):
    try:
        link.symlink_to(target, target_is_directory=target_is_directory)
    except OSError as error:
        if sys.platform == "win32" and getattr(error, "winerror", None) == 1314:
            test_case.skipTest("Windows symlink fixture requires Developer Mode or symlink privilege")
        raise


class InstallExtensionTests(unittest.TestCase):
    def test_symlink_fixture_skips_only_missing_windows_privilege(self):
        link = mock.Mock()
        privilege = OSError("fixture privilege missing")
        privilege.winerror = 1314
        with mock.patch.object(sys, "platform", "win32"):
            link.symlink_to.side_effect = privilege
            with self.assertRaises(unittest.SkipTest):
                symlink_or_skip(self, link, "fixture")
            link.symlink_to.side_effect = PermissionError("unexpected destination failure")
            with self.assertRaisesRegex(PermissionError, "unexpected destination failure"):
                symlink_or_skip(self, link, "fixture")

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

    def test_preflight_writes_nothing_and_runs_before_first_install_copy(self):
        with tempfile.TemporaryDirectory(prefix="vqro-test-check-") as temporary:
            home = Path(temporary)
            destination = home / "missing/extensions"
            checked = subprocess.run([sys.executable, "-B", str(ROOT / "scripts/install_extension_tests.py"),
                                      "--check", str(destination)], cwd=home,
                                     env={"HOME": str(home), "PATH": os.environ.get("PATH", os.defpath)},
                                     capture_output=True, text=True, timeout=10)
            self.assertEqual(checked.returncode, 0, checked.stderr)
            self.assertEqual(list(home.iterdir()), [])
        script = (ROOT / "scripts/dev-install.sh").read_text(encoding="utf-8")
        self.assertLess(script.index("install_extension_tests.py --check"), script.index("cp extensions/"))

    def test_bad_mapping_preflight_preserves_existing_suite(self):
        with tempfile.TemporaryDirectory(prefix="vqro-test-invalid-") as temporary:
            destination = Path(temporary) / "extensions"
            installer.install_tests(ROOT, destination)
            original = (destination / "tests/agents/test_agents.py").read_bytes()
            source = Path(temporary) / "source"
            (source / "extensions").mkdir(parents=True)
            (source / "extensions/defaults.json").write_text(json.dumps({"extensions": [{"name": "agents"}]}))
            (source / "extensions/tests.json").write_text(json.dumps({"suites": {"agents": ["tests/test_missing.py"]}}))
            with self.assertRaisesRegex(ValueError, "not a test source"):
                installer.install_tests(source, destination, check=True)
            self.assertEqual((destination / "tests/agents/test_agents.py").read_bytes(), original)

    def test_symlinked_parent_and_leaf_refused_without_changes(self):
        for leaf in (False, True):
            with self.subTest(leaf=leaf), tempfile.TemporaryDirectory(prefix="vqro-test-symlink-") as temporary:
                home = Path(temporary)
                destination = home / "extensions"
                outside = home / "outside"
                outside.mkdir()
                canary = outside / "test_canary.py"
                canary.write_text("must survive unchanged\n")
                destination.mkdir()
                if leaf:
                    (destination / "tests").mkdir()
                    (destination / "tests/agents").mkdir()
                    (destination / "tests/agents/keep.py").write_text("previous suite\n")
                    link = destination / "tests/tier1"
                else:
                    link = destination / "tests"
                symlink_or_skip(self, link, outside, target_is_directory=True)
                for check in (True, False):
                    completed = subprocess.run([sys.executable, "-B", str(ROOT / "scripts/install_extension_tests.py"),
                                                *(["--check"] if check else []), str(destination)],
                                               cwd=home, env={"HOME": str(home), "PATH": os.environ.get("PATH", os.defpath)},
                                               capture_output=True, text=True, timeout=10)
                    self.assertNotEqual(completed.returncode, 0)
                    self.assertIn("unsafe test destination", completed.stderr)
                    self.assertTrue(link.is_symlink())
                    self.assertEqual(canary.read_text(), "must survive unchanged\n")
                    self.assertEqual(list(outside.iterdir()), [canary])
                    self.assertFalse((destination / "tests.json").exists())
                    if leaf:
                        self.assertEqual((destination / "tests/agents/keep.py").read_text(), "previous suite\n")

    def test_invalid_mapping_output_preserves_every_target(self):
        for kind in ("directory", "symlink", "dangling-symlink"):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory(prefix="vqro-test-output-") as temporary:
                home = Path(temporary)
                destination = home / "extensions"
                mapping = installer.test_files(ROOT)
                for name in mapping:
                    suite = destination / "tests" / name
                    suite.mkdir(parents=True)
                    (suite / "sentinel").write_bytes(b"unchanged suite: " + name.encode())
                metadata = destination / "tests.json"
                outside = home / "outside.json"
                if kind == "directory":
                    metadata.mkdir()
                    (metadata / "sentinel").write_bytes(b"unchanged metadata directory")
                else:
                    if kind == "symlink":
                        outside.write_bytes(b"unchanged outside file")
                    symlink_or_skip(self, metadata, outside)
                before = {str(path.relative_to(destination)): path.read_bytes()
                          for path in destination.rglob("*") if path.is_file() and not path.is_symlink()}
                before_paths = {str(path.relative_to(destination)) for path in destination.rglob("*")}
                for check in (True, False):
                    completed = subprocess.run([sys.executable, "-B", str(ROOT / "scripts/install_extension_tests.py"),
                                                *(["--check"] if check else []), str(destination)],
                                               cwd=home, env={"HOME": str(home), "PATH": os.environ.get("PATH", os.defpath)},
                                               capture_output=True, text=True, timeout=10)
                    self.assertNotEqual(completed.returncode, 0)
                    self.assertIn("mapping", completed.stderr)
                    self.assertEqual({str(path.relative_to(destination)) for path in destination.rglob("*")}, before_paths)
                    self.assertEqual({str(path.relative_to(destination)): path.read_bytes()
                                      for path in destination.rglob("*") if path.is_file() and not path.is_symlink()}, before)
                    if kind != "directory":
                        self.assertTrue(metadata.is_symlink())
                        self.assertEqual(metadata.readlink(), outside)
                        self.assertEqual(outside.exists(), kind == "symlink")
                        if outside.exists():
                            self.assertEqual(outside.read_bytes(), b"unchanged outside file")

    def test_copy_failure_keeps_previous_complete_suite(self):
        with tempfile.TemporaryDirectory(prefix="vqro-test-copy-failure-") as temporary:
            destination = Path(temporary) / "extensions"
            installer.install_tests(ROOT, destination)
            suite = destination / "tests/agents"
            before = {path.name: path.read_bytes() for path in suite.iterdir()}
            copyfile = installer.shutil.copyfile
            count = 0

            def fail_second_copy(source, target):
                nonlocal count
                count += 1
                if count == 2:
                    raise OSError("injected copy failure")
                return copyfile(source, target)

            with mock.patch.object(installer.shutil, "copyfile", side_effect=fail_second_copy):
                with self.assertRaisesRegex(OSError, "injected copy failure"):
                    installer.install_tests(ROOT, destination)
            self.assertEqual({path.name: path.read_bytes() for path in suite.iterdir()}, before)
            self.assertFalse(any(path.name.startswith(".") for path in suite.parent.iterdir()))

    def test_publication_failure_restores_previous_suite(self):
        with tempfile.TemporaryDirectory(prefix="vqro-test-rename-failure-") as temporary:
            destination = Path(temporary) / "extensions"
            installer.install_tests(ROOT, destination)
            suite = destination / "tests/agents"
            before = {path.name: path.read_bytes() for path in suite.iterdir()}
            rename = Path.rename

            def fail_publication(source, target):
                if source.name == "suite":
                    raise OSError("injected rename failure")
                return rename(source, target)

            with mock.patch.object(Path, "rename", fail_publication):
                with self.assertRaisesRegex(OSError, "injected rename failure"):
                    installer.install_tests(ROOT, destination)
            self.assertEqual({path.name: path.read_bytes() for path in suite.iterdir()}, before)

    def test_failed_rollback_retains_backup_even_if_destination_exists(self):
        with tempfile.TemporaryDirectory(prefix="vqro-test-rollback-failure-") as temporary:
            destination = Path(temporary) / "extensions"
            installer.install_tests(ROOT, destination)
            suite = destination / "tests/agents"
            before = {path.name: path.read_bytes() for path in suite.iterdir()}
            rename = Path.rename

            def fail_publication_and_rollback(source, target):
                if source.name == "suite":
                    target.mkdir()
                    (target / "interloper").write_text("simulated competing destination")
                    raise OSError("injected publication failure")
                if source.name == "previous":
                    raise OSError("injected rollback failure")
                return rename(source, target)

            with mock.patch.object(Path, "rename", fail_publication_and_rollback):
                with self.assertRaisesRegex(OSError, "injected rollback failure"):
                    installer.install_tests(ROOT, destination)
            backups = list(suite.parent.glob(".agents-*/previous"))
            self.assertEqual(len(backups), 1)
            self.assertEqual({path.name: path.read_bytes() for path in backups[0].iterdir()}, before)

    def test_private_installed_offline_cli_and_broken_test_control(self):
        with tempfile.TemporaryDirectory(prefix="vqro-test-install-") as temporary:
            home = Path(temporary)
            destination = home / ".local/share/vqro/extensions"
            shutil.copytree(ROOT / "extensions", destination, ignore=shutil.ignore_patterns("__pycache__", "node_modules"))  # dev packages are never installed
            (destination / "tier1").mkdir(exist_ok=True)
            shutil.copyfile(ROOT / "integrations/pi/vqro-pull.ts", destination / "tier1/vqro-pull.ts")
            shutil.copyfile(ROOT / "integrations/pi/vqro-testing.ts", destination / "tier1/vqro-testing.ts")
            shutil.copytree(ROOT / "integrations/pi/skills", destination / "tier1/skills")
            env = {"HOME": str(home), "PATH": os.environ.get("PATH", os.defpath), "PYTHONIOENCODING": "utf-8"}
            copied = subprocess.run([sys.executable, "-B", str(ROOT / "scripts/install_extension_tests.py"), str(destination)],
                                    env=env, cwd=home, capture_output=True, text=True, encoding="utf-8", timeout=10)
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
                completed = subprocess.run(command, env=env, cwd=home, capture_output=True, text=True, encoding="utf-8", timeout=120)
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
