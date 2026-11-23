"""Disabled declarations bind known absence, not an invented unknown input."""
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
import vqro_testing as testing


class DisabledFingerprintTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="vqro-absent-")
        self.addCleanup(temporary.cleanup)
        self.base = Path(temporary.name)
        self.root = self.base / "extensions"
        self.root.mkdir()
        self.hello = {"server": "fixture", "version": "1", "epoch": "private"}

    def fingerprint(self, declaration):
        return testing.fingerprint(self.root, {"extension/optional": declaration}, self.hello,
                                   state_dir=self.base / "state", endpoint=self.base / "core.sock")

    def test_disabled_missing_absolute_and_bare_programs_have_deterministic_absence(self):
        with patch.dict(os.environ, {"PATH": ""}):
            for program in (str(self.base / "missing-program"), "missing-fingerprint-fixture"):
                with self.subTest(program=program):
                    declaration = {"argv": [program], "enabled": False}
                    first, second = self.fingerprint(declaration), self.fingerprint(declaration)
                    self.assertIsNotNone(first["setup_id"])
                    self.assertEqual(first["setup_id"], second["setup_id"])
                    self.assertEqual(first["unknown"], [])
                    self.assertIsNotNone(first["extensions"]["optional"])
                    self.assertEqual(first["executables"]["optional"]["resolution"], "absent")
                    self.assertIsNone(self.fingerprint({**declaration, "enabled": True})["setup_id"])

    @unittest.skipUnless(os.name == "posix", "POSIX permission fixture; Windows ACL behavior needs native validation")
    def test_inaccessible_path_directory_is_unknown_not_proved_bare_name_absence(self):
        directory = self.base / "hidden-path"
        directory.mkdir()
        image = directory / "hidden-program"
        image.write_text("#!/bin/sh\nexit 0\n")
        image.chmod(0o700)
        directory.chmod(0)
        try:
            try:
                image.stat()
            except PermissionError:
                pass
            else:
                self.skipTest("current identity bypasses the private permission fixture")
            with patch.dict(os.environ, {"PATH": str(directory)}):
                self.assertIsNone(testing.shutil.which("hidden-program", mode=os.F_OK), "old fallback hides the inaccessible candidate")
                result = self.fingerprint({"argv": ["hidden-program"], "enabled": False})
            self.assertIsNone(result["setup_id"])
            self.assertEqual(result["executables"]["optional"]["resolution"], "unknown")
            self.assertTrue(result["unknown"])
        finally:
            directory.chmod(0o700)

    @unittest.skipUnless(os.name == "posix", "POSIX permission fixture; Windows ACL behavior needs native validation")
    def test_readable_nonexec_first_match_wins_over_inaccessible_later_path(self):
        first, hidden = self.base / "first", self.base / "hidden"
        first.mkdir()
        hidden.mkdir()
        program = first / "optional-tool"
        program.write_text("known first input")
        program.chmod(0o600)
        (hidden / "optional-tool").write_text("must not override the first input")
        hidden.chmod(0)
        try:
            try:
                (hidden / "optional-tool").stat()
            except PermissionError:
                pass
            else:
                self.skipTest("current identity bypasses the private permission fixture")
            declaration = {"argv": ["optional-tool"], "enabled": False}
            with patch.dict(os.environ, {"PATH": str(first)}):
                expected = self.fingerprint(declaration)
            with patch.dict(os.environ, {"PATH": os.pathsep.join((str(first), str(hidden)))}):
                actual = self.fingerprint(declaration)
            self.assertIsNotNone(actual["setup_id"])
            self.assertEqual(actual["setup_id"], expected["setup_id"])
            self.assertEqual(actual["executables"]["optional"]["path"], str(program))
            self.assertTrue(actual["executables"]["optional"]["sha256"])
            self.assertEqual(actual["unknown"], [])
            with patch.dict(os.environ, {"PATH": os.pathsep.join((str(hidden), str(first)))}):
                self.assertIsNone(self.fingerprint(declaration)["setup_id"])
        finally:
            hidden.chmod(0o700)

    def test_disabled_missing_python_script_is_known_until_enabled(self):
        declaration = {"argv": [sys.executable, "-B", str(self.root / "missing.py")], "enabled": False}
        absent = self.fingerprint(declaration)
        self.assertIsNotNone(absent["setup_id"])
        self.assertEqual(absent["unknown"], [])
        self.assertIsNone(self.fingerprint({**declaration, "enabled": True})["setup_id"])
        (self.root / "missing.py").write_text("print('now installed')\n")
        self.assertNotEqual(absent["setup_id"], self.fingerprint(declaration)["setup_id"])

    def test_disabled_broken_script_link_is_known_absence_not_unreadability(self):
        script = self.root / "optional.py"
        script.symlink_to("not-installed.py")
        declaration = {"argv": [sys.executable, str(script)], "enabled": False}
        self.assertIsNotNone(self.fingerprint(declaration)["setup_id"])
        self.assertIsNone(self.fingerprint({**declaration, "enabled": True})["setup_id"])

    def test_program_appearance_and_declaration_changes_change_identity(self):
        program = self.base / "provider"
        declaration = {"argv": [str(program)], "enabled": False}
        absent = self.fingerprint(declaration)
        changed = self.fingerprint({**declaration, "argv": [str(program), "--changed"]})
        self.assertNotEqual(absent["setup_id"], changed["setup_id"])
        program.write_text("#!/bin/sh\nexit 0\n")
        program.chmod(0o700)
        present = self.fingerprint(declaration)
        self.assertNotEqual(absent["setup_id"], present["setup_id"])
        self.assertEqual(present["executables"]["optional"]["resolution"], "resolved")

    def test_unresolvable_existing_program_bytes_still_bind_identity(self):
        program = self.base / "not-executable"
        program.write_text("first")
        declaration = {"argv": [str(program)], "enabled": False}
        first = self.fingerprint(declaration)
        self.assertIsNotNone(first["setup_id"])
        self.assertEqual(first["executables"]["optional"]["resolution"], "absent")
        program.write_text("changed")
        self.assertNotEqual(first["setup_id"], self.fingerprint(declaration)["setup_id"])

    def test_existing_unreadable_disabled_program_and_script_remain_unknown(self):
        unreadable = self.base / "unreadable"
        unreadable.write_text("must not be silently replaced by an absent marker")
        original_open = Path.open

        def open_input(path, *args, **kwargs):
            if path == unreadable:
                raise PermissionError("fixture denies input read")
            return original_open(path, *args, **kwargs)

        for argv in ([str(unreadable)], [sys.executable, str(unreadable)]):
            with self.subTest(argv=argv), patch.object(Path, "open", open_input):
                result = self.fingerprint({"argv": argv, "enabled": False})
                self.assertIsNone(result["setup_id"])
                self.assertTrue(any("denies input read" in error for error in result["unknown"]))

    def test_disabled_explicit_missing_manifest_is_known_absence(self):
        result = self.fingerprint({"argv": [str(self.base / "missing")], "enabled": False,
                                   "manifest": "manifests/optional.json"})
        self.assertIsNotNone(result["setup_id"])
        self.assertEqual(result["unknown"], [])


if __name__ == "__main__":
    unittest.main()
