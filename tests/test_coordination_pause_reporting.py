"""H4 selection-point pause evidence; fake native calls, no socket/runtime/model."""
from concurrent.futures import ThreadPoolExecutor
import json
import threading
import unittest
import test_coordination_engine as fixtures


class PauseReportingTests(unittest.TestCase):
    def test_pause_wins_without_claim_or_native_input(self):
        f = fixtures.EngineTests(); f.setUp()
        delivery = f.send()['delivery']
        f.resume(action='pause')
        self.assertIsNone(f.engine.claim_one())
        self.assertIsNone(f.engine.get(delivery).get('attempt'))
        self.assertEqual(f.ledger, [])

    def claim_wins(self, early_receipt=False):
        f = fixtures.EngineTests(); f.setUp()
        f.resources['r1']['data']['agent']['caps']['agent.send']['correlation'] = 'insertion'
        delivery = f.send()['delivery']
        entered, released = threading.Event(), threading.Event()
        original = f.engine.invoke
        def held_reply(*args):
            response = original(*args)
            entered.set()
            if not released.wait(5):
                raise AssertionError('owned native reply barrier was not released')
            return response
        f.engine.invoke = held_reply
        f.resume()
        with ThreadPoolExecutor(max_workers=1) as pool:
            native = pool.submit(f.engine.dispatch_one)
            self.assertTrue(entered.wait(3), 'native call must start before pause')
            try:
                claimed = f.engine.get(delivery)
                attempt = claimed['attempt']['id']
                self.assertEqual(claimed['state'], 'claimed')
                if early_receipt:
                    self.assertTrue(f.engine.observe_receipts('r1', 'i1', [{'epoch': 'receipt', 'seq': 1,
                        'receipt': {'delivery': delivery, 'stage': 'read'}}]))
                    self.assertEqual(f.engine.get(delivery)['state'], 'read')
                paused = f.resume(action='pause')
                self.assertIn(attempt, json.dumps(paused), 'pause must report the winning outstanding attempt')
                self.assertEqual(paused['outstanding'], [{'agent': 'r1', 'attempts': [attempt]}])
                self.assertEqual(paused['outstanding_count'], 1)
                self.assertFalse(paused['outstanding_incomplete'])
                history = f.engine.scope_result(paused['receipt'])
                self.assertEqual(history['outstanding_count'], 1)
                self.assertEqual(history['outstanding_hash'], paused['outstanding_hash'])
                self.assertTrue(history['outstanding_incomplete'])
                self.assertNotIn(attempt, json.dumps(history))
                self.assertNotIn(attempt, json.dumps(f.store.get('scope/' + paused['receipt'])))
                self.assertNotIn('hello', json.dumps(paused), 'pause evidence never contains insertion content')
                self.assertEqual(len(f.ledger), 1)
            finally:
                released.set()
            native.result(5)
        later = f.send(send_new=True)['delivery']
        self.assertFalse(f.engine.dispatch_one())
        self.assertIsNone(f.engine.get(later).get('attempt'))
        self.assertEqual(len(f.ledger), 1, 'closing the gate never duplicates the winning native input')

    def test_outstanding_snapshot_covers_targets_beyond_first_target_page(self):
        f = fixtures.EngineTests(); f.setUp()
        f.resources = {'r' + str(n): fixtures.resource('r' + str(n)) for n in range(1, 201)}
        delivery = f.send(to='r99')['delivery']
        f.resume()
        claimed = f.engine.claim_one('r99')
        self.assertEqual(claimed['delivery'], delivery)
        reply = f.resume(action='pause')
        self.assertEqual(reply['selected'], 200)
        self.assertNotIn('r99', reply['targets'])
        self.assertEqual(reply['outstanding'], [{'agent': 'r99', 'attempts': [claimed['attempt']['id']]}])
        self.assertEqual(f.ledger, [])

    def test_unresolved_unknown_attempt_is_not_presented_as_certainly_stopped(self):
        f = fixtures.EngineTests(); f.setUp()
        original = f.engine.invoke
        def uncertain(*args):
            original(*args)
            return {'outcome': 'indeterminate'}
        f.engine.invoke = uncertain
        delivery = f.send()['delivery']
        f.resume(); f.engine.dispatch_one()
        record = f.engine.get(delivery)
        self.assertEqual(record['state'], 'unknown')
        reply = f.resume(action='pause')
        self.assertEqual(reply['outstanding'], [{'agent': 'r1', 'attempts': [record['attempt']['id']]}])
        self.assertEqual(reply['execution'], 'not_asserted')
        self.assertEqual(len(f.ledger), 1)

    def test_claim_wins_and_pause_reports_it_without_replaying(self):
        self.claim_wins()

    def test_early_positive_receipt_does_not_hide_a_still_running_native_call(self):
        self.claim_wins(early_receipt=True)
