"""Retained claims, explicit handover recovery and layered sender provenance."""
import copy
import unittest
import test_coordination_engine as fixtures
from test_coordination_service import FakeExtension
from coordination import CoordinationService
from coordination_policy import PolicyError


class ContinuityTests(unittest.TestCase):
    def test_lost_take_history_survives_window_rotation_without_content(self):
        f = fixtures.EngineTests(); f.setUp(); f.setup_pull()
        sent = f.send(mode='admit')
        take_op = f.op()
        take_args = {'op': take_op, 'agent': 'r1', 'consumer': 'c1', 'at': 'tool'}
        taken = f.engine.take(take_args, fixtures.OWNER)
        token = taken['records'][0]['claim']
        old_epoch = f.engine.window()['epoch']
        for _ in range(1300):
            try:
                f.engine.report({'op': f.op(), 'delivery': 'd:missing', 'claim': 'x', 'stage': 'read'}, fixtures.OWNER)
            except PolicyError:
                pass
            if f.engine.window()['epoch'] != old_epoch:
                break
        else:
            self.fail('window did not close')
        frozen, revision = f.engine.get(sent['delivery']), f.store.revision
        replay = f.engine.take(take_args, fixtures.OWNER)
        self.assertTrue(replay['historical'])
        self.assertEqual(replay['records'], [])
        self.assertEqual(replay['claims'][0]['claim'], token)
        self.assertEqual(f.store.revision, revision, 'retained historical replay is read-only')
        # Force actual eviction of the old cached result, rather than relying on
        # unrelated random buckets. All calls still go through public mutations.
        while True:
            compact_op = f.op()
            if f.engine._bucket(fixtures.OWNER['principal'], compact_op) == f.engine._bucket(fixtures.OWNER['principal'], take_op):
                break
        with self.assertRaises(PolicyError):
            f.engine.report({'op': compact_op, 'delivery': 'd:missing', 'claim': 'x', 'stage': 'read'}, fixtures.OWNER)
        revision = f.store.revision
        with self.assertRaises(PolicyError) as expired:
            f.engine.take(take_args, fixtures.OWNER)
        self.assertEqual(expired.exception.kind, 'expired_operation')
        self.assertEqual(f.store.revision, revision, 'expired replay is read-only too')
        self.assertEqual(f.engine.get(sent['delivery']), frozen)
        history = f.engine.operation(fixtures.OWNER['principal'], take_op)
        self.assertEqual(history['records'], [])
        self.assertEqual(history['claims'][0]['delivery'], sent['delivery'])
        self.assertEqual(history['claims'][0]['claim'], token)
        self.assertEqual(f.engine.get(sent['delivery'])['state'], 'claimed')
        self.assertEqual(f.ledger, [])

    def test_abort_restores_pending_target_without_undoing_accepted_stage(self):
        f = fixtures.EngineTests(); f.setUp()
        sent = f.send()
        original = f.engine.get(sent['delivery'])['to']
        f.resources['r1']['data']['agent']['lifecycle'] = 'ended'
        f.resources['r2'] = fixtures.resource('r2')
        def apply(ops, context):
            for op in ops:
                if f.config.get(op['key'], {}).get('version', 0) != op['expect']:
                    raise PolicyError('conflict', 'CAS changed')
            for op in ops:
                f.config[op['key']] = {'version': 2, 'value': copy.deepcopy(op['value'])}
            return {'id': 2}
        f.engine.config_apply = apply
        plan = f.engine.handover({'op': f.op(), 'from': {'agent': 'r1', 'incarnation': 'i1'},
            'to': {'agent': 'r2', 'incarnation': 'i1'}, 'deliveries': [sent['delivery']],
            'bind': [{'handle': '@kept', 'agent': 'r2', 'expect': 0},
                     {'handle': '@bad', 'agent': 'r2', 'expect': 99}]}, fixtures.OWNER)['plan']
        f.engine.handover({'op': f.op(), 'action': 'stage', 'plan': plan, 'stage': 0}, fixtures.OWNER)
        with self.assertRaises(PolicyError):
            f.engine.handover({'op': f.op(), 'action': 'stage', 'plan': plan, 'stage': 1}, fixtures.OWNER)
        result = f.engine.handover({'op': f.op(), 'action': 'abort', 'plan': plan}, fixtures.OWNER)
        self.assertEqual(result['configuration'], 'not_reverted')
        restored = f.engine.get(sent['delivery'])
        self.assertEqual(restored['to'], original)
        self.assertEqual(restored['state'], 'held')
        self.assertNotIn('handover_plan', restored)
        self.assertEqual(f.config['doc/agents/names/@kept']['value']['agent'], 'r2')
        self.assertNotIn('r1', f.engine.permits)
        self.assertNotIn('r2', f.engine.permits)
        with self.assertRaises(PolicyError):
            f.engine.handover({'op': f.op(), 'action': 'stage', 'plan': plan, 'stage': 1}, fixtures.OWNER)
        self.assertEqual(f.ledger, [])

    def test_complete_and_abort_skip_withdrawn_members_without_stranding_pending(self):
        for action, withdrawn_count in [('complete', 1), ('abort', 1), ('abort', 2), ('complete', 2)]:
            with self.subTest(action=action, withdrawn=withdrawn_count):
                f = fixtures.EngineTests(); f.setUp()
                deliveries = [f.send(send_new=True)['delivery'] for _ in range(2)]
                f.resources['r1']['data']['agent']['lifecycle'] = 'ended'
                f.resources['r2'] = fixtures.resource('r2')
                f.resources['r2']['data']['agent']['session'] = {'id': 's2'}
                plan = f.engine.handover({'op': f.op(), 'from': {'agent': 'r1', 'incarnation': 'i1'},
                    'to': {'agent': 'r2', 'incarnation': 'i1'}, 'deliveries': deliveries}, fixtures.OWNER)['plan']
                for delivery in deliveries[:withdrawn_count]:
                    f.engine.edit({'op': f.op(), 'delivery': delivery,
                                   'expect': f.engine.get(delivery)['revision']}, fixtures.OWNER, withdraw=True)
                result = f.engine.handover({'op': f.op(), 'action': action, 'plan': plan}, fixtures.OWNER)
                self.assertEqual(result['state'], 'completed' if action == 'complete' else 'aborted')
                for n, delivery in enumerate(deliveries):
                    record = f.engine.get(delivery)
                    self.assertNotIn('handover_plan', record)
                    if n < withdrawn_count:
                        self.assertEqual(record['state'], 'withdrawn')
                    else:
                        self.assertEqual(record['state'], 'held')
                        self.assertEqual(record['to']['agent'], 'r2' if action == 'complete' else 'r1')
                self.assertEqual(f.ledger, [])

    def test_native_transport_identity_is_hello_while_sender_is_inbox_record(self):
        ext = FakeExtension()
        service = CoordinationService(ext)
        sender = dict(fixtures.OWNER, provenance='inbox-record')
        args = {'origin': {'principal': 'p:original', 'actor': 'p:original', 'agent': 'r9', 'sender': sender}}
        service.invoke('r1', 'agent.send', args, 1000)
        sent = ext.vqro.calls[-1][1]['args']['origin']
        self.assertEqual(sent['principal'], ext.info['principal'])
        self.assertEqual(sent['actor'], ext.info['actor'])
        self.assertEqual(sent['sender'], sender)
        self.assertNotIn('agent', sent)
        self.assertEqual(args['origin']['principal'], 'p:original', 'caller arguments are not mutated')


if __name__ == '__main__':
    unittest.main()
