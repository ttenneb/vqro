"""Explicit archive durability helpers; only private temporary directories."""
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
import coordination_archive
from coordination_archive import export
import test_coordination_engine as fixtures


class ArchiveTests(unittest.TestCase):
    def test_windows_export_refuses_before_any_filesystem_effect(self):
        with patch.object(coordination_archive.os, "name", "nt"), \
                patch.object(coordination_archive, "Path") as path:
            with self.assertRaisesRegex(OSError, "not supported on native Windows yet"):
                export("C:\\archive", "abc", [])
            path.assert_not_called()

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

    def test_native_and_pull_scans_leave_frozen_pending_versions_unchanged(self):
        for pull in (False, True):
            with self.subTest(pull=pull), tempfile.TemporaryDirectory(prefix='coord-archive-scan-') as directory:
                f = fixtures.EngineTests(); f.setUp()
                if pull:
                    f.resources['r1']['data']['agent']['pull'] = {
                        'points': {'idle': {'precision': 'exact'}}, 'insert': 'context', 'evidence': 'entry'}
                    f.engine.reconcile({'op': f.op(), 'agent': 'r1', 'consumer': 'c1', 'records': []}, fixtures.OWNER)
                f.resume()
                delivery = f.send(**({'render': ['context']} if pull else {}))['delivery']
                plan = f.engine.archive({'op': f.op(), 'agents': ['r1'], 'destination': directory}, fixtures.OWNER)['archive']
                frozen = f.engine.get(delivery)
                for phase in ('frozen', 'exported'):
                    for _ in range(3):
                        self.assertIsNone(f.engine.claim_one())
                        if pull:
                            result = f.engine.take({'op': f.op(), 'agent': 'r1', 'consumer': 'c1',
                                                    'at': 'idle', 'accept': ['text']}, fixtures.OWNER)
                            self.assertEqual(result['records'], [])
                        self.assertEqual(f.engine.get(delivery), frozen, phase)
                    if phase == 'frozen':
                        exported = f.engine.archive({'op': f.op(), 'action': 'export', 'archive': plan}, fixtures.OWNER)
                        self.assertEqual(exported['state'], 'accepted')
                freed = f.engine.archive({'op': f.op(), 'action': 'free', 'archive': plan}, fixtures.OWNER)
                self.assertEqual(freed['state'], 'completed')
                self.assertEqual(freed['freed'], ['message/' + delivery])
                self.assertEqual(f.ledger, [])

    def test_destination_and_identifier_are_explicit(self):
        with self.assertRaises(ValueError):
            export("relative", "abc", [])
        with tempfile.TemporaryDirectory(prefix="coord-archive-") as directory:
            with self.assertRaises(ValueError):
                export(directory, "../../other", [])
            self.assertEqual(list(Path(directory).iterdir()), [])


if __name__ == "__main__":
    unittest.main()
