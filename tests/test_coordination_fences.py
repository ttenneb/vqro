"""Uncached refusal IDs cannot invalidate an unrelated owner's fetched epoch."""
from unittest.mock import patch
import unittest
import coordination_engine as engine
from coordination_policy import PolicyError
import test_coordination_engine as fixtures


class RefusalFenceTests(unittest.TestCase):
    def setUp(self):
        self.f = fixtures.EngineTests(); self.f.setUp()
        self.caller = {'principal': 'p:denied', 'actor': 'p:denied'}
        original = self.f.engine._preflight_reservations
        def ceiling(*args, **kwargs):
            if kwargs.get('refusal'):
                raise engine.CoordinationError('quota', 'injected full refusal ceiling')
            return original(*args, **kwargs)
        self.patch = patch.object(self.f.engine, '_preflight_reservations', side_effect=ceiling)
        self.patch.start(); self.addCleanup(self.patch.stop)

    def refuse(self):
        op = self.f.op()
        args = {'op': op, 'submission': op, 'to': 'r1', 'mode': 'start',
                'content': [{'type': 'text', 'text': 'not admitted'}]}
        with self.assertRaises(PolicyError) as error:
            self.f.engine.send(args, self.caller)
        self.assertEqual(error.exception.kind, 'no_grant')
        self.assertTrue(error.exception.data['uncached'])
        return args, error.exception.data

    def test_flood_cannot_invalidate_an_owners_already_fetched_epoch(self):
        f = self.f
        pause = {'op': f.op(), 'action': 'pause', 'scope': 'workspace',
                 'expect': f.engine.scope_preview('workspace')['expect']}
        before = f.engine.window()
        for _ in range(30):
            _, data = self.refuse()
            self.assertTrue(data['fenced'])
            self.assertEqual(data['window']['epoch'], before['epoch'])
        self.assertEqual(f.engine.window()['used'], before['used'] + 30)
        self.assertEqual(f.engine.scope(pause, fixtures.OWNER)['selected'], 1)
        self.assertEqual(f.ledger, [])

    def test_combined_cached_decisions_and_fences_fill_one_bounded_window(self):
        with patch.object(engine, 'WINDOW', 4):
            old = self.f.engine.window()['epoch']
            a, _ = self.refuse(); b, _ = self.refuse()
            self.assertEqual(self.f.engine.window()['used'], 2)
            self.f.resume(action='pause')
            self.assertEqual(self.f.engine.window()['used'], 3)
            self.f.resume()
            self.assertNotEqual(self.f.engine.window()['epoch'], old)
            self.assertEqual(self.f.engine.window()['used'], 0)
            for args in (a, b):
                with self.assertRaises(PolicyError) as expired:
                    self.f.engine.send(args, self.caller)
                self.assertEqual(expired.exception.kind, 'expired_operation')

    def test_fence_only_overflow_rotates_and_never_recreates_old_ids(self):
        with patch.object(engine, 'WINDOW', 3):
            old = self.f.engine.window()['epoch']
            a, _ = self.refuse(); self.refuse()
            self.assertEqual(self.f.engine.window()['epoch'], old)
            self.refuse()
            self.assertNotEqual(self.f.engine.window()['epoch'], old)
            self.assertEqual(self.f.engine.window()['used'], 0)
            with self.assertRaises(PolicyError) as expired:
                self.f.engine.send(a, self.caller)
            self.assertEqual(expired.exception.kind, 'expired_operation')

    def test_window_close_keeps_maximum_handover_within_atomic_batch(self):
        f = self.f
        f.config['doc/agents/grants/fill'] = {'version': 1, 'value': {
            'who': '*', 'on': 'agent:r1', 'can': ['send:start']}}
        deliveries = []
        for n in range(58):
            op = f.op(); principal = 'p:fill' + str(n // 8)
            deliveries.append(f.engine.send({'op': op, 'submission': op, 'to': 'r1', 'mode': 'start',
                'send_new': True, 'content': [{'type': 'text', 'text': str(n)}]},
                {'principal': principal, 'actor': principal})['delivery'])
        f.resources['r1']['data']['agent']['lifecycle'] = 'ended'
        f.resources['r2'] = fixtures.resource('r2')
        plan = f.engine.handover({'op': f.op(), 'from': {'agent': 'r1', 'incarnation': 'i1'},
            'to': {'agent': 'r2', 'incarnation': 'i1'}, 'deliveries': deliveries}, fixtures.OWNER)['plan']
        old = f.engine.window()
        with patch.object(engine, 'WINDOW', old['used'] + 1):
            reply = f.engine.handover({'op': f.op(), 'action': 'complete', 'plan': plan}, fixtures.OWNER)
        self.assertEqual(reply['state'], 'completed')
        self.assertNotEqual(f.engine.window()['epoch'], old['epoch'])
        self.assertTrue(all(len(write['changes']) <= 64 for write in f.store.writes))
        self.assertIsNone(f.engine.maintenance_error)
        self.assertEqual(f.ledger, [])

    def test_boot_rotates_before_admission_but_retains_cached_history(self):
        f = self.f
        args, _ = self.refuse()
        accepted_op = f.op()
        pause = {'op': accepted_op, 'action': 'pause', 'scope': 'workspace',
                 'expect': f.engine.scope_preview('workspace')['expect']}
        f.engine.scope(pause, fixtures.OWNER)
        old = f.engine.window()['epoch']
        f.engine.fence(); f.engine = f.make_engine()
        self.assertNotEqual(f.engine.window()['epoch'], old)
        historical = f.engine.scope(pause, fixtures.OWNER)
        self.assertTrue(historical['historical'])
        self.assertEqual(f.engine.permits, set())
        f.config['doc/agents/grants/new'] = {'version': 1, 'value': {
            'who': self.caller['principal'], 'on': 'agent:r1', 'can': ['send:start']}}
        with self.assertRaises(PolicyError) as expired:
            f.engine.send(args, self.caller)
        self.assertEqual(expired.exception.kind, 'expired_operation')
        self.assertEqual(f.ledger, [])
