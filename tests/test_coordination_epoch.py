"""Native queue actions retain the runtime epoch, not a guessed route generation."""
import unittest
import test_coordination_engine as fixtures


class QueueEpochTests(unittest.TestCase):
    def setUp(self):
        self.f = f = fixtures.EngineTests(); f.setUp()
        f.resources['r1']['data']['agent']['caps']['agent.queue']['actions'] = ['list', 'clear']
        self.calls = []
        def invoke(target, operation, args, deadline):
            self.calls.append((operation, args))
            if operation == 'agent.queue' and args.get('epoch') != f.resources['r1']['data']['agent']['epoch']:
                return {'outcome': 'rejected', 'error': {'kind': 'stale'}}
            return {'outcome': 'observed'}
        f.engine.invoke = invoke

    def test_interrupt_then_start_queue_clear_has_current_native_epoch(self):
        f = self.f
        f.resources['r1']['data']['agent']['epoch'] = 'native-current-2'
        f.send(mode='preempt', strategy='interrupt_then_start')
        f.resume()
        self.assertEqual(f.engine.dispatch_one()['state'], 'observed')
        self.assertEqual([op for op, _ in self.calls], ['agent.queue', 'agent.interrupt', 'agent.send'])
        self.assertEqual(self.calls[0][1]['epoch'], 'native-current-2')

    def test_explicit_queue_defaults_epoch_but_preserves_supplied_stale_guard(self):
        f = self.f
        accepted = f.engine.control({'op': f.op(), 'target': 'r1', 'action': 'queue', 'args': {'action': 'list'}}, fixtures.OWNER)
        self.assertEqual(accepted['state'], 'observed')
        self.assertEqual(self.calls[-1][1]['epoch'], 'e1')
        stale = f.engine.control({'op': f.op(), 'target': 'r1', 'action': 'queue', 'args': {'action': 'list', 'epoch': 'old'}}, fixtures.OWNER)
        self.assertEqual(stale['state'], 'rejected')
        self.assertEqual(self.calls[-1][1]['epoch'], 'old')


if __name__ == '__main__':
    unittest.main()
