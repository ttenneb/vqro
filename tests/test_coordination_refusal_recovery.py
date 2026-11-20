"""Refusal floods cannot spend the owner's recovery capacity or reopen old IDs."""
import json
import tempfile
import unittest
import test_coordination_engine as fixtures
from coordination_engine import Working
from coordination_policy import PolicyError
from coordination_store import encoded


class RefusalRecoveryTests(unittest.TestCase):
    FLOOD = 512  # Full reviewer2600-case retained in coordination_refusal_stress.py.

    def saturated(self):
        f = fixtures.EngineTests(); f.setUp()
        f.resources = {'r' + str(n): fixtures.resource('r' + str(n)) for n in range(1, 10)}
        for rid in f.resources:
            f.config['doc/agents/members/' + rid] = {'version': 1, 'value': {'project': 'fill'}}
        f.config['doc/agents/grants/fill'] = {'version': 1, 'value': {
            'who': '*', 'on': 'project:fill', 'can': ['send:start']}}
        f.resume(action='pause')
        for text in ('z' * 30000, 'y' * 2000, 'm'):
            streak = 0
            for n in range(2000):
                principal = 'p:fill' + str(n % 10)
                op = f.op()
                try:
                    f.engine.send({'op': op, 'submission': op, 'to': 'r' + str((n // 10) % 9 + 1),
                                   'mode': 'start', 'send_new': True, 'content': [{'type': 'text', 'text': text}]},
                                  {'principal': principal, 'actor': principal, 'author': 'client:fill'})
                    streak = 0
                except PolicyError as error:
                    self.assertIn(error.kind, ('quota', 'busy'))
                    if 'reserve' in str(error) or error.kind == 'busy':
                        streak += 1
                        if streak >= 12:
                            break
        ring = ['r' + str(n) for n in range(10, 74)]
        for n, rid in enumerate(ring):
            f.resources[rid] = fixtures.resource(rid)
            f.config['doc/agents/members/' + rid] = {'version': 1, 'value': {
                'project': 'ring', 'reports_to': ring[(n + 1) % len(ring)]}}
        return f, ring

    def test_ungranted_refusals_leave_workspace_recovery_and_fence_uncached_ids(self):
        f, ring = self.saturated()
        caller = {'principal': 'p:flood', 'actor': 'p:flood', 'author': 'client:flood'}
        uncached = None
        for n in range(self.FLOOD):
            op = f.op()
            args = {'op': op, 'submission': op, 'to': ring[n % len(ring)], 'mode': 'start',
                    'send_new': True, 'content': [{'type': 'text', 'text': 'b'}]}
            with self.assertRaises(PolicyError) as refusal:
                f.engine.send(args, caller)
            self.assertIn(refusal.exception.kind, ('no_grant', 'busy'))
            if refusal.exception.data.get('uncached'):
                self.assertEqual(refusal.exception.kind, 'no_grant')
                uncached = args
        self.assertEqual(f.resume(action='pause')['selected'], 73)
        self.assertEqual(f.resume()['selected'], 73)
        self.assertTrue(f.engine.current)
        self.assertIsNone(f.engine.maintenance_error)
        self.assertIsNotNone(uncached, 'saturation reports live uncached refusals, never busy forever')
        # Both rights and space change after the live refusal: its old ID still
        # cannot become an admission, but a new operation can use the new grant.
        f.config['doc/agents/grants/changed'] = {'version': 1, 'value': {
            'who': 'p:flood', 'on': 'agent:' + uncached['to'], 'can': ['send:start']}}
        # Pause allocated summaries for the64 newly discovered ring agents.
        # Free enough content for those summaries AND a real fresh admission.
        spacers = [row for row in f.engine.records() if len(row['content'][0]['text']) == 30000][:16]
        with tempfile.TemporaryDirectory(prefix='coord-refusal-archive-') as destination:
            archive = f.engine.archive({'op': f.op(), 'agents': sorted({row['to']['agent'] for row in spacers}),
                                        'deliveries': [row['delivery'] for row in spacers],
                                        'destination': destination}, fixtures.OWNER)['archive']
            f.engine.archive({'op': f.op(), 'action': 'export', 'archive': archive}, fixtures.OWNER)
            f.engine.archive({'op': f.op(), 'action': 'free', 'archive': archive}, fixtures.OWNER)
        with self.assertRaises(PolicyError) as expired:
            f.engine.send(uncached, caller)
        self.assertEqual(expired.exception.kind, 'expired_operation')
        new = f.op()
        fresh = dict(uncached, op=new, submission=new)
        self.assertEqual(f.engine.send(fresh, caller)['state'], 'recorded')
        self.assertEqual(f.ledger, [])

    def test_cached_refusal_keeps_kind_and_fingerprint_not_full_message(self):
        f = fixtures.EngineTests(); f.setUp()
        caller = {'principal': 'p:denied', 'actor': 'p:denied'}
        op = f.op()
        args = {'op': op, 'submission': op, 'to': 'r1', 'mode': 'start',
                'content': [{'type': 'text', 'text': 'denied'}]}
        with self.assertRaises(PolicyError) as live:
            f.engine.send(args, caller)
        saved = f.engine._cached(Working(f.engine), caller['principal'], op)
        self.assertLessEqual(encoded(saved), 256)
        self.assertNotIn(str(live.exception), json.dumps(saved))
        with self.assertRaises(PolicyError) as repeat:
            f.engine.send(args, caller)
        self.assertEqual(repeat.exception.kind, live.exception.kind)
        self.assertEqual(f.engine.records(), [])
