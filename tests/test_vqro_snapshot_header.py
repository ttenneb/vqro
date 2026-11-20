"""Generic snapshot metadata is cached without another core read or large collections."""
from pathlib import Path
import sys
import unittest
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'extensions'))
from vqro_ext import Extension


class SnapshotHeaderTests(unittest.TestCase):
    def test_sdk_keeps_generic_header_and_replaces_it_on_reload(self):
        ext = Extension.__new__(Extension)
        snap = {'entries': {}, 'resources': [], 'live': {}, 'providers': {}, 'epoch': 'e', 'seq': 1,
                'identity_reservations': {'max_entries': 4096, 'max_entry_bytes': 2048}, 'future': {'value': 7}}
        ext._load(snap)
        self.assertEqual(ext.snapshot_header['future'], {'value': 7})
        self.assertFalse(set(ext.snapshot_header) & {'entries', 'resources', 'live', 'providers'})
        snap['identity_reservations']['max_entries'] = 0
        self.assertEqual(ext.snapshot_header['identity_reservations']['max_entries'], 4096)
        ext._load({'entries': {}, 'resources': [], 'epoch': 'next', 'seq': 0})
        self.assertNotIn('identity_reservations', ext.snapshot_header)
