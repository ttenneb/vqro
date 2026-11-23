"""Platform branches that can be checked without a Windows runtime."""
import ast
import ntpath
import os
from pathlib import Path
import sys
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
import tier1


def path_function(filename, name):
    """Load a pure label helper without starting the extension or its scan thread."""
    source = Path(__file__).resolve().parents[1] / "extensions" / filename
    tree = ast.parse(source.read_text(encoding="utf-8"))
    functions = [node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == name]
    namespace = {"os": SimpleNamespace(path=ntpath, sep="\\"), "norm": ntpath.abspath}
    exec(compile(ast.Module(body=functions, type_ignores=[]), str(source), "exec"), namespace)
    return namespace[name]


class NativePathLabelTests(unittest.TestCase):
    def test_workspace_title_trims_windows_separator(self):
        title = path_function("workspaces.py", "folder_title")
        self.assertEqual(title("C:\\work\\project\\"), "project")
        self.assertEqual(title("C:/work/project/"), "project")

    def test_sidebar_title_trims_windows_separator(self):
        title = path_function("sidebar.py", "folder_of")
        self.assertEqual(title({"cwd": "C:\\work\\project\\"}), "project")
        self.assertEqual(title({"cwd": "C:/work/project/"}), "project")


class Tier1WindowsTests(unittest.TestCase):
    def test_windows_state_uses_localappdata_without_posix_chmod(self):
        with tempfile.TemporaryDirectory() as root:
            with patch.dict(os.environ, {"LOCALAPPDATA": root}, clear=True), \
                    patch.object(tier1.sys, "platform", "win32"), \
                    patch.object(tier1.os, "chmod") as chmod:
                self.assertEqual(tier1.state_dir(), Path(root) / "vqro" / "state" / "tier1")
                chmod.assert_not_called()

    def test_explicit_state_wins_on_windows(self):
        with tempfile.TemporaryDirectory() as root:
            with patch.dict(os.environ, {"VQRO_STATE_DIR": root}, clear=True), \
                    patch.object(tier1.sys, "platform", "win32"):
                self.assertEqual(tier1.state_dir(), Path(root) / "tier1")

    def test_pi_declaration_is_utf8(self):
        with tempfile.TemporaryDirectory() as root:
            plugin = Path(root) / "pull.ts"
            plugin.write_bytes('export const PULL_DECLARATION = {"pull":{"note":"café"}};\n'.encode("utf-8"))
            with patch.object(tier1, "PI_PLUGIN", plugin):
                self.assertEqual(tier1.pi_declaration(), {"note": "café"})

    def test_hook_output_is_utf8(self):
        with patch.object(tier1.subprocess, "run") as run:
            run.return_value.stdout = "{}"
            self.assertEqual(tier1.Tier1.run(["hook", "declaration"]), "{}")
            self.assertEqual(run.call_args.kwargs["encoding"], "utf-8")
