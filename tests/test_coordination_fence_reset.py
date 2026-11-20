"""Owner capacity acknowledgement retains evidence and never authorizes a resend."""
import copy
import unittest
import test_coordination_engine as fixtures
import test_coordination_service as services
import test_coordination_requests as routes
from coordination_engine import Working, digest
from coordination_policy import PolicyError
from coordination_store import StoreError
from agent_request_routes import request_identity_key


def seed(engine, capacity, entries, overflow=False):
    buckets = {}
    for slot, identity, revision, order in entries:
        buckets.setdefault('notice-fences/' + str(slot // 64), {})[str(slot)] = [identity, revision, order]
    meta = engine.store.get('meta')['value']
    meta['notice_fences'] = {'capacity': capacity, 'next': max((v[3] for v in entries), default=-1) + 1,
                             'overflow': overflow, 'generation': 0}
    ops = [{'op': 'set', 'key': key, 'value': value, 'expect': 0} for key, value in buckets.items()]
    ops.append({'op': 'set', 'key': 'meta', 'value': meta, 'expect': engine.store.get('meta')['version']})
    for offset in range(0, len(ops), 32):
        engine.store.apply(ops[offset:offset + 32], control=True)


class FenceReset(unittest.TestCase):
    def setUp(self):
        self.f = fixtures.EngineTests(); self.f.setUp()
        self.f.engine.notice_fence_capacity = 2

    def args(self):
        return {'op': self.f.op(), 'action': 'reset_fences', 'expect': self.f.engine.notice_fence_status()['revision']}

    def test_owner_growth_retains_every_fence_and_history_no_native_or_config(self):
        seed(self.f.engine, 2, [(0, digest('old'), 7, 0)])
        self.f.engine.notice_fence_capacity = 4
        before = self.f.engine.notice_fences()[0]
        result = self.f.engine.reset_notice_fences(self.args(), fixtures.OWNER)
        self.assertEqual(result['state'], 'recorded')
        status = self.f.engine.notice_fence_status()
        self.assertEqual((status['capacity'], status['stored_capacity'], status['entries']), (4, 4, 1))
        self.assertFalse(status['history_incomplete'])
        self.assertFalse(status['maintenance_pending'])
        self.assertEqual(self.f.engine.notice_fences()[0], before)
        self.assertEqual(self.f.ledger, [])
        self.assertEqual(len(self.f.config), 1)

    def test_shrink_many_buckets_is_bounded_restart_safe_and_keeps_newest(self):
        entries = [(i * 64, digest(i), i, i) for i in range(64)]
        seed(self.f.engine, 4096, entries)
        self.f.engine.notice_fence_capacity = 1
        result = self.f.engine.reset_notice_fences(self.args(), fixtures.OWNER)
        self.assertTrue(result['maintenance_pending'])
        status = self.f.engine.notice_fence_status()
        self.assertTrue(status['history_incomplete'])
        self.assertTrue(status['maintenance_pending'])
        self.assertEqual(status['entries'], 40)  # at most24 bucket mutations, not one oversized transaction
        self.f.engine.fence(); self.f.engine = self.f.make_engine()
        self.f.engine.notice_fence_capacity = 1
        self.f.engine._safe_maintain(); self.f.engine._safe_maintain()
        self.assertFalse(self.f.engine.notice_fence_status()['maintenance_pending'])
        self.assertEqual(self.f.engine.notice_fences()[0], {digest(63): 63})
        # Retain its old slot; no shadow table or multi-page in-place reindex.
        self.assertIn(63 * 64, self.f.engine.notice_fences()[2])
        self.assertEqual(self.f.ledger, [])

    def test_zero_capacity_is_known_and_trims_without_enabling_notices(self):
        seed(self.f.engine, 2, [(0, digest('old'), 1, 0)])
        self.f.engine.notice_fence_capacity = 0
        self.f.engine.reset_notice_fences(self.args(), fixtures.OWNER)
        status = self.f.engine.notice_fence_status()
        self.assertEqual(status['entries'], 0)
        self.assertFalse(status['incomplete'])
        self.assertTrue(status['history_incomplete'])
        with self.assertRaises(PolicyError):
            self.f.engine._remember_notice(Working(self.f.engine), {'source_key': digest('new'), 'revision': 1}, generation=1)

    def test_owner_only_current_cas_and_no_caller_supplied_capacity(self):
        args = self.args()
        with self.assertRaises(PolicyError) as denied:
            self.f.engine.reset_notice_fences(args, fixtures.WORKER)
        self.assertEqual(denied.exception.kind, 'no_grant')
        # Refusal history can change, but no fence header/table is rewritten.
        self.assertNotIn('notice_fences', self.f.store.get('meta')['value'])
        args = self.args(); args['capacity'] = 999999
        with self.assertRaises(PolicyError) as bad:
            self.f.engine.reset_notice_fences(args, fixtures.OWNER)
        self.assertEqual(bad.exception.kind, 'invalid')
        args = self.args(); self.f.engine.notice_fence_capacity = 3
        with self.assertRaises(PolicyError) as stale:
            self.f.engine.reset_notice_fences(args, fixtures.OWNER)
        self.assertEqual(stale.exception.kind, 'conflict')
        self.assertNotIn('notice_fences', self.f.store.get('meta')['value'])

    def test_unknown_capacity_refuses_and_duplicate_never_resets_twice(self):
        self.f.engine.notice_fence_capacity = None
        with self.assertRaises(PolicyError) as unknown:
            self.f.engine.reset_notice_fences(self.args(), fixtures.OWNER)
        self.assertEqual(unknown.exception.kind, 'unavailable')
        self.f.engine.notice_fence_capacity = 2
        args = self.args()
        self.f.engine.reset_notice_fences(args, fixtures.OWNER)
        self.assertTrue(self.f.engine.reset_notice_fences(args, fixtures.OWNER)['historical'])
        self.assertEqual(self.f.engine.notice_fence_status()['generation'], 1)
        with self.assertRaises(PolicyError) as stale:
            self.f.engine.reset_notice_fences({**args, 'op': self.f.op()}, fixtures.OWNER)
        self.assertEqual(stale.exception.kind, 'conflict')

    def test_unacknowledged_change_never_trims_and_stale_observation_cannot_admit(self):
        seed(self.f.engine, 2, [(0, digest('old'), 1, 0), (1, digest('newer'), 1, 1)])
        self.f.engine.notice_fence_capacity = 1
        self.f.engine._safe_maintain()
        self.assertEqual(len(self.f.engine.notice_fences()[0]), 2)
        self.f.engine.reset_notice_fences(self.args(), fixtures.OWNER)
        work = Working(self.f.engine)
        with self.assertRaises(PolicyError) as stale:
            self.f.engine._remember_notice(work, {'source_key': digest('different'), 'revision': 2}, after_gap=True, generation=0)
        self.assertEqual(stale.exception.kind, 'conflict')
        self.assertEqual(work.changes, {})

    def test_unconfirmed_ack_never_deletes_before_durable_history_gap(self):
        seed(self.f.engine, 2, [(0, digest('old'), 1, 0), (1, digest('newer'), 1, 1)])
        self.f.engine.notice_fence_capacity = 1
        args = self.args(); self.f.store.fault = 'after'
        with self.assertRaises((PolicyError, StoreError)):
            self.f.engine.reset_notice_fences(args, fixtures.OWNER)
        self.assertEqual(len(self.f.engine.notice_fences()[0]), 2)
        self.assertTrue(self.f.store.get('meta')['value']['notice_fences']['overflow'])
        self.f.engine._safe_maintain()
        self.assertEqual(self.f.engine.notice_fences()[0], {digest('newer'): 1})
        self.assertEqual(self.f.ledger, [])


class ResetRouting(unittest.TestCase):
    def test_ack_growth_reconsiders_only_never_admitted_current_question(self):
        ext = services.FakeExtension(); routes.setup_routes(ext.entries, ext.resources)
        ext.snapshot_header['identity_reservations']['max_entries'] = 1
        live = {}; ext.publish = lambda name, value: live.__setitem__(name, copy.deepcopy(value))
        service = services.CoordinationService(ext)
        seed(service.engine, 1, [(0, digest('different'), 1, 0)])
        ext.snapshot_header['identity_reservations']['max_entries'] = 2
        service.route_requests()  # current question refused while stored capacity still1
        self.assertEqual(service.engine.records(), [])
        self.assertTrue(live['coord-requests-r1']['human'])
        preview = service.route('request_route', {'action': 'inspect_fences'}, fixtures.OWNER)
        service.route('request_route', {'op': service.engine.window()['epoch'] + ':grow',
                      'action': 'reset_fences', 'expect': preview['revision']}, fixtures.OWNER)
        service.route_requests()
        self.assertEqual(len(service.engine.records()), 1)
        service.route_requests()
        self.assertEqual(len(service.engine.records()), 1)
        self.assertFalse(any(method == 'resource.invoke' for method, _ in ext.vqro.calls))

    def test_actual_header_shrink_relearns_stale_baseline_without_resending_current_question(self):
        ext = services.FakeExtension(); routes.setup_routes(ext.entries, ext.resources)
        ext.snapshot_header['identity_reservations']['max_entries'] = 2
        live = {}; ext.publish = lambda name, value: live.__setitem__(name, copy.deepcopy(value))
        service = services.CoordinationService(ext)
        question = ext.resources['r1']['data']['agent']['requests'][0]
        identity = request_identity_key(ext.resources['r1'], question)
        seed(service.engine, 2, [(0, identity, 1, 0), (1, digest('newer'), 1, 1)])
        service.request_baselines[identity] = 0
        service.request_fence_generation = 0
        ext.snapshot_header['identity_reservations']['max_entries'] = 1
        preview = service.route('request_route', {'action': 'inspect_fences'}, fixtures.OWNER)
        self.assertEqual(preview['capacity'], 1)
        service.route('request_route', {'op': service.engine.window()['epoch'] + ':ack',
                      'action': 'reset_fences', 'expect': preview['revision']}, fixtures.OWNER)
        service.route_requests()
        self.assertEqual(service.engine.records(), [])
        self.assertEqual(live['coord-requests-r1']['fallback_reason'], 'notice_history_incomplete')
        self.assertEqual(service.request_baselines[identity], 1)
        question['revision'] = 2
        service.route_requests()
        self.assertEqual(len(service.engine.records()), 1)
        self.assertFalse(any(method == 'resource.invoke' for method, _ in ext.vqro.calls))


if __name__ == '__main__':
    unittest.main()
