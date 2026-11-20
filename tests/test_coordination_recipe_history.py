"""I11 every settled recipe step stays observable within compact history bounds."""
import copy
import unittest
from unittest.mock import patch
import coordination_engine as engine
import test_coordination_engine as fixtures
from coordination_store import encoded


class RecipeHistoryTests(unittest.TestCase):
    def recipe(self, *, large_metadata=False, final_outcome='rejected', principal_override=None):
        f = fixtures.EngineTests(); f.setUp()
        facts = f.resources['r1']['data']['agent']
        facts['state'] = 'running'
        facts['caps'].update({'agent.queue': {'actions': ['clear']},
            'agent.interrupt': {'waits': 'idle', 'pending': 'unknown', 'scope': 'foreground'}})
        principal = principal_override if principal_override is not None else ('p:' + 'x' * 254 if large_metadata else fixtures.OWNER['principal'])
        caller = {'principal': principal, 'actor': principal, 'author': 'client:recipe'}
        f.config['doc/agents/grants/recipe'] = {'version': 1, 'value': {'who': principal, 'on': 'agent:r1',
            'can': ['send:interrupt_then_start', 'queue', 'interrupt', 'send:start']}}
        op = f.op()
        if large_metadata:
            op += 'o' * (256 - len(op))
        args = {'op': op, 'submission': op, 'to': 'r1', 'mode': 'preempt',
                'strategy': 'interrupt_then_start', 'content': [{'type': 'text', 'text': 'recipe'}]}
        def invoke(target, operation, params, deadline):
            f.ledger.append((target, operation, copy.deepcopy(params)))
            if operation == 'agent.interrupt':
                facts['state'] = 'idle'
            if operation == 'agent.send':
                return {'outcome': final_outcome, 'error': {'kind': 'busy', 'message': 'competing start'},
                        'data': {'opaque': 'x' * 20000}}
            return {'outcome': 'observed', 'data': {'opaque': 'y' * 20000}}
        f.engine.invoke = invoke
        delivery = f.engine.send(args, caller)['delivery']
        f.resume()
        live = f.engine.execute_claim(f.engine.claim_one())
        return f, delivery, live, args, caller

    def check_steps(self, record, expected):
        steps = (record.get('attempt') or {}).get('steps', [])
        self.assertEqual([(s['op'], s['outcome']) for s in steps], expected)
        self.assertTrue(record['incomplete'])
        self.assertLessEqual(encoded(record), 1024)

    def test_compacted_partial_keeps_clear_interrupt_and_rejected_start(self):
        f, delivery, live, _, _ = self.recipe()
        expected = [('agent.queue', 'observed'), ('agent.interrupt', 'observed'), ('agent.send', 'rejected')]
        self.assertEqual(len(live['attempt']['steps']), 3)
        record = f.engine.get(delivery)
        self.assertEqual(record['state'], 'partial')
        self.check_steps(record, expected)
        self.assertEqual(record['result']['native']['error_kind'], 'busy')
        self.assertIsNone(f.engine.maintenance_error)
        f.engine.maintenance()
        self.check_steps(f.engine.get(delivery), expected)
        self.assertEqual(len(f.ledger), 3)

    def test_large_legal_metadata_cannot_drop_steps_or_recreate_submission(self):
        f, delivery, _, args, caller = self.recipe(large_metadata=True)
        expected = [('agent.queue', 'observed'), ('agent.interrupt', 'observed'), ('agent.send', 'rejected')]
        record = f.engine.get(delivery)
        self.check_steps(record, expected)
        self.assertNotIn('submission', record)
        self.assertEqual(record['submission_hash'], engine.digest([caller['principal'], args['submission']]))
        self.assertEqual(record['from']['principal'], caller['principal'])
        self.assertEqual(record['intent'], engine.digest({k: v for k, v in args.items() if k != 'op'}))
        self.assertIsNone(f.engine.maintenance_error)
        again = f.engine.send(dict(args, op=f.op()), caller)
        self.assertEqual(again['delivery'], delivery)
        self.assertEqual(len(f.engine.records()), 1)
        f.resume(); self.assertFalse(f.engine.dispatch_one())
        self.assertEqual(len(f.ledger), 3)

    def test_compact_submission_lookup_survives_window_index_eviction_and_restart(self):
        f, delivery, _, args, caller = self.recipe(large_metadata=True)
        with patch.object(engine, 'WINDOW', f.engine.window()['used'] + 1):
            f.resume(action='pause')
        # A fresh-window admission expires old submission-index entries; the
        # retained final record must still recognize its old opaque submission.
        f.send(send_new=True)
        key = engine.digest([caller['principal'], args['submission']])
        self.assertNotIn(key, f.store.get('submissions')['value'])
        again = f.engine.send(dict(args, op=f.op()), caller)
        self.assertEqual(again['delivery'], delivery)
        f.engine.fence(); f.engine = f.make_engine()
        again = f.engine.send(dict(args, op=f.op()), caller)
        self.assertEqual(again['delivery'], delivery)
        self.assertEqual(len(f.engine.records()), 2)
        self.assertEqual(f.engine.permits, set())
        self.assertEqual(len(f.ledger), 3)

    def test_compacted_success_keeps_every_observed_step_too(self):
        f, delivery, _, _, _ = self.recipe(final_outcome='accepted')
        self.check_steps(f.engine.get(delivery),
            [('agent.queue', 'observed'), ('agent.interrupt', 'observed'), ('agent.send', 'accepted')])
        self.assertEqual(len(f.ledger), 3)
