"""Partial-publication repro: real router + Agents, event order then sorted snapshot.

Adapted from lead-secondary/probe-route-partial-publication-core-order.py.
No core, provider effects, runtime or model; publications reflect the live-view seam.
"""
import copy
from types import SimpleNamespace
import unittest
import test_coordination_requests as routes
import test_coordination_service as services
from agents import Agents
from vqro_ext import VqroError


class RequestViewCleanup(unittest.TestCase):
    def fixture(self, *, clear_fails_once=False, write_then_raise=False):
        ext = services.FakeExtension()
        ext.resources.clear()
        ext.resources['r2'] = routes.fixtures.resource('r2')
        ext.resources['r2']['data']['agent']['principal'] = 'p:coordinator'
        ext.entries['doc/agents/members/r2'] = {'version': 1, 'value': {'project': None, 'reports_to': None}}
        for rid in ('r3', 'r10'):  # r10 arrives as an appended resource event
            resource = routes.fixtures.resource(rid)
            resource['data']['agent']['requests'] = [{'request': 'q1', 'revision': 1, 'scope': {'session': 's1'},
                'status': 'pending', 'method': 'confirm', 'title': 'Continue?', 'params': {}, 'incomplete': False}]
            ext.resources[rid] = resource
            ext.entries[routes.REQUEST_ROUTES + rid] = {'version': 1, 'value': {'route': 'coordinator'}}
            ext.entries['doc/agents/members/' + rid] = {'version': 1, 'value': {'project': None, 'reports_to': 'r2'}}
            ext.entries['doc/agents/grants/answer-' + rid] = {'version': 1, 'value': {
                'who': 'p:coordinator', 'on': 'agent:' + rid, 'can': ['answer']}}
        live, attempts = {}, []
        faults = {'publish': True, 'clear_once': clear_fails_once}
        def publish(name, value):
            attempts.append((name, value is None))
            if value is None:
                if name == 'coord-requests-r3' and faults['clear_once']:
                    faults['clear_once'] = False
                    raise VqroError('fixture clear failed', 'io')
                live.pop(name, None)
                return
            if faults['publish']:
                if write_then_raise and name == 'coord-requests-r3':
                    live[name] = copy.deepcopy(value)
                    raise VqroError('fixture uncertain publication', 'io')
                if name == 'coord-requests-r10':
                    raise VqroError('fixture publication quota', 'quota')
            live[name] = copy.deepcopy(value)
        ext.publish = publish
        service = services.CoordinationService(ext)
        pane = Agents.__new__(Agents)
        pane.ext = SimpleNamespace(live=live,
            value=lambda key: (ext.entries.get(key) or {}).get('value'),
            version=lambda key: (ext.entries.get(key) or {}).get('version', 0))
        return ext, service, pane, live, attempts, faults

    def test_partial_new_view_cleared_before_revocation_and_sorted_snapshot(self):
        ext, service, pane, live, attempts, faults = self.fixture()
        self.assertEqual(list(ext.resources), ['r2', 'r3', 'r10'])
        service.scan_requests()
        self.assertIsNotNone(service.request_error)
        del ext.entries['doc/agents/grants/answer-r3']
        ordered = dict(sorted(ext.resources.items()))
        ext.resources.clear(); ext.resources.update(ordered)
        self.assertEqual(list(ext.resources), ['r10', 'r2', 'r3'])
        service.scan_requests()  # failing r10 now comes before r3
        self.assertTrue(service.engine.request_route('r3')['human'])
        human, _ = pane.request_presentation(ext.resources['r3'], ext.resources['r3']['data']['agent']['requests'])
        self.assertTrue(human, 'stale partial projection must never hide the human Answer path')
        self.assertIn(('coord-requests-r3', True), attempts)
        self.assertNotIn('coord-requests-r3', live)
        before = len(service.engine.records())
        faults['publish'] = False
        service.scan_requests()
        self.assertIsNone(service.request_error)
        self.assertTrue(live['coord-requests-r3']['human'])
        self.assertEqual(live['coord-requests-r3']['fallback_reason'], 'coordinator_no_answer_right')
        self.assertFalse(live['coord-requests-r10']['human'])
        self.assertEqual(len(service.engine.records()), before)  # publication recovery never resubmits notices
        self.assertFalse(any(method == 'resource.invoke' for method, _ in ext.vqro.calls))

    def test_failed_clear_remains_tracked_for_next_scan(self):
        ext, service, _, live, _, _ = self.fixture(clear_fails_once=True)
        service.scan_requests()
        self.assertIn('r3', service.request_views)
        self.assertIn('coord-requests-r3', live)
        ordered = dict(sorted(ext.resources.items()))
        ext.resources.clear(); ext.resources.update(ordered)
        service.scan_requests()
        self.assertNotIn('coord-requests-r3', live)
        self.assertNotIn('r3', service.request_views)

    def test_track_before_publication_that_writes_then_raises(self):
        _, service, _, live, attempts, _ = self.fixture(write_then_raise=True)
        service.scan_requests()
        self.assertIn(('coord-requests-r3', True), attempts)
        self.assertNotIn('coord-requests-r3', live)
        self.assertEqual(service.request_views, set())


if __name__ == '__main__':
    unittest.main()
