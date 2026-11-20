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
    def legacy_freeze(self, f, deliveries, destination):
        """Seed a valid archive admitted by the pre-guard implementation."""
        archive = 'a' * 32
        f.store.apply([{'op': 'set', 'key': 'archive/' + archive, 'expect': 0, 'value': {
            'state': 'frozen', 'agents': ['r2'], 'principal': fixtures.OWNER['principal'],
            'destination': destination, 'created_ms': 1,
            'frozen': {'message/' + d: f.store.get('message/' + d)['version'] for d in deliveries}}}], control=True)
        return archive

    def test_archive_refuses_open_handover_before_freeze(self):
        f = fixtures.EngineTests(); f.setUp()
        deliveries = [f.send(send_new=True)['delivery'] for _ in range(2)]
        f.resources['r1']['data']['agent']['lifecycle'] = 'ended'
        f.resources['r2'] = fixtures.resource('r2')
        f.engine.handover({'op': f.op(), 'from': {'agent': 'r1', 'incarnation': 'i1'},
            'to': {'agent': 'r2', 'incarnation': 'i1'}, 'deliveries': deliveries}, fixtures.OWNER)
        with tempfile.TemporaryDirectory(prefix='coord-open-handover-') as destination:
            with self.assertRaises(PolicyError) as refused:
                f.engine.archive({'op': f.op(), 'agents': ['r2'], 'destination': destination}, fixtures.OWNER)
        self.assertEqual(refused.exception.kind, 'conflict')
        self.assertFalse(any(k.startswith('archive/') for k in f.store.snapshot()))
        self.assertEqual(f.ledger, [])

    def test_legacy_archived_members_do_not_strand_multi_record_handover(self):
        for action in ('complete', 'abort'):
            for count in (1, 2):
                with self.subTest(action=action, freed=count), tempfile.TemporaryDirectory(prefix='coord-legacy-handover-') as destination:
                    f = fixtures.EngineTests(); f.setUp()
                    deliveries = [f.send(send_new=True)['delivery'] for _ in range(2)]
                    f.resources['r1']['data']['agent']['lifecycle'] = 'ended'
                    f.resources['r2'] = fixtures.resource('r2')
                    plan = f.engine.handover({'op': f.op(), 'from': {'agent': 'r1', 'incarnation': 'i1'},
                        'to': {'agent': 'r2', 'incarnation': 'i1'}, 'deliveries': deliveries}, fixtures.OWNER)['plan']
                    archive = self.legacy_freeze(f, deliveries[:count], destination)
                    f.engine.archive({'op': f.op(), 'action': 'export', 'archive': archive}, fixtures.OWNER)
                    freed = f.engine.archive({'op': f.op(), 'action': 'free', 'archive': archive}, fixtures.OWNER)
                    self.assertEqual(len(freed['freed']), count)
                    reply = f.engine.handover({'op': f.op(), 'action': action, 'plan': plan}, fixtures.OWNER)
                    self.assertEqual(reply['state'], 'completed' if action == 'complete' else 'aborted')
                    self.assertEqual(reply['skipped'], deliveries[:count])
                    self.assertEqual(f.store.get('plan/' + plan)['value']['skipped'], deliveries[:count])
                    for delivery in deliveries[:count]:
                        self.assertEqual(f.engine.get(delivery)['state'], 'archived')
                    for delivery in deliveries[count:]:
                        remaining = f.engine.get(delivery)
                        self.assertNotIn('handover_plan', remaining)
                        self.assertEqual(remaining['to']['agent'], 'r2' if action == 'complete' else 'r1')
                    self.assertEqual(f.ledger, [])

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
                archive = (f.engine.archive({'op': f.op(), 'agents': [target], 'destination': destination}, fixtures.OWNER)['archive']
                           if action == 'create' else self.legacy_freeze(f, [delivery], destination))
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
