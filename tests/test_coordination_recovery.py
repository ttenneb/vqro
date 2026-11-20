"""Invalid forest config blocks team expansion, not independent coordination."""
import copy
import unittest
import test_coordination_engine as fixtures
from coordination_policy import PolicyError


class InvalidMembershipRecovery(unittest.TestCase):
    def setUp(self):
        self.f = f = fixtures.EngineTests()
        f.setUp()
        f.resources['r2'] = fixtures.resource('r2')
        f.resources['r1']['data']['agent']['principal'] = 'p:a'
        for rid, parent in [('r1', 'r2'), ('r2', 'r1')]:
            f.config['doc/agents/members/' + rid] = {'version': 1, 'value': {'project': 'p', 'reports_to': parent}}
        for name, who, on in [('direct', 'p:direct', 'agent:r1'), ('project', 'p:project', 'project:p'),
                              ('wild', 'p:wild', '*'), ('team', 'p:team', 'team:r1'),
                              ('subject', 'team:r1', 'agent:r1')]:
            f.config['doc/agents/grants/' + name] = {'version': 1, 'value': {'who': who, 'on': on,
                'can': ['send:start', 'send:boundary', 'receive', 'pause', 'resume']}}

    @staticmethod
    def context(principal):
        return dict(fixtures.OWNER, principal=principal, actor=principal)

    def test_direct_project_and_wildcard_send_do_not_depend_on_forest(self):
        f = self.f
        original = copy.deepcopy(f.config)
        deliveries = []
        for principal in ('p:direct', 'p:project', 'p:wild'):
            op = f.op()
            sent = f.engine.send({'op': op, 'submission': op, 'to': 'r1', 'mode': 'start', 'send_new': True,
                                 'content': [{'type': 'text', 'text': principal}]}, self.context(principal))
            deliveries.append(sent['delivery'])
        f.resume(context=self.context('p:project'))
        for _ in deliveries:
            self.assertEqual(f.engine.dispatch_one()['state'], 'accepted')
        self.assertEqual(f.config, original, 'no quiet edge repair')
        self.assertEqual(len(f.ledger), 3)
        status = f.engine.configuration_status()
        self.assertFalse(status['membership_valid'])
        self.assertIn('cycle', status['membership_errors'][0]['message'])
        self.assertIn('r1', status['membership_errors'][0]['message'])

    def test_nonowner_scope_and_pull_work_while_team_expansion_refuses(self):
        f = self.f
        for scope, principal, expected in [('workspace', 'p:project', 2), ('project:p', 'p:project', 2),
                                           ('agent:r1', 'p:direct', 1)]:
            paused = f.resume(scope=scope, action='pause', context=self.context(principal))
            self.assertEqual(paused['selected'], expected)
            resumed = f.resume(scope=scope, context=self.context(principal))
            self.assertEqual(resumed['selected'], expected)
        for principal in ('p:team', 'p:a'):
            op = f.op()
            with self.assertRaises(PolicyError) as failure:
                f.engine.send({'op': op, 'submission': op, 'to': 'r1', 'mode': 'start',
                               'content': [{'type': 'text', 'text': 'blocked'}]}, self.context(principal))
            self.assertEqual(failure.exception.kind, 'membership_invalid')
            self.assertIn('cycle', str(failure.exception))
        with self.assertRaises(PolicyError) as failure:
            f.engine.scope_preview('team:r1')
        self.assertEqual(failure.exception.kind, 'membership_invalid')
        f.setup_pull()
        sent = f.send(mode='boundary')
        result = f.engine.take({'op': f.op(), 'agent': 'r1', 'consumer': 'c1', 'at': 'turn'}, self.context('p:project'))
        self.assertEqual([row['delivery'] for row in result['records']], [sent['delivery']])

    def test_invalid_member_shape_does_not_brick_workspace_or_exact_grants(self):
        f = self.f
        f.config['doc/agents/members/r2']['value'] = {'project': 'p', 'reports_to': 5}
        self.assertEqual(f.resume(action='pause', context=self.context('p:wild'))['selected'], 2)
        self.assertEqual(f.resume(context=self.context('p:wild'))['selected'], 2)
        self.assertFalse(f.engine.configuration_status()['membership_valid'])
        self.assertEqual(f.send()['state'], 'recorded')

    def test_list_point_declaration_has_explicit_invalid_diagnostic(self):
        f = self.f
        f.setup_pull()
        f.resources['r1']['data']['agent']['pull']['points'] = ['turn', 'idle']
        f.send(mode='boundary')
        with self.assertRaises(PolicyError) as failure:
            f.engine.take({'op': f.op(), 'agent': 'r1', 'consumer': 'c1', 'at': 'turn'}, fixtures.OWNER)
        self.assertEqual(failure.exception.kind, 'invalid')
        self.assertIn('declaration_invalid', str(failure.exception))


if __name__ == '__main__':
    unittest.main()
