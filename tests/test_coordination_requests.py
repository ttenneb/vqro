"""Request notices are bounded normal inbox records, never native answers/replay."""
import copy
import json
import tempfile
import unittest
import test_coordination_engine as fixtures
import test_coordination_service as services
from coordination_policy import PolicyError
from coordination_requests import REQUEST_ROUTES, inspect_route, request_key

ROUTER = {'principal': 'extension:coord', 'actor': 'extension:coord', 'kind': 'tool'}


def setup_routes(config, resources):
    resources['r2'] = fixtures.resource('r2')
    resources['r2']['data']['agent']['principal'] = 'p:coordinator'
    resources['r1']['data']['agent'].setdefault('epoch', 'e1')
    resources['r1']['data']['agent']['requests'] = [
        {'request': 'q1', 'revision': 1, 'scope': {'session': 's1'}, 'status': 'pending',
         'method': 'confirm', 'title': 'Continue?', 'params': {}, 'incomplete': False}]
    config[REQUEST_ROUTES + 'r1'] = {'version': 1, 'value': {'route': 'coordinator'}}
    config['doc/agents/members/r2'] = {'version': 1, 'value': {'project': None, 'reports_to': None}}
    config['doc/agents/members/r1'] = {'version': 1, 'value': {'project': None, 'reports_to': 'r2'}}
    config['doc/agents/grants/answer'] = {'version': 1, 'value': {'who': 'p:coordinator', 'on': 'agent:r1', 'can': ['answer']}}


class RequestRoutingTests(unittest.TestCase):
    def fixture(self):
        f = fixtures.EngineTests(); f.setUp()
        setup_routes(f.config, f.resources)
        f.engine.notice_principal = ROUTER['principal']
        return f

    def notice(self, f):
        key = request_key(f.resources['r1'], f.resources['r1']['data']['agent']['requests'][0])
        op = f.op()
        return f.engine.send({'op': op, 'submission': op, 'to': 'r2', 'mode': 'admit', 'send_new': True,
                              'content': [{'type': 'text', 'text': 'native request notice'}]}, ROUTER,
                             request_notice={'agent': 'r1', 'key': key})['delivery']

    def test_defaults_configure_cas_and_invalid_visible_fallback(self):
        f = self.fixture(); policy = f.engine.policy()
        self.assertEqual(inspect_route(policy, 'r1')['coordinator'], 'r2')
        with self.assertRaises(PolicyError) as denied:
            policy.request_route_ops('p:stranger', {'agent': 'r1', 'route': 'both', 'expect': 1})
        self.assertEqual(denied.exception.kind, 'no_grant')
        ops = policy.request_route_ops('p:owner', {'agent': 'r1', 'route': 'both', 'expect': 1})
        self.assertEqual(ops, [{'op': 'set', 'key': REQUEST_ROUTES + 'r1', 'expect': 1, 'value': {'route': 'both'}}])
        for raw in ({'route': []}, {'route': 'future'}, [], 3):
            f.config[REQUEST_ROUTES + 'r1']['value'] = raw
            route = inspect_route(f.engine.policy(), 'r1')
            self.assertTrue(route['human']); self.assertEqual(route['fallback_reason'], 'invalid_request_route')
        del f.config[REQUEST_ROUTES + 'r1']
        self.assertEqual(inspect_route(f.engine.policy(), 'r1')['route'], 'human')

    def test_initialize_uses_actual_owner_or_unique_coordinator_not_args(self):
        f = self.fixture(); del f.config[REQUEST_ROUTES + 'r1']
        owner = f.engine.policy().request_route_ops('p:owner',
                    {'action': 'initialize', 'agent': 'r1', 'route': 'both', 'expect': 0})
        self.assertEqual(owner[-1]['value'], {'route': 'human'})
        f.config['doc/agents/grants/answer']['value']['can'].append('configure')
        coordinator = f.engine.policy().request_route_ops('p:coordinator',
                    {'action': 'initialize', 'agent': 'r1', 'route': 'human', 'expect': 0})
        self.assertEqual(coordinator[-1]['value'], {'route': 'coordinator'})
        f.resources['r3'] = fixtures.resource('r3')
        f.resources['r3']['data']['agent']['principal'] = 'p:coordinator'
        ambiguous = f.engine.policy().request_route_ops('p:coordinator',
                    {'action': 'initialize', 'agent': 'r1', 'expect': 0})
        self.assertEqual(ambiguous[-1]['value'], {'route': 'human'})

    def test_initialize_seeds_unknown_child_membership_atomically_with_wildcard_grants(self):
        f = self.fixture(); del f.config[REQUEST_ROUTES + 'r1']; del f.config['doc/agents/members/r1']
        f.config['doc/agents/grants/launcher'] = {'version': 1, 'value': {'who': 'p:coordinator', 'on': '*',
                                                               'can': ['configure', 'membership', 'answer']}}
        # A bridge host is not a second logical coordinator endpoint.
        f.resources['r9'] = {'id': 'r9', 'kind': 'terminal', 'available': True, 'principal': 'p:coordinator',
                             'status': {'app': 'pi'}}
        ops = f.engine.policy().request_route_ops('p:coordinator', {'action': 'initialize', 'agent': 'r1', 'expect': 0})
        values = {op['key']: op['value'] for op in ops}
        self.assertEqual(values['doc/agents/members/r1'], {'project': None, 'reports_to': 'r2'})
        self.assertEqual(values[REQUEST_ROUTES + 'r1'], {'route': 'coordinator'})
        self.assertIn('doc/agents/membership-revision', values)
        self.assertTrue(all(op['expect'] == 0 for op in ops))

    def test_notice_admission_refusal_is_visible_human_fallback_not_answer(self):
        ext = services.FakeExtension(); setup_routes(ext.entries, ext.resources)
        publications = {}; ext.publish = lambda name, value: publications.__setitem__(name, copy.deepcopy(value))
        service = services.CoordinationService(ext)
        def quota(*_, **__):
            raise PolicyError('quota', 'test admission ceiling')
        service.engine.send = quota
        service.route_requests()
        self.assertTrue(publications['coord-requests-r1']['human'])
        self.assertEqual(publications['coord-requests-r1']['fallback_reason'], 'notification_quota')
        self.assertEqual(ext.resources['r1']['data']['agent']['requests'][0]['status'], 'pending')
        self.assertEqual(service.engine.records(), [])

    def test_narrow_notice_authority_without_general_send_and_revoke_before_claim(self):
        f = self.fixture()
        self.assertFalse(f.engine.policy().allowed(ROUTER['principal'], 'send:admit', 'r2'))
        delivery = self.notice(f)
        f.resume()
        del f.config['doc/agents/grants/answer']
        self.assertFalse(f.engine.dispatch_one())
        self.assertEqual(f.engine.get(delivery)['reason'], 'request_route_changed')
        self.assertEqual(f.ledger, [])
        self.assertEqual(inspect_route(f.engine.policy(), 'r1')['fallback_reason'], 'coordinator_no_answer_right')

    def test_pinned_notice_compacts_but_survives_gc_restart_and_unknown_source(self):
        f = self.fixture(); delivery = self.notice(f); f.resume(); self.assertTrue(f.engine.dispatch_one())
        self.assertTrue(f.engine.get(delivery)['incomplete'])
        self.assertIn('request_notice', f.engine.get(delivery))
        for _ in range(257):
            f.send(mode='admit', send_new=True); self.assertTrue(f.engine.dispatch_one())
        self.assertIsNotNone(f.store.get('message/' + delivery))
        f.resources['r1']['available'] = False
        f.engine.fence(); f.engine = f.make_engine(); f.engine.notice_principal = ROUTER['principal']
        f.engine.maintenance()
        self.assertIsNotNone(f.store.get('message/' + delivery))
        f.resources['r1']['available'] = True
        f.resources['r1']['data']['agent']['requests'] = []
        f.engine.maintenance()
        self.assertIsNone(f.store.get('message/' + delivery))
        self.assertEqual(len(f.ledger), 258)

    def test_archive_free_refuses_pending_notice_then_allows_positive_obsolete_source(self):
        f = self.fixture(); delivery = self.notice(f)
        with tempfile.TemporaryDirectory() as path:
            archive = f.engine.archive({'op': f.op(), 'action': 'freeze', 'deliveries': [delivery], 'agents': ['r2'], 'destination': path}, fixtures.OWNER)['archive']
            f.engine.archive({'op': f.op(), 'action': 'export', 'archive': archive}, fixtures.OWNER)
            with self.assertRaises(PolicyError) as pending:
                f.engine.archive({'op': f.op(), 'action': 'free', 'archive': archive}, fixtures.OWNER)
            self.assertEqual(pending.exception.kind, 'conflict')
            f.resources['r1']['data']['agent']['requests'] = []
            freed = f.engine.archive({'op': f.op(), 'action': 'free', 'archive': archive}, fixtures.OWNER)
            self.assertIn('message/' + delivery, freed['freed'])
            self.assertEqual(f.ledger, [])

    def test_service_notice_shape_projection_and_restart_dedup(self):
        ext = services.FakeExtension(); setup_routes(ext.entries, ext.resources)
        publications = {}; ext.publish = lambda name, value: publications.__setitem__(name, copy.deepcopy(value))
        service = services.CoordinationService(ext)
        service.route_requests(); service.route_requests()
        records = service.engine.records()
        self.assertEqual(len(records), 1)
        record = records[0]
        self.assertEqual(record['from']['principal'], ext.info['principal'])
        payload = json.loads(record['content'][0]['text'].split('\n', 1)[1])
        self.assertEqual((payload['agent'], payload['epoch'], payload['request'], payload['revision'], payload['scope']),
                         ('r1', 'e1', 'q1', 1, {'session': 's1'}))
        self.assertFalse(publications['coord-requests-r1']['human'])
        service.engine.fence()
        service = services.CoordinationService(ext); service.route_requests()
        self.assertEqual(len(service.engine.records()), 1)
        self.assertFalse(any(method == 'resource.invoke' for method, _ in ext.vqro.calls))
