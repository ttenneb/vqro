# SPDX-License-Identifier: Apache-2.0
"""Offline licence policy, original notices and deterministic inventory regressions."""
import contextlib
import hashlib
import io
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]


def module():
    spec = importlib.util.spec_from_file_location("vqro_license_check", ROOT / "scripts/license_check.py")
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


class Licenses(unittest.TestCase):
    def test_spdx_precedence_choices_and_explicit_exceptions(self):
        m = module()
        for expression in ["Apache-2.0", "MIT/Apache-2.0", "GPL-3.0-only OR MIT", "BSD-3-Clause AND ISC",
                           "(Apache-2.0 OR MIT) AND Unicode-3.0", "Apache-2.0 WITH LLVM-exception", "WTFPL"]:
            self.assertTrue(m.allowed_expression(expression), expression)
        for expression in ["GPL-3.0-only", "MIT AND GPL-3.0-only", "Apache-2.0 WITH Unknown-exception",
                           "(MIT OR Apache-2.0) AND Unknown", "Unicode-unknown"]:
            self.assertFalse(m.allowed_expression(expression), expression)
        for expression in ["", "MIT OR", "(MIT", "MIT junk", "MIT AND (Apache-2.0 OR)", "MIT | Apache-2.0"]:
            with self.assertRaises(ValueError, msg=expression):
                m.allowed_expression(expression)

    def fixture(self, root, license="MIT"):
        crate = root / "crate"
        crate.mkdir()
        (crate / "Cargo.toml").write_text("[package]\nname='fixture'\n")
        return {"workspace_members": ["vqro"], "packages": [
            {"id": "vqro", "name": "vqro", "version": "0.1.0", "license": "Apache-2.0"},
            {"id": "fixture", "name": "fixture", "version": "1.0.0", "license": license,
             "manifest_path": str(crate / "Cargo.toml"), "license_file": None,
             "source": "registry+https://github.com/rust-lang/crates.io-index", "authors": ["Declared Author"],
             "repository": "https://example.invalid/declared"}]}

    def test_original_texts_notices_and_stable_order(self):
        m = module()
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            meta = self.fixture(root)
            original = "Copyright Original Author\nOriginal licence text.\n"
            (root / "crate/LICENSE").write_text(original)
            (root / "crate/NOTICE").write_text("Required attribution\n")
            first = m.render(meta)
            self.assertIn(original, first)
            self.assertIn(hashlib.sha256(original.encode()).hexdigest(), first)
            self.assertIn("Required attribution", first)
            meta["packages"].reverse()
            self.assertEqual(first, m.render(meta))

    def test_no_archive_text_uses_labelled_standard_not_invented_holder(self):
        m = module()
        with tempfile.TemporaryDirectory() as name:
            result = m.render(self.fixture(Path(name)))
            self.assertIn("licence text not shipped in the crate; standard text for the declared licence", result)
            self.assertIn("Declared Author", result)
            self.assertIn("https://example.invalid/declared", result)
            self.assertNotIn("Copyright (c) Declared Author", result)
            self.assertIn("Permission is hereby granted", result)

    def test_rejects_unapproved_expression_and_external_license_file(self):
        m = module()
        with tempfile.TemporaryDirectory() as name:
            root = Path(name)
            meta = self.fixture(root, "MIT AND GPL-3.0-only")
            with self.assertRaisesRegex(ValueError, "allowlist"):
                m.render(meta)
            meta["packages"][1]["license"] = "MIT"
            outside = root / "private"
            outside.write_text("not a crate notice")
            meta["packages"][1]["license_file"] = str(outside)
            with self.assertRaisesRegex(ValueError, "outside"):
                m.render(meta)

    def test_every_metadata_package_is_checked_even_if_unbuilt(self):
        m = module()
        with tempfile.TemporaryDirectory() as name:
            meta = self.fixture(Path(name))
            meta["packages"].append({**meta["packages"][1], "id": "unbuilt", "name": "unbuilt", "license": "GPL-3.0-only"})
            with self.assertRaisesRegex(ValueError, "unbuilt"):
                m.render(meta)

    def test_check_detects_missing_and_stale_inventory_without_writing(self):
        m = module()
        with tempfile.TemporaryDirectory() as name:
            root = Path(name); meta = self.fixture(root)
            with mock.patch.object(m, "ROOT", root), mock.patch.object(m, "check_project"), \
                 mock.patch.object(m, "cargo_output", return_value=json.dumps(meta)), \
                 contextlib.redirect_stdout(io.StringIO()), contextlib.redirect_stderr(io.StringIO()):
                with mock.patch("sys.argv", ["license_check", "--check"]): self.assertEqual(m.main(), 1)
                self.assertFalse((root / m.OUTPUT).exists())
                with mock.patch("sys.argv", ["license_check", "--write"]): self.assertEqual(m.main(), 0)
                with mock.patch("sys.argv", ["license_check", "--check"]): self.assertEqual(m.main(), 0)
                (root / m.OUTPUT).write_text("stale")
                with mock.patch("sys.argv", ["license_check", "--check"]): self.assertEqual(m.main(), 1)
                self.assertEqual((root / m.OUTPUT).read_text(), "stale")

    def test_repository_metadata_and_exact_root_license(self):
        m = module()
        m.check_project(ROOT)
        self.assertEqual(hashlib.sha256((ROOT / "LICENSE").read_bytes()).hexdigest(),
                         "cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30")
        for path in (ROOT / "extensions/manifests").glob("*.json"):
            manifest = json.loads(path.read_text())
            self.assertEqual(manifest["license"], "Apache-2.0", str(path))
            self.assertEqual(manifest["author"], "Bennett Garcia", str(path))


if __name__ == "__main__":
    unittest.main()
