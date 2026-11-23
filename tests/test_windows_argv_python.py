"""Core-compatible diagnostic interpreter selection, without executing candidate programs."""
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
import testing_unit as unit
import vqro_testing as shared


class PythonSelection(unittest.TestCase):
    def test_windows_directory_order_and_launcher_preference_match_core(self):
        first, second = Path("first"), Path("second")
        for present, expected in [
            ({first / "python.exe", second / "py.exe"}, first / "python.exe"),
            ({first / "python.exe", first / "py.exe"}, first / "py.exe"),
            ({first / "python3.exe"}, first / "python3.exe"),
            (set(), None),
        ]:
            with self.subTest(present=present):
                self.assertEqual(shared._windows_python_program([first, second], present.__contains__), expected)

    def test_metadata_failure_does_not_abort_the_candidate_search(self):
        directory = Path("tools")
        exists = Mock(side_effect=[PermissionError("metadata unavailable"), True])
        self.assertEqual(shared._windows_python_program([directory], exists), directory / "python.exe")
        self.assertEqual(exists.call_count, 2)

    def test_windowsapps_entries_are_not_even_inspected(self):
        exists = Mock(side_effect=AssertionError("Store stub must not be inspected"))
        self.assertIsNone(shared._windows_python_program([Path("WindowsApps"), Path("wInDoWsApPs")], exists))
        exists.assert_not_called()

    def test_quoted_windows_path_entries_are_split_like_core(self):
        self.assertEqual(shared._windows_path_dirs('"C:/Semi;Colon";C:/WindowsApps;C:/Tools', separator=";"),
                         [Path("C:/Semi;Colon"), Path("C:/WindowsApps"), Path("C:/Tools")])

    def test_py_only_setup_is_healthy_without_a_python3_alias(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tools = root / "tools"
            tools.mkdir()
            (tools / "py.exe").write_bytes(b"fixture, never executed")
            (root / "sample.py").write_text("pass\n", encoding="utf-8")
            with patch.object(sys, "platform", "win32"), patch.dict(os.environ, {"VQRO_PYTHON": ""}):
                result = unit.argv_row("fixture", {"argv": ["python3", "sample.py"]}, root, path=str(tools))
            self.assertEqual(result["status"], "pass", result)
            self.assertIn("py.exe", result["evidence"])

    def test_explicit_override_with_spaces_wins_and_missing_override_never_falls_back(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            override = root / "Python With Spaces" / "python.exe"
            override.parent.mkdir()
            override.write_bytes(b"fixture, never executed")
            (root / "sample.py").write_text("pass\n", encoding="utf-8")
            with patch.object(sys, "platform", "win32"), \
                    patch.dict(os.environ, {"VQRO_PYTHON": str(override)}), \
                    patch.object(shared, "_windows_python_program", side_effect=AssertionError("override must win")):
                result = unit.argv_row("fixture", {"argv": ["python3", "sample.py"]}, root, path="unused")
                self.assertEqual(result["status"], "pass", result)
                self.assertIn(str(override), result["evidence"])
                override.unlink()
                self.assertEqual(unit.argv_row("fixture", {"argv": ["python3", "sample.py"]}, root,
                                               path="unused")["status"], "fail")

    def test_override_is_one_program_not_a_shell_command(self):
        with patch.dict(os.environ, {"VQRO_PYTHON": "py -3"}), \
                patch.object(shared.shutil, "which", return_value=None) as which:
            self.assertIsNone(shared.resolve_program("python3", ".", path="fixture-path"))
            which.assert_called_once_with("py -3", path="fixture-path")

    def test_unix_default_remains_literal_python3(self):
        with patch.object(sys, "platform", "linux"), patch.dict(os.environ, {"VQRO_PYTHON": ""}), \
                patch.object(shared.shutil, "which", return_value=None) as which:
            self.assertIsNone(shared.resolve_program("python3", ".", path="fixture-path"))
            which.assert_called_once_with("python3", path="fixture-path")
