"""Explicit archive durability helpers; only private temporary directories."""
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
from coordination_archive import export


class ArchiveTests(unittest.TestCase):
    def test_exact_export_and_reconcile_never_overwrite_a_different_set(self):
        with tempfile.TemporaryDirectory(prefix="coord-archive-") as directory:
            records = [{"delivery": "d:1", "version": 7, "record": {"state": "unknown", "content": "evidence"}}]
            first = export(directory, "abc123", records)
            before = Path(first["locator"]).read_bytes()
            self.assertEqual(export(directory, "abc123", records), first)
            self.assertEqual(Path(first["locator"]).read_bytes(), before)
            with self.assertRaises(ValueError):
                export(directory, "abc123", [dict(records[0], version=8)])
            self.assertEqual(Path(first["locator"]).read_bytes(), before)

    def test_partial_existing_export_is_not_replayed_or_overwritten(self):
        with tempfile.TemporaryDirectory(prefix="coord-archive-") as directory:
            path = Path(directory) / "abc.json"
            path.write_bytes(b"partial")
            with self.assertRaises(ValueError):
                export(directory, "abc", [])
            self.assertEqual(path.read_bytes(), b"partial")

    def test_destination_and_identifier_are_explicit(self):
        with self.assertRaises(ValueError):
            export("relative", "abc", [])
        with tempfile.TemporaryDirectory(prefix="coord-archive-") as directory:
            with self.assertRaises(ValueError):
                export(directory, "../../other", [])
            self.assertEqual(list(Path(directory).iterdir()), [])


if __name__ == "__main__":
    unittest.main()
