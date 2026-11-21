"""Completed handover diagnostics do not re-evaluate stale members or wake again."""
import unittest
import test_coordination_engine as fixtures
from coordination_policy import PolicyError


class CompletedHandover(unittest.TestCase):
    def setUp(self):
        self.f = fixtures.EngineTests(); self.f.setUp()
        self.sent = self.f.send()
        self.f.resources['r1']['data']['agent']['lifecycle'] = 'ended'
        self.f.resources['r2'] = fixtures.resource('r2')
        self.wakes = []
        self.f.engine.wake = lambda *args: self.wakes.append(args)
        self.plan = self.f.engine.handover({'op': self.f.op(),
            'from': {'agent': 'r1', 'incarnation': 'i1'}, 'to': {'agent': 'r2', 'incarnation': 'i1'},
            'deliveries': [self.sent['delivery']]}, fixtures.OWNER)['plan']
        self.complete = {'op': self.f.op(), 'action': 'complete', 'plan': self.plan}
        self.f.engine.handover(self.complete, fixtures.OWNER)

    def test_fresh_complete_on_completed_is_final_without_plan_member_gate_or_wake_changes(self):
        keys = ['plan/' + self.plan, 'message/' + self.sent['delivery'], 'control']
        before = {key: self.f.store.get(key) for key in keys}
        wakes = list(self.wakes)
        with self.assertRaises(PolicyError) as final:
            self.f.engine.handover({**self.complete, 'op': self.f.op()}, fixtures.OWNER)
        self.assertEqual(final.exception.kind, 'conflict')
        self.assertEqual(str(final.exception), 'handover is already final')
        self.assertEqual({key: self.f.store.get(key) for key in keys}, before)
        self.assertEqual(self.wakes, wakes)
        self.assertEqual(self.f.ledger, [])
        self.assertTrue(self.f.engine.handover(self.complete, fixtures.OWNER)['historical'])

    def test_final_diagnostic_precedes_changed_successor_but_not_authority(self):
        self.f.resources['r2']['incarnation'] = 'replacement'
        with self.assertRaises(PolicyError) as final:
            self.f.engine.handover({**self.complete, 'op': self.f.op()}, fixtures.OWNER)
        self.assertEqual(str(final.exception), 'handover is already final')
        with self.assertRaises(PolicyError) as denied:
            self.f.engine.handover({**self.complete, 'op': self.f.op()}, fixtures.WORKER)
        self.assertEqual(denied.exception.kind, 'no_grant')
        self.assertEqual(self.f.ledger, [])


if __name__ == '__main__':
    unittest.main()
