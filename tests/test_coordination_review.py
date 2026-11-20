"""Permanent regressions for the independent four-case review."""
import copy
import threading
import unittest
import test_coordination_engine as fixtures
from coordination_engine import IDENTITY_SUMMARY_BYTES
from coordination_policy import MEMBERSHIP_REVISION, PolicyError
from vqro_ext import VqroError


class ReviewRegressions(unittest.TestCase):
    def setUp(self):
        self.f = fixtures.EngineTests()
        self.f.setUp()

    def test_approximate_idle_never_claims_start_and_preserves_boundary(self):
        f = self.f
        f.setup_pull(context=True)
        pull = f.resources['r1']['data']['agent']['pull']
        pull['points'] = {'idle': {'precision': 'approximate'}}
        f.resources['r1']['data']['agent']['state'] = 'running'
        start = f.send(mode='start', render=['context'])
        boundary = f.send(mode='boundary', render=['context'], send_new=True)
        result = f.engine.take({'op': f.op(), 'agent': 'r1', 'consumer': 'c1', 'at': 'idle'}, fixtures.OWNER)
        self.assertEqual([r['delivery'] for r in result['records']], [boundary['delivery']])
        self.assertEqual(result['records'][0]['mode'], 'boundary')
        self.assertIsNone(f.engine.get(start['delivery'])['attempt'])

    def test_unknown_precision_fails_closed(self):
        f = self.f
        f.setup_pull()
        f.resources['r1']['data']['agent']['pull']['points'] = {'idle': {'precision': 'future'}}
        sent = f.send()
        result = f.engine.take({'op': f.op(), 'agent': 'r1', 'consumer': 'c1', 'at': 'idle'}, fixtures.OWNER)
        self.assertEqual(result['records'], [])
        self.assertIsNone(f.engine.get(sent['delivery'])['attempt'])

    def test_disjoint_membership_updates_cas_one_forest_revision(self):
        f = self.f
        f.resources['r2'] = fixtures.resource('r2')
        for rid in ('r1', 'r2'):
            f.config['doc/agents/members/' + rid] = {'version': 1, 'value': {'project': 'p', 'reports_to': None}}
        barrier, lock = threading.Barrier(2), threading.Lock()
        results, errors = [], []
        def apply(ops, context):
            barrier.wait(3)
            with lock:
                if any(f.config.get(op['key'], {}).get('version', 0) != op['expect'] for op in ops):
                    data = {'kind': 'conflict', 'outcome': 'rejected'}
                    raise VqroError('forest changed', 'conflict', data=data,
                                    wire_error={'code': -32000, 'message': 'forest changed', 'data': data})
                for op in ops:
                    f.config[op['key']] = {'version': 2, 'value': copy.deepcopy(op['value'])}
            return {'id': 2}
        f.engine.config_apply = apply
        def move(rid, parent):
            try:
                results.append(f.engine.config_action('membership', {'op': f.op(), 'agent': rid,
                    'project': 'p', 'reports_to': parent, 'expect': 1}, fixtures.OWNER))
            except BaseException as error:
                errors.append(error)
        threads = [threading.Thread(target=move, args=('r1', 'r2')), threading.Thread(target=move, args=('r2', 'r1'))]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(5)
            self.assertFalse(thread.is_alive())
        self.assertEqual(errors, [])
        self.assertEqual(sorted(r['state'] for r in results), ['accepted', 'rejected'])
        self.assertIn(MEMBERSHIP_REVISION, f.config)
        f.engine.policy()  # full resulting forest is still valid

    def test_names_only_binding_cannot_join_a_team_or_widen_its_grants(self):
        f = self.f
        f.resources['r2'] = fixtures.resource('r2')
        f.resources['r1']['data']['agent']['principal'] = 'p:legit'
        f.config['doc/agents/members/r1'] = {'version': 1, 'value': {'project': 'p', 'reports_to': None}}
        f.config['doc/agents/grants/name-only'] = {'version': 1, 'value': {'who': 'p:attacker', 'on': 'agent:r1', 'can': ['names']}}
        f.config['doc/agents/grants/team-send'] = {'version': 1, 'value': {'who': 'team:r1', 'on': 'agent:r2', 'can': ['send:start']}}
        self.assertFalse(f.engine.policy().allowed('p:attacker', 'send:start', 'r2'))
        def apply(ops, context):
            for op in ops:
                f.config[op['key']] = {'version': 2, 'value': copy.deepcopy(op['value'])}
            return {'id': 2}
        f.engine.config_apply = apply
        context = dict(fixtures.OWNER, principal='p:attacker', actor='p:attacker')
        f.engine.config_action('names', {'op': f.op(), 'handle': '@root', 'agent': 'r1', 'expect': 0,
                                       'endpoint_principal': 'p:attacker'}, context)
        policy = f.engine.policy()
        self.assertFalse(policy.allowed('p:attacker', 'send:start', 'r2'))
        self.assertNotIn('p:attacker', policy._principal_agents)
        # Even authorized issuance cannot mistake an alias claim for conflicting
        # native identity. The caller must grant the exact principal explicitly.
        with self.assertRaises(PolicyError):
            policy.grant_ops('p:owner', 'explicit', {'who': '@root', 'on': 'agent:r2', 'can': ['send:start']}, expect=0)

    def test_legal_identity_and_handles_fit_three_kib_reservation(self):
        f = self.f
        f.resources['r1'].update(provider='p' * 128, instance='\x01' * 256, backend_ref='r' * 128, incarnation='n' * 128)
        for n in range(4):
            f.config['doc/agents/names/@' + str(n) + 'x' * 29] = {'version': 1, 'value': {'agent': 'r1'}}
        f.send()
        summary = f.engine.tombstones()['r1']
        from coordination_store import encoded
        self.assertGreater(encoded(summary), 2048)
        self.assertLessEqual(encoded(summary), IDENTITY_SUMMARY_BYTES)
        charge, _ = f.engine._reservation(f.store.view())
        self.assertGreaterEqual(charge, IDENTITY_SUMMARY_BYTES)

    def test_oversize_identity_is_explicit_and_does_not_block_whole_scope(self):
        f = self.f
        f.resources['r2'] = fixtures.resource('r2')
        f.resources['r2']['instance'] = 'x' * 5000
        paused = f.resume(action='pause')
        self.assertEqual(paused['state'], 'accepted')
        resumed = f.resume()
        self.assertEqual(resumed['selected'], 2)
        self.assertEqual(f.engine.permits, {'r1', 'r2'})
        stone = f.engine.tombstones()['r2']
        self.assertTrue(stone['identity_incomplete'])
        self.assertTrue(stone['identity']['incomplete'])
        sent = f.send()
        other = f.op()
        bad = f.engine.send({'op': other, 'submission': other, 'to': 'r2', 'mode': 'start',
                            'content': [{'type': 'text', 'text': 'held'}]}, fixtures.OWNER)
        f.engine.dispatch_one()
        f.engine.dispatch_one()
        self.assertEqual(f.engine.get(sent['delivery'])['state'], 'accepted')
        self.assertEqual(f.engine.get(bad['delivery'])['reason'], 'identity_unavailable')
        self.assertTrue(all(target == 'r1' for target, _, _ in f.ledger))

    def test_sixty_four_claims_reconcile_in_bounded_pages_without_replay(self):
        f = self.f
        f.setup_pull()
        f.config['doc/agents/grants/senders'] = {'version': 1, 'value': {'who': '*', 'on': 'agent:r1', 'can': ['send:admit']}}
        consumer, deliveries, old_claim = 'c1', [], None
        for batch in range(4):
            for n in range(16):
                principal = 'p:s' + str(batch * 2 + n // 8)
                op = f.op()
                sent = f.engine.send({'op': op, 'submission': op, 'to': 'r1', 'mode': 'admit', 'send_new': True,
                    'content': [{'type': 'text', 'text': 'm'}]}, dict(fixtures.OWNER, principal=principal, actor=principal))
                deliveries.append(sent['delivery'])
            taken = f.engine.take({'op': f.op(), 'agent': 'r1', 'consumer': consumer, 'at': 'tool', 'limit': 16}, fixtures.OWNER)
            self.assertEqual(len(taken['records']), 16)
            old_claim = taken['records'][0]
            consumer = 'c' + str(batch + 2)
            for page in range(10):
                result = f.engine.reconcile({'op': f.op(), 'agent': 'r1', 'consumer': consumer, 'records': []}, fixtures.OWNER)
                self.assertGreater(len(result['records']), 0)
                if result['state'] == 'reconciled':
                    break
                self.assertGreater(result['remaining'], 0)
                held = f.engine.take({'op': f.op(), 'agent': 'r1', 'consumer': consumer, 'at': 'tool'}, fixtures.OWNER)
                self.assertEqual(held['held'], 'reconciling')
                self.assertEqual(held['records'], [])
            else:
                self.fail('bounded reconcile did not finish')
        self.assertEqual(len(deliveries), 64)
        self.assertTrue(all(f.engine.get(d)['state'] == 'unknown' for d in deliveries))
        self.assertEqual(f.ledger, [])
        proofs = [{'delivery': delivery, 'entry': 'entry-' + delivery} for delivery in deliveries]
        progress = f.engine.reconcile({'op': f.op(), 'agent': 'r1', 'consumer': 'positive', 'records': proofs}, fixtures.OWNER)
        self.assertEqual(progress['state'], 'reconciling')
        self.assertEqual(progress['remaining'], 8)
        held = f.engine.take({'op': f.op(), 'agent': 'r1', 'consumer': 'positive', 'at': 'tool'}, fixtures.OWNER)
        self.assertEqual(held['held'], 'reconciling')
        final = f.engine.reconcile({'op': f.op(), 'agent': 'r1', 'consumer': 'positive', 'records': proofs}, fixtures.OWNER)
        self.assertEqual(final['state'], 'reconciled')
        self.assertEqual(len(final['records']), 8)
        self.assertTrue(all(f.engine.get(d)['state'] == 'inserted' for d in deliveries))
        self.assertEqual(f.ledger, [])
        with self.assertRaises(PolicyError):
            f.engine.report({'op': f.op(), 'delivery': old_claim['delivery'], 'claim': old_claim['claim'],
                             'consumer': 'c4', 'stage': 'inserted'}, fixtures.OWNER)

    def test_large_records_reconcile_by_byte_budget_and_keep_entry_evidence(self):
        f = self.f
        f.setup_pull()
        f.config['doc/agents/grants/large'] = {'version': 1, 'value': {'who': '*', 'on': 'agent:r1', 'can': ['send:admit']}}
        for n in range(16):
            principal = 'p:large' + str(n // 8)
            op = f.op()
            f.engine.send({'op': op, 'submission': op, 'to': 'r1', 'mode': 'admit', 'send_new': True,
                           'content': [{'type': 'text', 'text': 'x' * 6000}]},
                          dict(fixtures.OWNER, principal=principal, actor=principal))
        taken = f.engine.take({'op': f.op(), 'agent': 'r1', 'consumer': 'c1', 'at': 'tool', 'limit': 16}, fixtures.OWNER)
        # The take itself is byte bounded; reconcile every actual claim.
        claims = taken['records']
        self.assertGreater(len(claims), 0)
        proofs = [{'delivery': r['delivery'], 'entry': 'x' * 900} for r in claims]
        result = f.engine.reconcile({'op': f.op(), 'agent': 'r1', 'consumer': 'c2', 'records': proofs}, fixtures.OWNER)
        self.assertEqual(result['state'], 'reconciling')
        self.assertLess(len(result['records']), len(claims))
        while result['state'] != 'reconciled':
            result = f.engine.reconcile({'op': f.op(), 'agent': 'r1', 'consumer': 'c2', 'records': proofs}, fixtures.OWNER)
        for claim in claims:
            record = f.engine.get(claim['delivery'])
            self.assertEqual(record['state'], 'inserted')
            self.assertEqual(record['attempt']['claim'], claim['claim'])
            self.assertEqual(record['result']['evidence'], 'entry')


if __name__ == '__main__':
    unittest.main()
