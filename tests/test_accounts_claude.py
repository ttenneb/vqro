"""Fake-only cutover callback tests: no keychain, Claude process or real core."""
import copy
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'extensions'))
from vqro_ext import VqroError
import accounts_policy as policy
from accounts_claude import ClaudeCutover


class Fake:
    def __init__(self):
        self.handlers = {}
        self.vqro = self
        self.providers = {'runtime': {'role': 'runtime', 'host': 'r1', 'capabilities': {'claude.restart': {}}}}
        self.source = {'id': 'r2', 'kind': 'agent', 'incarnation': 'birth', 'provider': 'runtime',
                       'host': 'r1', 'available': True, 'data': {'agent': {
                           'v': 0, 'lifecycle': 'live', 'state': 'idle', 'epoch': 'e1',
                           'session': {'id': 'native-session'}, 'account': {'id': 'a'},
                           'caps': {'claude.restart': {}, 'agent.availability': {'reasons': ['quota']}},
                           'availability': {'state': 'blocked', 'reason': 'quota', 'native': {
                               'subscription_limit': True, 'episode': 'native-result-uuid',
                               'session_id': 'native-session', 'terminal_reason': 'blocking_limit'}}}}}
        self.target = {'id': 'b', 'label': 'Account B', 'runtime': 'claude', 'provider': 'anthropic',
                       'kind': 'subscription', 'secret_ref': 'b' * 32, 'host': 'r1'}
        self.registry = {'version': 7, 'value': {'accounts': [self.target]}}
        self.episode = policy.episode(self.source)
        self.claim = {'state': 'claimed', 'resource': 'r2', 'episode': self.episode, 'host': 'r1',
                      'account': 'b', '_account_revision': 7, 'automatic': True}
        self.result = {'outcome': 'observed', 'effect': 'e7', 'data': {
            'started': True, 'requested_session': 'native-session', 'session': 'native-session', 'resume_confirmed': True,
            'account': 'b', 'epoch': 'e2'}}
        self.calls = []
        self.failure = None
        self.mutate = None

    def command(self, name, title):
        def register(fn):
            self.handlers[name] = fn
            return fn
        return register

    def fresh(self):
        pass

    def invoke(self, command, args):
        self.calls.append(('command.invoke', {'name': command, 'args': copy.deepcopy(args)}))
        if command != 'accounts.claim.read':
            raise AssertionError(command)
        return copy.deepcopy(self.claim)

    def call(self, method, params):
        self.calls.append((method, copy.deepcopy(params)))
        if self.mutate:
            self.mutate(method, params)
        if method == 'resource.list':
            return {'resources': [copy.deepcopy(self.source)]}
        if method == 'workspace.get':
            return copy.deepcopy(self.registry)
        if method == 'operational.get':
            return copy.deepcopy(self.claim)
        if method == 'resource.invoke':
            if self.failure:
                raise self.failure
            return copy.deepcopy(self.result)
        raise AssertionError(method)

    def native(self):
        return [(m, p) for m, p in self.calls if m == 'resource.invoke']


class CutoverTests(unittest.TestCase):
    def setUp(self):
        self.ext = Fake()
        self.cutover = ClaudeCutover(self.ext)
        self.args = {'resource': 'r2', 'episode': self.ext.episode, 'account': 'b',
                     '_account_revision': 7, 'automatic': True, '_claim': 'protected-claim'}

    def run_cutover(self):
        return self.ext.handlers['accounts.cutover.claude'](self.args)

    def refused(self):
        with self.assertRaises(VqroError):
            self.run_cutover()
        self.assertEqual(self.ext.native(), [])

    def test_exact_target_restart_only_no_claim_selection_or_continuation(self):
        result = self.run_cutover()
        method, params = self.ext.native()[0]
        self.assertEqual((method, params['resource'], params['op']), ('resource.invoke', 'r2', 'claude.restart'))
        self.assertEqual({k: params['args'][k] for k in ('epoch', 'session', 'account', 'episode')},
                         {'epoch': 'e1', 'session': 'native-session', 'account': 'a', 'episode': 'native-result-uuid'})
        self.assertEqual(params['args']['target'], {'account': 'b', 'ref': 'b' * 32, 'host': 'r1', 'revision': 7})
        self.assertEqual((result['session'], result['account'], result['epoch']), ('native-session', 'b', 'e2'))
        self.assertTrue(result['process_started'])
        self.assertTrue(result['session_confirmed'])
        self.assertEqual(len(self.ext.native()), 1)
        self.assertFalse(any(m in ('operational.apply', 'operational.register', 'resource.kill',
                                  'resource.spawn') for m, _ in self.ext.calls))
        self.assertEqual({p['name'] for m, p in self.ext.calls if m == 'command.invoke'}, {'accounts.claim.read'})

    def test_unconfirmed_session_is_not_invented_but_start_is_reported(self):
        self.ext.result['data']['resume_confirmed'] = False
        del self.ext.result['data']['session']
        result = self.run_cutover()
        self.assertTrue(result['process_started'])
        self.assertFalse(result['session_confirmed'])
        self.assertFalse(result['resumed'])
        self.assertNotIn('session', result)
        self.assertNotIn('session', result['affected'][0])
        self.assertEqual(result['requested_session'], 'native-session')
        self.assertIn('not confirmed', result['notice'])
        self.assertEqual(result['effect'], 'e7')

    def test_missing_process_start_proof_never_allows_normal_result(self):
        for proof in (None, False, 'true'):
            self.ext.result['data']['started'] = proof
            with self.assertRaises(VqroError) as caught:
                self.run_cutover()
            self.assertIn('not confirmed', str(caught.exception))
        self.assertEqual(len(self.ext.native()), 3)

    def test_wrong_native_session_account_or_unchanged_epoch_is_uncertain(self):
        for field, value in (('session', 'other'), ('account', 'c'), ('epoch', 'e1')):
            original = copy.deepcopy(self.ext.result)
            self.ext.result['data'][field] = value
            with self.assertRaises(VqroError):
                self.run_cutover()
            self.ext.result = original

    def test_rejected_and_indeterminate_errors_preserve_original_evidence(self):
        wire = {'code': -32000, 'data': {'effect': 'e8', 'outcome': 'indeterminate'}}
        self.ext.failure = VqroError('lost', 'unavailable', data=wire['data'], wire_error=wire)
        with self.assertRaises(VqroError) as caught:
            self.run_cutover()
        self.assertEqual(caught.exception.data, wire['data'])
        self.assertEqual(caught.exception.wire_error, wire)
        self.assertEqual(len(self.ext.native()), 1)

    def test_availability_label_without_ended_native_subscription_proof_refuses(self):
        native = self.ext.source['data']['agent']['availability']['native']
        for field, value in (('subscription_limit', False), ('terminal_reason', 'auth_error'),
                             ('episode', ''), ('session_id', 'another-session')):
            original = copy.deepcopy(native)
            native[field] = value
            self.refused()
            native.clear()
            native.update(original)

    def test_claim_binding_checks_host_revision_automatic_and_episode(self):
        for field, value in (('host', 'other'), ('_account_revision', 8), ('automatic', False),
                             ('episode', 'other'), ('state', 'unknown')):
            original = copy.deepcopy(self.ext.claim)
            self.ext.claim[field] = value
            self.refused()
            self.ext.claim = original

    def test_no_native_operation_without_matching_protected_common_claim(self):
        self.ext.claim = {}
        self.refused()

    def test_shared_registry_schema_rejects_extra_credential_material(self):
        self.ext.target['token'] = 'not-a-supported-registry-field'
        self.refused()

    def test_registry_revision_and_target_host_must_match(self):
        self.ext.registry['version'] = 8
        self.refused()
        self.ext.registry['version'] = 7
        self.ext.target['host'] = 'another-terminal'
        self.refused()

    def test_runtime_host_authentication_not_native_child_host(self):
        self.ext.providers['runtime']['role'] = 'backend'
        self.refused()
        self.ext.providers['runtime']['role'] = 'runtime'
        self.ext.providers['runtime']['host'] = 'different'
        self.refused()

    def test_quota_episode_source_account_session_and_capability_required(self):
        facts = self.ext.source['data']['agent']
        for change in ('epoch', 'session', 'account', 'caps'):
            original = copy.deepcopy(facts)
            facts.pop(change)
            self.refused()
            facts.clear()
            facts.update(original)

    def test_api_billing_and_nonopaque_refs_refused(self):
        self.ext.target['kind'] = 'api_key'
        self.refused()
        self.ext.target['kind'] = 'subscription'
        self.ext.target['secret_ref'] = 'raw-credential-not-allowed'
        self.refused()

    def test_claim_or_registry_change_on_final_read_prevents_dispatch(self):
        seen = []
        def mutate(method, params):
            if method == 'resource.list':
                seen.append(method)
                if len(seen) == 2:
                    self.ext.source['data']['agent']['epoch'] = 'newer'
        self.ext.mutate = mutate
        self.refused()
        self.assertEqual(len(seen), 2)


if __name__ == '__main__':
    unittest.main()
