"""I22 predecessor visibility shares the scheduler's lane predicate; reads never resolve."""
import unittest
import test_coordination_engine as fixtures
from coordination_policy import PolicyError


class OrderingVisibilityTests(unittest.TestCase):
    def waiting(self):
        f = fixtures.EngineTests(); f.setUp()
        original = f.engine.invoke
        def invoke(*args):
            reply = original(*args)
            return {'outcome': 'indeterminate'} if len(f.ledger) == 1 else reply
        f.engine.invoke = invoke
        predecessor = f.send()['delivery']
        f.resume(); f.engine.dispatch_one()
        self.assertEqual(f.engine.get(predecessor)['state'], 'unknown')
        successor = f.send(send_new=True)['delivery']
        self.assertFalse(f.engine.dispatch_one())
        return f, predecessor, successor

    def test_get_list_attention_name_predecessor_without_mutating_or_replaying(self):
        f, predecessor, successor = self.waiting()
        revision = f.store.revision
        for _ in range(3):
            got = f.engine.get(successor)
            listed = next(r for r in f.engine.listing({'state': 'held'})['items'] if r['delivery'] == successor)
            attention = next(r for r in f.engine.attention_summary()['items'] if r['delivery'] == successor)
            for row in (got, listed, attention):
                self.assertEqual(row['state'], 'held')
                self.assertEqual(row['reason'], 'ordering_unknown')
                self.assertEqual(row['ordering_predecessor'], predecessor)
        self.assertEqual(f.store.revision, revision)
        self.assertEqual(len(f.ledger), 1)
        self.assertEqual(f.engine.get(predecessor)['state'], 'unknown')
        self.assertNotIn('ordering_predecessor', f.store.get('message/' + successor)['value'])

    def test_resolution_rechecks_pause_and_never_replays_the_predecessor(self):
        f, predecessor, successor = self.waiting()
        with self.assertRaises(PolicyError):
            f.engine.resolve({'op': f.op(), 'delivery': predecessor, 'as': 'not_delivered'}, {'principal': 'p:other'})
        f.resume(action='pause')
        f.engine.resolve({'op': f.op(), 'delivery': predecessor, 'as': 'not_delivered'}, fixtures.OWNER)
        self.assertFalse(f.engine.dispatch_one())
        held = f.engine.get(successor)
        self.assertEqual(held['reason'], 'paused')
        self.assertNotIn('ordering_predecessor', held)
        f.resume(); self.assertTrue(f.engine.dispatch_one())
        self.assertEqual([call[2]['delivery'] for call in f.ledger], [predecessor, successor])

    def test_a_different_sender_or_mode_does_not_inherit_the_dependency(self):
        f, _, _ = self.waiting()
        f.config['doc/agents/grants/other'] = {'version': 1, 'value': {
            'who': 'p:other', 'on': 'agent:r1', 'can': ['send:start']}}
        op = f.op()
        other = f.engine.send({'op': op, 'submission': op, 'to': 'r1', 'mode': 'start',
            'content': [{'type': 'text', 'text': 'other sender'}]}, {'principal': 'p:other', 'actor': 'p:other'})['delivery']
        different_mode = f.send(mode='admit', send_new=True)['delivery']
        self.assertNotIn('ordering_predecessor', f.engine.get(other))
        self.assertNotIn('ordering_predecessor', f.engine.get(different_mode))

    def test_list_cursor_is_fenced_when_only_the_dependency_changes(self):
        f, predecessor, successor = self.waiting()
        f.resources['r2'] = fixtures.resource('r2')
        f.config['doc/agents/grants/bulk'] = {'version': 1, 'value': {'who': '*', 'on': '*', 'can': ['send:start']}}
        f.resume(action='pause')
        for n in range(66):
            op = f.op(); principal = 'p:bulk' + str(n // 8)
            f.engine.send({'op': op, 'submission': op, 'to': 'r1' if n % 2 else 'r2', 'mode': 'start',
                'send_new': True, 'content': [{'type': 'text', 'text': str(n)}]},
                {'principal': principal, 'actor': principal})
        self.assertFalse(f.engine.dispatch_one())
        page = f.engine.listing({'state': 'held'})
        self.assertIsNotNone(page['next'])
        version = f.store.get('message/' + successor)['version']
        f.engine.resolve({'op': f.op(), 'delivery': predecessor, 'as': 'not_delivered'}, fixtures.OWNER)
        self.assertEqual(f.store.get('message/' + successor)['version'], version)
        with self.assertRaises(PolicyError) as changed:
            f.engine.listing({'state': 'held', 'page': page['next']})
        self.assertEqual(changed.exception.kind, 'conflict')
