"""Fake-only cutover callback tests: no keychain, Claude process or real core."""
import copy
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'extensions'))
from vqro_ext import VqroError
import accounts_policy as policy
from accounts_claude import ClaudeCutover, eligible_episode
from account_credentials import snapshot as registry_snapshot


class Fake:
    def __init__(self):
        self.handlers = {}
        self.vqro = self
        self.providers = {'runtime': {'role': 'runtime', 'host': 'r1', 'capabilities': {'claude.restart': {}}}}
        self.source = {'id': 'r2', 'kind': 'agent', 'incarnation': 'birth', 'provider': 'runtime',
                       'host': 'r1', 'available': True, 'data': {'agent': {
                           'v': 0, 'lifecycle': 'live', 'state': 'idle', 'queue': None, 'epoch': 'e1',
                           '_claude': {'input_ledger': {'v': 0, 'source': 'vqro-ledger', 'epoch': 'e1',
                                                       'complete': True, 'pending': 0}}, 'background': [],
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
                      'account': 'b', '_account_revision': 7, 'automatic': True,
                      'source_incarnation': 'birth', 'source_epoch': 'e1', 'source_account': 'a',
                      'session': 'native-session', 'native_episode': 'native-result-uuid',
                      'authorization': {'mode': 'auto', 'principal': None, 'owner_revision': None}}
        self.owner = {'version': 3, 'value': 'workspace-owner'}
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
            return copy.deepcopy(self.owner if params['key'] == 'doc/principals/owner' else self.registry)
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
        self.assertIs(params['args']['allow_background_loss'], False)
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

    def test_required_incomplete_or_malformed_markers_never_invoke_actual_callback(self):
        facts = self.ext.source['data']['agent']
        for markers in (['state'], ['_claude.input_ledger'], ['_claude.input_ledger.pending'],
                        ['background'], ['background[0]'], ['session'], ['session.id'],
                        ['availability.native'], ['availability.native.episode'], ['epoch'],
                        ['account.id'], ['caps.claude.restart'], None, {}, [None], ['']):
            facts['incomplete'] = markers
            self.refused()
        facts.pop('incomplete')

    def test_unrelated_incomplete_markers_do_not_disable_valid_episode(self):
        self.ext.source['data']['agent']['incomplete'] = ['history', 'items', 'session.title', 'queue']
        result = self.run_cutover()
        self.assertTrue(result['process_started'])

    def test_pure_eligibility_has_no_rpc_and_newer_work_clears_native_witness(self):
        row = copy.deepcopy(self.ext.source)
        self.assertEqual(eligible_episode(row), 'native-result-uuid')
        row['data']['agent']['state'] = 'running'
        self.assertIsNone(eligible_episode(row))
        # Producer contract invalidates the retained witness on a newer turn start/result.
        row['data']['agent']['availability'].pop('native')
        row['data']['agent']['state'] = 'idle'
        self.assertIsNone(eligible_episode(row))
        self.assertEqual(self.ext.calls, [])
        self.assertIsNone(eligible_episode(None))
        self.assertIsNone(eligible_episode({}))

    def test_idle_episode_qualifies_after_turn_scoped_blocked_is_cleared(self):
        availability = self.ext.source['data']['agent']['availability']
        availability['state'] = 'available'
        availability.pop('reason')
        result = self.run_cutover()
        self.assertTrue(result['process_started'])
        self.assertEqual(len(self.ext.native()), 1)

    def test_running_or_nonempty_unknown_own_ledger_never_dispatches_episode(self):
        facts = self.ext.source['data']['agent']
        facts['state'] = 'running'
        self.refused()
        facts['state'] = 'idle'
        ledger = facts['_claude']['input_ledger']
        for field, value in (('pending', 1), ('pending', None), ('pending', False),
                             ('complete', False), ('epoch', 'old'), ('source', 'native-queue')):
            original = copy.deepcopy(ledger)
            ledger[field] = value
            self.refused()
            ledger.clear()
            ledger.update(original)

    def test_auto_requires_complete_native_background_level_with_no_running_work(self):
        for background in (None, [{'id': 'running-task'}], {}, 'unknown'):
            self.ext.source['data']['agent']['background'] = background
            self.refused()

    def test_ledger_or_background_change_before_second_source_check_prevents_auto(self):
        for field in ('ledger', 'background'):
            original = copy.deepcopy(self.ext.source)
            self.ext.calls.clear()
            reads = []
            def changed(method, args):
                if method == 'resource.list':
                    reads.append(method)
                    if len(reads) == 2:
                        facts = self.ext.source['data']['agent']
                        if field == 'ledger':
                            facts['_claude']['input_ledger']['pending'] = 1
                        else:
                            facts['background'] = [{'id': 'running'}]
            self.ext.mutate = changed
            self.refused()
            self.assertEqual(len(reads), 2)
            self.ext.source = original
        self.ext.mutate = None

    def test_offer_cannot_discard_pending_or_unknown_vqro_input(self):
        self.args['automatic'] = False
        self.ext.claim['automatic'] = False
        self.ext.claim['authorization'] = {'mode': 'offer', 'principal': 'workspace-owner', 'owner_revision': 3,
                                           'background_loss_disclosed': True}
        for pending in (1, None):
            self.ext.source['data']['agent']['_claude']['input_ledger']['pending'] = pending
            self.refused()

    def test_offer_may_accept_unknown_background_only_with_protected_disclosure(self):
        self.args['automatic'] = False
        self.ext.claim['automatic'] = False
        self.ext.claim['authorization'] = {'mode': 'offer', 'principal': 'workspace-owner', 'owner_revision': 3,
                                           'background_loss_disclosed': True}
        self.ext.source['data']['agent']['background'] = None
        result = self.run_cutover()
        self.assertTrue(result['process_started'])
        self.assertIs(self.ext.native()[0][1]['args']['allow_background_loss'], True)

    def test_native_queue_stays_unknown_not_synthesized_from_own_ledger(self):
        self.assertIsNone(self.ext.source['data']['agent']['queue'])
        result = self.run_cutover()
        self.assertTrue(result['process_started'])
        self.assertIsNone(self.ext.source['data']['agent']['queue'])

    def test_new_running_turn_before_second_source_check_prevents_dispatch(self):
        reads = []
        def changed(method, args):
            if method == 'resource.list':
                reads.append(method)
                if len(reads) == 2:
                    self.ext.source['data']['agent']['state'] = 'running'
        self.ext.mutate = changed
        self.refused()
        self.assertEqual(len(reads), 2)

    def test_idle_without_native_episode_does_not_restart(self):
        self.ext.source['data']['agent']['availability'] = {'state': 'available'}
        self.refused()

    def test_availability_label_without_ended_native_subscription_proof_refuses(self):
        native = self.ext.source['data']['agent']['availability']['native']
        for field, value in (('subscription_limit', False), ('terminal_reason', 'auth_error'),
                             ('episode', ''), ('session_id', 'another-session')):
            original = copy.deepcopy(native)
            native[field] = value
            self.refused()
            native.clear()
            native.update(original)

    def test_l1_claim_cannot_authorize_l2_limit_in_same_source_epoch(self):
        self.ext.source['data']['agent']['availability']['native']['episode'] = 'native-result-L2'
        self.assertEqual(policy.episode(self.ext.source), self.args['episode'])
        self.refused()

    def test_claim_binds_native_episode_session_and_source_at_both_reads(self):
        for field in ('native_episode', 'session', 'source_incarnation', 'source_epoch', 'source_account'):
            original = copy.deepcopy(self.ext.claim)
            self.ext.claim.pop(field)
            self.refused()
            self.ext.claim = original
        reads = []
        invoke = self.ext.invoke
        def changed(command, args):
            reads.append(command)
            if len(reads) == 2:
                self.ext.claim['native_episode'] = 'native-result-L0'
            return invoke(command, args)
        self.ext.invoke = changed
        self.refused()
        self.assertEqual(len(reads), 2)

    def test_auto_authority_must_be_exact_and_notify_is_not_authority(self):
        for authorization in (None, {}, {'mode': 'notify', 'principal': None, 'owner_revision': None},
                              {'mode': 'auto', 'principal': 'someone', 'owner_revision': 3}):
            self.ext.claim['authorization'] = authorization
            self.refused()

    def test_offer_requires_current_authenticated_owner_bound_in_protected_claim(self):
        self.args['automatic'] = False
        self.ext.claim['automatic'] = False
        self.ext.claim['authorization'] = {'mode': 'offer', 'principal': 'workspace-owner', 'owner_revision': 3,
                                           'background_loss_disclosed': True}
        self.run_cutover()
        self.assertEqual(len(self.ext.native()), 1)
        self.assertIs(self.ext.native()[0][1]['args']['allow_background_loss'], True)
        self.ext.calls.clear()
        self.ext.owner['version'] = 4
        self.refused()

    def test_offer_needs_protected_pane_disclosure_before_background_loss_flag(self):
        self.args['automatic'] = False
        self.ext.claim['automatic'] = False
        authority = {'mode': 'offer', 'principal': 'workspace-owner', 'owner_revision': 3}
        for disclosed in (None, False, 'true'):
            self.ext.claim['authorization'] = dict(authority)
            if disclosed is not None:
                self.ext.claim['authorization']['background_loss_disclosed'] = disclosed
            self.refused()

    def test_offer_owner_change_before_second_claim_read_prevents_dispatch(self):
        self.args['automatic'] = False
        self.ext.claim['automatic'] = False
        self.ext.claim['authorization'] = {'mode': 'offer', 'principal': 'workspace-owner', 'owner_revision': 3,
                                           'background_loss_disclosed': True}
        reads = []
        def changed(method, args):
            if method == 'workspace.get' and args['key'] == 'doc/principals/owner':
                reads.append(method)
                if len(reads) == 2:
                    self.ext.owner['value'] = 'new-owner'
        self.ext.mutate = changed
        self.refused()
        self.assertEqual(len(reads), 2)

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

    def test_visible_conflicting_or_incomplete_route_never_invokes_actual_callback(self):
        original = copy.deepcopy(self.ext.target)
        for patch, remove in (({'native_provider': 'another-provider'}, None),
                              ({'login_ref': {'sessionId': 'saved', 'accountId': 'workspace'}}, None),
                              ({}, 'secret_ref'), ({}, 'host')):
            self.ext.target.clear()
            self.ext.target.update(original)
            self.ext.target.update(patch)
            if remove:
                self.ext.target.pop(remove)
            rows, _ = registry_snapshot(self.ext)
            self.assertEqual(rows[0], self.ext.target, 'safe metadata remains displayable')
            self.refused()

    def test_known_source_credential_alias_is_not_another_account(self):
        alias = dict(self.ext.target, id='a', label='Same token under another label')
        self.ext.registry['value']['accounts'].append(alias)
        self.refused()

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
