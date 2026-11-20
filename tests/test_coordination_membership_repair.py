"""Explicit invalid-forest recovery: ordinary CAS writes, never inferred team authority."""
import copy
import threading
import unittest
import test_coordination_engine as engine_fixtures
from test_coordination_policy import Fixture, MEMBERS, GRANTS, entry, grant
from coordination_policy import PolicyError, MEMBERSHIP_REVISION
from coordination_engine import CoordinationError
from vqro_ext import VqroError


class RepairPolicy(Fixture):
    def setUp(self):
        super().setUp()
        self.entries[MEMBERS + 'r1']['value']['reports_to'] = 'r3'

    def args(self, changes=None):
        return {'action': 'repair', 'membership_revision': self.policy().membership_revision,
                'changes': changes if changes is not None else [
                    {'agent': 'r1', 'project': 'p', 'reports_to': None, 'expect': 1}]}

    def apply(self, ops):
        for op in ops:
            self.assertEqual(self.entries.get(op['key'], {}).get('version', 0), op['expect'])
        for op in ops:
            if op['op'] == 'delete':
                self.entries.pop(op['key'], None)
            else:
                self.entries[op['key']] = entry(copy.deepcopy(op['value']), op['expect'] + 1)

    def repair(self, args=None, principal='p:owner'):
        return self.policy().config_ops('coord.membership.set', args or self.args(), principal)

    def test_owner_breaks_cycle_with_only_explicit_row_and_shared_cas(self):
        before = copy.deepcopy(self.entries)
        ops = self.repair()
        self.assertEqual(self.entries, before)
        self.assertEqual({op['key'] for op in ops}, {MEMBERSHIP_REVISION, MEMBERS + 'r1'})
        self.apply(ops)
        self.assertIsNone(self.policy().membership_error)
        self.assertEqual(self.policy().scope('team:r1'), ['r1', 'r2', 'r3'])
        self.assertEqual(self.entries[MEMBERS + 'r2'], before[MEMBERS + 'r2'])
        self.assertEqual(self.entries[MEMBERS + 'r3'], before[MEMBERS + 'r3'])

    def test_two_disjoint_cycles_need_one_explicit_valid_batch(self):
        for rid, parent in [('r1', 'r2'), ('r2', 'r1'), ('r3', 'r4'), ('r4', 'r3')]:
            self.entries[MEMBERS + rid] = entry({'project': 'p', 'reports_to': parent})
        self.rejects('invalid', self.repair)
        args = self.args([{'agent': rid, 'project': 'p', 'reports_to': None, 'expect': 1}
                          for rid in ('r1', 'r3')])
        self.apply(self.repair(args))
        self.assertIsNone(self.policy().membership_error)

    def test_missing_and_stale_stamps_or_row_versions_refuse(self):
        args = self.args(); args.pop('membership_revision')
        self.rejects('invalid', self.repair, args)
        args['membership_revision'] = 'stale'
        self.rejects('conflict', self.repair, args)
        args = self.args(); args['changes'][0]['expect'] = 0
        self.rejects('conflict', self.repair, args)
        args = self.args(); args['changes'][0].pop('expect')
        self.rejects('invalid', self.repair, args)

    def test_malformed_duplicate_and_still_invalid_batches_never_change_rows(self):
        before = copy.deepcopy(self.entries)
        bad = [[], [None], [{'agent': 'r99', 'expect': 0}],
               [{'agent': 'r1', 'expect': 1, 'reports_to': 'r99'}],
               [{'agent': 'r1', 'project': 'q', 'reports_to': None, 'expect': 1}],
               [{'agent': 'r1', 'action': 'bogus', 'expect': 1}],
               [{'agent': 'r1', 'expect': 1}, {'agent': 'r1', 'expect': 1}]]
        for changes in bad:
            with self.subTest(changes=changes), self.assertRaises(PolicyError):
                self.repair(self.args(changes))
            self.assertEqual(self.entries, before)

    def test_unreadable_untouched_row_is_not_silently_normalized(self):
        self.entries[MEMBERS + 'r4'] = entry('broken raw row')
        self.rejects('invalid', self.repair)
        args = self.args([{'agent': 'r1', 'project': 'p', 'reports_to': None, 'expect': 1},
                          {'agent': 'r4', 'project': 'q', 'reports_to': None, 'expect': 1}])
        self.entries[GRANTS + 'all'] = entry(grant('p:repair', ['membership']))
        self.rejects('no_grant', self.repair, self.args(args['changes']), 'p:repair')
        self.apply(self.repair(self.args(args['changes'])))
        self.assertIsNone(self.policy().membership_error)

    def test_bad_raw_key_requires_generic_workspace_recovery(self):
        self.entries[MEMBERS + 'not-an-agent'] = entry({'reports_to': None})
        self.rejects('invalid', self.repair)
        self.assertIn(MEMBERS + 'not-an-agent', self.entries)

    def test_project_grant_repairs_without_ever_expanding_invalid_team(self):
        self.entries[GRANTS + 'team'] = entry(grant('p:team', ['membership'], 'team:r1'))
        self.rejects('membership_invalid', self.repair, self.args(), 'p:team')
        self.entries[GRANTS + 'project'] = entry(grant('p:repair', ['membership'], 'project:p'))
        self.apply(self.repair(self.args(), 'p:repair'))
        self.assertIsNone(self.policy().membership_error)

    def test_missing_affected_descendant_or_destination_right_refuses_all(self):
        self.entries[MEMBERS + 'r4']['value']['project'] = 'p'
        changes = [{'agent': 'r1', 'project': 'p', 'reports_to': 'r4', 'expect': 1}]
        for rid in ('r1', 'r3', 'r4'):
            self.entries[GRANTS + rid] = entry(grant('p:repair', ['membership'], 'agent:' + rid))
        self.rejects('no_grant', self.repair, self.args(changes), 'p:repair')  # missing r2 descendant
        self.entries[GRANTS + 'r2'] = entry(grant('p:repair', ['membership'], 'agent:r2'))
        self.entries.pop(GRANTS + 'r4')
        self.rejects('no_grant', self.repair, self.args(changes), 'p:repair')  # missing new coordinator
        self.entries[GRANTS + 'r4'] = entry(grant('p:repair', ['membership'], 'agent:r4'))
        self.apply(self.repair(self.args(changes), 'p:repair'))
        self.assertIsNone(self.policy().membership_error)

    def test_deletion_requires_explicit_reparenting_of_remaining_reports(self):
        self.rejects('invalid', self.repair, self.args([{'agent': 'r1', 'action': 'delete', 'expect': 1}]))
        args = self.args([{'agent': 'r1', 'action': 'delete', 'expect': 1},
                          {'agent': 'r2', 'project': 'p', 'reports_to': None, 'expect': 1}])
        self.apply(self.repair(args))
        self.assertNotIn('r1', self.policy().members)
        self.assertIsNone(self.policy().membership_error)

    def test_no_batch_alternate_for_normal_valid_moves(self):
        self.entries[MEMBERS + 'r1']['value']['reports_to'] = None
        self.rejects('invalid', self.repair)


class RepairEffects(unittest.TestCase):
    def setUp(self):
        self.f = engine_fixtures.EngineTests(); self.f.setUp()
        for rid, parent in [('r1', 'r2'), ('r2', 'r1')]:
            self.f.config[MEMBERS + rid] = entry({'project': 'p', 'reports_to': parent})
        self.calls = []
        def apply(ops, context):
            self.calls.append(copy.deepcopy(ops))
            self.assertTrue(any(key.startswith('external/') and row['value']['state'] == 'claimed'
                                for key, row in self.f.store.view().items()))
            for op in ops:
                self.assertEqual(self.f.config.get(op['key'], {}).get('version', 0), op['expect'])
            for op in ops:
                if op['op'] == 'delete':
                    self.f.config.pop(op['key'], None)
                else:
                    self.f.config[op['key']] = entry(copy.deepcopy(op['value']), op['expect'] + 1)
            return {'txn': 42}
        self.apply = apply
        self.f.engine.config_apply = apply

    def args(self):
        return {'op': self.f.op(), 'action': 'repair',
                'membership_revision': self.f.engine.configuration_status()['membership_revision'],
                'changes': [{'agent': 'r1', 'project': 'p', 'reports_to': None, 'expect': 1}]}

    def test_shared_cas_conflicts_for_competing_prepared_repairs(self):
        barrier, lock = threading.Barrier(2), threading.Lock()
        results, errors = [], []
        def apply(ops, context):
            barrier.wait(3)
            with lock:
                if any(self.f.config.get(op['key'], {}).get('version', 0) != op['expect'] for op in ops):
                    data = {'kind': 'conflict', 'outcome': 'rejected'}
                    raise VqroError('forest changed', 'conflict', data=data,
                                    wire_error={'code': -32000, 'message': 'forest changed', 'data': data})
                for op in ops:
                    self.f.config[op['key']] = entry(copy.deepcopy(op['value']), op['expect'] + 1)
            return {'txn': 42}
        self.f.engine.config_apply = apply
        def repair(agent):
            try:
                args = self.args(); args['changes'][0]['agent'] = agent
                results.append(self.f.engine.config_action('membership', args, engine_fixtures.OWNER))
            except Exception as error:
                errors.append(error)
        threads = [threading.Thread(target=repair, args=(agent,)) for agent in ('r1', 'r2')]
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join(5)
            self.assertFalse(thread.is_alive())
        self.assertEqual(errors, [])
        self.assertEqual(sorted(result['state'] for result in results), ['accepted', 'rejected'])
        self.assertTrue(self.f.engine.configuration_status()['membership_valid'])

    def test_effect_once_historical_repeat_does_not_reapply(self):
        args = self.args()
        self.assertEqual(self.f.engine.config_action('membership', args, engine_fixtures.OWNER)['state'], 'accepted')
        self.assertTrue(self.f.engine.configuration_status()['membership_valid'])
        self.assertTrue(self.f.engine.config_action('membership', args, engine_fixtures.OWNER)['historical'])
        self.assertEqual(len(self.calls), 1)
        self.assertEqual(self.f.ledger, [])

    def test_lost_reply_does_not_reapply_even_when_config_changed(self):
        def lost(ops, context):
            self.apply(ops, context)
            data = {'kind': 'io', 'outcome': 'indeterminate'}
            raise VqroError('lost after application', 'io', data=data,
                            wire_error={'code': -32000, 'message': 'lost after application', 'data': data})
        self.f.engine.config_apply = lost
        args = self.args()
        self.assertEqual(self.f.engine.config_action('membership', args, engine_fixtures.OWNER)['state'], 'unknown')
        self.f.engine.config_action('membership', args, engine_fixtures.OWNER)
        self.assertEqual(len(self.calls), 1)
        self.assertTrue(self.f.engine.configuration_status()['membership_valid'])

    def test_unconfirmed_durable_intent_has_zero_workspace_effects(self):
        args = self.args(); before = copy.deepcopy(self.f.config)
        self.f.store.fault = 'before'
        with self.assertRaises(CoordinationError):
            self.f.engine.config_action('membership', args, engine_fixtures.OWNER)
        self.assertEqual(self.calls, [])
        self.assertEqual(self.f.config, before)

    def test_normal_domain_quota_refusal_has_zero_workspace_effects(self):
        args = self.args(); before = copy.deepcopy(self.f.config)
        self.f.store.byte_limit = 4096
        with self.assertRaises(PolicyError) as failure:
            self.f.engine.config_action('membership', args, engine_fixtures.OWNER)
        self.assertEqual(failure.exception.kind, 'quota')
        self.assertEqual(self.calls, [])
        self.assertEqual(self.f.config, before)


if __name__ == '__main__':
    unittest.main()
