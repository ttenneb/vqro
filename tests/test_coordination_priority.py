"""I21: priority is an eligible per-target/lane selection rule, not authority."""
import unittest
import test_coordination_engine as fixtures
from coordination_policy import FairQueue, PolicyError


def mail(delivery, sender='a', order=1, priority='normal', mode='start', target='r1'):
    return {'delivery': delivery, 'from': {'principal': sender}, 'to': {'agent': target},
            'order': order, 'priority': priority, 'mode': mode}


class PrioritySelectionTests(unittest.TestCase):
    def test_highest_priority_across_senders_then_equal_priority_round_robin_fifo(self):
        rows = [mail('a2', order=3, priority='high'), mail('a1', order=2, priority='high'),
                mail('b1', sender='b', order=4, priority='high'), mail('c1', sender='c', priority='low')]
        queue = FairQueue()
        picked = []
        while rows:
            chosen = queue.pick(rows)
            picked.append(chosen['delivery'])
            rows.remove(chosen)
        self.assertEqual(picked, ['a1', 'b1', 'a2', 'c1'])

    def test_own_high_may_overtake_own_older_low(self):
        self.assertEqual(FairQueue().pick([mail('low', priority='low'), mail('high', order=2, priority='high')])['delivery'], 'high')

    def test_priority_never_crosses_lanes_or_target_round_robin(self):
        queue = FairQueue()
        self.assertEqual(queue.pick([mail('high', priority='high'), mail('boundary', priority='low', mode='boundary')])['delivery'], 'boundary')
        rows = [mail('r1-high', priority='high'), mail('r2-low', priority='low', target='r2')]
        self.assertEqual(queue.pick(rows)['delivery'], 'r1-high')
        self.assertEqual(queue.pick(rows)['delivery'], 'r2-low')

    def test_missing_priority_is_normal_and_unknown_value_is_refused(self):
        normal = mail('normal')
        normal.pop('priority')
        self.assertEqual(FairQueue().pick([mail('low', priority='low'), normal])['delivery'], 'normal')
        with self.assertRaises(PolicyError):
            FairQueue().pick([mail('bad', priority='urgent')])


class PriorityEngineTests(unittest.TestCase):
    def setUp(self):
        self.f = fixtures.EngineTests()
        self.f.setUp()

    def test_priority_persists_and_is_editable_only_before_claim(self):
        f = self.f
        low = f.send(priority='low')
        high = f.send(priority='high', send_new=True)
        self.assertIsNone(f.engine.dispatch_one())
        f.resume()
        f.engine.dispatch_one()
        self.assertEqual(f.ledger[0][2]['delivery'], high['delivery'])
        high_record = f.engine.get(high['delivery'])
        self.assertEqual(high_record['priority'], 'high')
        with self.assertRaises(PolicyError):
            f.engine.edit({'op': f.op(), 'delivery': high['delivery'], 'expect': high_record['revision'], 'priority': 'low'}, fixtures.OWNER)
        low_record = f.engine.get(low['delivery'])
        f.engine.edit({'op': f.op(), 'delivery': low['delivery'], 'expect': low_record['revision'], 'priority': 'high'}, fixtures.OWNER)
        self.assertEqual(f.engine.get(low['delivery'])['priority'], 'high')

    def test_high_does_not_bypass_unknown_predecessor(self):
        f = self.f
        calls = []
        def uncertain(*args):
            calls.append(args)
            return {'outcome': 'indeterminate', 'error': {'kind': 'backend'}}
        f.engine.invoke = uncertain
        f.send(priority='low')
        f.resume()
        f.engine.dispatch_one()
        high = f.send(priority='high', send_new=True)
        self.assertIsNone(f.engine.dispatch_one())
        self.assertEqual(len(calls), 1)
        self.assertEqual(f.engine.get(high['delivery'])['reason'], 'ordering_unknown')

    def test_normal_default_and_invalid_priority(self):
        f = self.f
        sent = f.send()
        self.assertEqual(f.engine.get(sent['delivery'])['priority'], 'normal')
        with self.assertRaises(PolicyError) as invalid:
            f.send(priority='urgent', send_new=True)
        self.assertEqual(invalid.exception.kind, 'invalid')


if __name__ == '__main__':
    unittest.main()
