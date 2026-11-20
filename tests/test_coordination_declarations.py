"""Capacity comes from public core facts; historical latency is not a runtime promise."""
import copy
import unittest
import test_coordination_service as fixtures


class DeclarationTests(unittest.TestCase):
    def setUp(self):
        self.fixture = fixtures.ServiceTests(); self.fixture.setUp()
        self.ext, self.service = self.fixture.ext, self.fixture.service

    def test_identity_capacity_projects_core_snapshot_without_status_rpc(self):
        self.ext.snapshot_header = {'epoch': self.ext.epoch, 'seq': 42,
            'identity_reservations': {'max_entries': 17, 'max_entry_bytes': 333, 'count': 9}}
        calls = copy.deepcopy(self.ext.vqro.calls)
        status = self.fixture.callback('status', {})
        cache = status['identity_cache']
        self.assertEqual(cache['capacity'], 17, 'never substitute a bundled-core constant')
        self.assertEqual(cache['max_entry_bytes'], 333)
        self.assertFalse(cache['incomplete'])
        self.assertEqual(cache['source'], 'workspace.snapshot.identity_reservations')
        self.assertNotIn('count', cache, 'snapshot occupancy is not a live counter')
        self.assertEqual(self.ext.vqro.calls, calls)

    def test_absent_malformed_or_other_epoch_capacity_is_unknown(self):
        for stats in (None, {}, {'max_entries': True, 'max_entry_bytes': 2048},
                      {'max_entries': '4096', 'max_entry_bytes': 2048},
                      {'max_entries': -1, 'max_entry_bytes': 2048},
                      {'max_entries': 4096, 'max_entry_bytes': 0}):
            self.ext.snapshot_header = {'epoch': self.ext.epoch, 'identity_reservations': stats}
            cache = self.fixture.callback('status', {})['identity_cache']
            self.assertTrue(cache['incomplete']); self.assertIsNone(cache['capacity'])
        self.ext.snapshot_header = {'epoch': 'other', 'identity_reservations': {'max_entries': 4096, 'max_entry_bytes': 2048}}
        self.assertTrue(self.fixture.callback('status', {})['identity_cache']['incomplete'])

    def test_latency_is_explicit_historical_conditional_measurement(self):
        status = self.fixture.callback('status', {})
        self.assertEqual(status['control_latency_ms'], 3000)
        envelope = status['control_latency_conditions']
        self.assertTrue(envelope['historical']); self.assertFalse(envelope['runtime_guarantee'])
        self.assertFalse(envelope['current_release_verified'])
        self.assertTrue(envelope['verified_on_measured_pin_and_load'])
        basis = envelope['measurement']
        self.assertEqual(basis['core'], 'ac140ac7'); self.assertEqual(basis['coord'], 'ac140ac7')
        self.assertEqual((basis['agents'], basis['streams'], basis['cpu_count'], basis['nice']), (200, 600, 8, 10))
        self.assertEqual(basis['samples'], {'interrupt': 80, 'respond': 80})
        self.assertEqual(basis['stalled_streams'], 200)
        self.assertEqual(basis['max_ms'], {'interrupt': 2378.1, 'respond': 1914.2})
        self.assertEqual(envelope['earlier_s1_observation']['samples'], {'interrupt': 3, 'respond': 3})
        envelope['measurement']['agents'] = -1
        self.assertEqual(self.fixture.callback('status', {})['control_latency_conditions']['measurement']['agents'], 200)
