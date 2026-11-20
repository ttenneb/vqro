"""JSON-encoded identity bounds and isolation of unrepresentable history rows."""
import copy
import unittest
import test_coordination_engine as fixtures
import test_coordination_recipe_history as recipes
from coordination_engine import digest
from unittest.mock import patch
import coordination_engine as engine
from coordination_policy import PolicyError
from coordination_store import encoded


class CompactionIsolationTests(unittest.TestCase):
    def test_json_expanded_principals_keep_steps_and_sender_identity_without_replay(self):
        for character in ('"', '\\'):
            with self.subTest(character=character):
                principal = 'p:' + character * 254
                self.assertEqual(len(principal.encode()), 256)
                self.assertGreater(encoded(principal), 256)
                f, delivery, _, args, caller = recipes.RecipeHistoryTests().recipe(
                    large_metadata=True, principal_override=principal)
                record = f.engine.get(delivery)
                self.assertTrue(record.get('incomplete'), f.engine.maintenance_error)
                self.assertEqual(record['from'], {'principal_hash': digest(principal)})
                self.assertEqual(len(record['attempt']['steps']), 3)
                self.assertEqual(record['submission_hash'], digest([principal, args['submission']]))
                self.assertLessEqual(encoded(f.store.get('message/' + delivery)['value']), 1024)
                self.assertIsNone(f.engine.maintenance_error)
                self.assertEqual(f.engine.send(dict(args, op=f.op()), caller)['delivery'], delivery)
                self.assertEqual([r['delivery'] for r in f.engine.listing({'sender': principal})['items']], [delivery])
                self.assertEqual(f.engine.listing({'sender': digest(principal)})['items'], [])
                with self.assertRaises(PolicyError) as late:
                    f.engine.edit({'op': f.op(), 'delivery': delivery, 'expect': record['revision']}, caller)
                self.assertEqual(late.exception.kind, 'too_late')
                with self.assertRaises(PolicyError) as not_sender:
                    f.engine.edit({'op': f.op(), 'delivery': delivery, 'expect': record['revision']},
                                  {'principal': digest(principal)})
                self.assertEqual(not_sender.exception.kind, 'no_grant')
                with patch.object(engine, 'WINDOW', f.engine.window()['used'] + 1):
                    f.resume(action='pause')
                f.send(send_new=True)  # expire the previous-window submissions index
                self.assertNotIn(digest([principal, args['submission']]), f.store.get('submissions')['value'])
                self.assertEqual(f.engine.send(dict(args, op=f.op()), caller)['delivery'], delivery)
                f.engine.fence(); f.engine = f.make_engine()
                self.assertEqual(f.engine.send(dict(args, op=f.op()), caller)['delivery'], delivery)
                self.assertEqual(f.engine.permits, set())
                self.assertEqual(len(f.ledger), 3)

    def test_one_unrepresentable_legacy_record_does_not_abort_other_compaction(self):
        f = fixtures.EngineTests(); f.setUp()
        maintain = f.engine._safe_maintain
        f.engine._safe_maintain = lambda: None
        f.resume()
        bad = f.send()['delivery']; f.engine.dispatch_one()
        good = f.send(send_new=True)['delivery']; f.engine.dispatch_one()
        row = f.store.get('message/' + bad)
        legacy = copy.deepcopy(row['value'])
        legacy['reason'] = 'legacy/future opaque detail' * 200
        f.store.apply([{'op': 'set', 'key': 'message/' + bad, 'expect': row['version'], 'value': legacy}], control=True)
        before = f.store.get('message/' + bad)
        f.engine._safe_maintain = maintain
        f.engine._safe_maintain()
        self.assertEqual(f.store.get('message/' + bad), before, 'unrepresentable evidence stays unchanged')
        self.assertTrue(f.engine.get(good)['incomplete'], 'one bad row cannot poison another sender/history row')
        self.assertLessEqual(encoded(f.store.get('message/' + good)['value']), 1024)
        self.assertEqual(f.engine.maintenance_error['kind'], 'compaction_incomplete')
        self.assertIn('message/' + bad, f.engine.maintenance_error['records'])
        self.assertEqual(len(f.ledger), 2)
