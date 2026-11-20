"""Frozen evidence is immutable to handover, not blind to genuine late receipts."""
from concurrent.futures import ThreadPoolExecutor
import json
from pathlib import Path
import tempfile
import threading
import unittest
from unittest.mock import patch
import test_coordination_engine as fixtures
from coordination_archive import export
from coordination_policy import PolicyError


class ArchiveRecoveryTests(unittest.TestCase):
    def test_handover_cannot_rewrite_frozen_records(self):
        for action in ('create', 'complete', 'abort'):
            with self.subTest(action=action), tempfile.TemporaryDirectory(prefix='coord-frozen-handover-') as destination:
                f = fixtures.EngineTests(); f.setUp()
                delivery = f.send()['delivery']
                f.resources['r1']['data']['agent']['lifecycle'] = 'ended'
                f.resources['r2'] = fixtures.resource('r2')
                args = {'from': {'agent': 'r1', 'incarnation': 'i1'},
                        'to': {'agent': 'r2', 'incarnation': 'i1'}, 'deliveries': [delivery]}
                target = 'r1'
                if action != 'create':
                    plan = f.engine.handover(dict(args, op=f.op()), fixtures.OWNER)['plan']
                    args = {'action': action, 'plan': plan}
                    target = 'r2'
                archive = f.engine.archive({'op': f.op(), 'agents': [target], 'destination': destination}, fixtures.OWNER)['archive']
                frozen = f.engine.get(delivery)
                with self.assertRaises(PolicyError) as refused:
                    f.engine.handover(dict(args, op=f.op()), fixtures.OWNER)
                self.assertEqual(refused.exception.kind, 'conflict')
                self.assertEqual(f.engine.get(delivery), frozen)
                self.assertEqual(f.engine.archive({'op': f.op(), 'action': 'export', 'archive': archive}, fixtures.OWNER)['state'], 'accepted')
                self.assertEqual(f.ledger, [])

    def test_late_receipt_survives_abort_replan_export_and_plan_slot_reuse(self):
        f = fixtures.EngineTests(); f.setUp()
        f.resources['r1']['data']['agent']['caps']['agent.send']['correlation'] = 'insertion'
        delivery = f.send()['delivery']
        f.resume(); f.engine.dispatch_one()
        native_count = len(f.ledger)
        with tempfile.TemporaryDirectory(prefix='coord-receipt-abort-') as destination:
            archive = f.engine.archive({'op': f.op(), 'agents': ['r1'], 'destination': destination}, fixtures.OWNER)['archive']
            for _ in range(3):
                f.engine.archive({'op': f.op(), 'agents': ['r1'], 'deliveries': [], 'destination': destination}, fixtures.OWNER)
            frozen = f.engine.get(delivery)
            self.assertTrue(f.engine.observe_receipts('r1', 'i1', [{'epoch': 'receipts', 'seq': 1,
                'receipt': {'delivery': delivery, 'stage': 'read'}}]))
            observed = f.engine.get(delivery)
            self.assertEqual(observed['state'], 'read')
            self.assertGreater(observed['revision'], frozen['revision'])
            self.assertEqual(f.engine.archive({'op': f.op(), 'action': 'export', 'archive': archive}, fixtures.OWNER)['state'], 'rejected')
            f.config['doc/agents/grants/recover'] = {'version': 1, 'value': {
                'who': 'p:worker', 'on': 'agent:r1', 'can': ['recover']}}
            with self.assertRaises(PolicyError) as refused:
                f.engine.archive({'op': f.op(), 'action': 'abort', 'archive': archive}, fixtures.WORKER)
            self.assertEqual(refused.exception.kind, 'no_grant')
            aborted = f.engine.archive({'op': f.op(), 'action': 'abort', 'archive': archive}, fixtures.OWNER)
            self.assertEqual(aborted['state'], 'aborted')
            audit = f.engine.archive({'action': 'inspect', 'archive': archive}, fixtures.OWNER)
            self.assertEqual(audit['abort']['principal'], fixtures.OWNER['principal'])
            self.assertFalse(f.engine._archive_frozen('message/' + delivery))
            new = f.engine.archive({'op': f.op(), 'agents': ['r1'], 'destination': destination}, fixtures.OWNER)['archive']
            f.engine.archive({'op': f.op(), 'action': 'export', 'archive': new}, fixtures.OWNER)
            plan = f.engine.archive({'action': 'inspect', 'archive': new}, fixtures.OWNER)
            archived = json.loads(Path(plan['locator']).read_text())
            self.assertEqual(archived['records'][0]['record']['result']['receipt']['stage'], 'read')
            freed = f.engine.archive({'op': f.op(), 'action': 'free', 'archive': new}, fixtures.OWNER)
            self.assertEqual(freed['state'], 'completed')
            self.assertEqual(freed['freed'], ['message/' + delivery])
            self.assertEqual(len(f.ledger), native_count)

    def test_export_finishing_after_abort_cannot_resurrect_plan(self):
        f = fixtures.EngineTests(); f.setUp()
        delivery = f.send()['delivery']
        entered, released = threading.Event(), threading.Event()
        def delayed(*args):
            entered.set()
            if not released.wait(5):
                raise AssertionError('owned export barrier was not released')
            return export(*args)
        with tempfile.TemporaryDirectory(prefix='coord-export-abort-') as destination:
            archive = f.engine.archive({'op': f.op(), 'agents': ['r1'], 'destination': destination}, fixtures.OWNER)['archive']
            with patch('coordination_engine.export_archive', side_effect=delayed), ThreadPoolExecutor(max_workers=1) as pool:
                job = pool.submit(f.engine.archive, {'op': f.op(), 'action': 'export', 'archive': archive}, fixtures.OWNER)
                self.assertTrue(entered.wait(3))
                try:
                    f.engine.archive({'op': f.op(), 'action': 'abort', 'archive': archive}, fixtures.OWNER)
                finally:
                    released.set()
                reply = job.result(5)
            self.assertEqual(reply['state'], 'observed')
            self.assertFalse(reply['coordination']['data']['published'])
            self.assertTrue(Path(reply['coordination']['data']['export']['locator']).is_file())
            self.assertEqual(f.engine.archive({'action': 'inspect', 'archive': archive}, fixtures.OWNER)['state'], 'aborted')
            self.assertFalse(f.engine._archive_frozen('message/' + delivery))
            with self.assertRaises(PolicyError):
                f.engine.archive({'op': f.op(), 'action': 'free', 'archive': archive}, fixtures.OWNER)
            self.assertEqual(f.ledger, [])
