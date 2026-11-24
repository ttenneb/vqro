"""Protected cutover/continuation intents; fake core only, no native calls."""
import copy
from pathlib import Path
import sys
import unittest
sys.path[:0] = [str(Path(__file__).resolve().parents[1] / 'extensions'), str(Path(__file__).resolve().parent)]
from agent_fixture import FakeCore
from vqro_ext import VqroError
from accounts_cutover import Cutovers


class Core(FakeCore):
    def __init__(self):
        super().__init__('accounts')
        self.entries['doc/settings/accounts-policy'] = {'version':1,'value':{'blocked':'auto_restart'}}
        self.op_entry = None

    def call(self, method, params=None):
        params = params or {}
        if method == 'operational.register': return {}
        if method == 'operational.get': return {'entry': copy.deepcopy(self.op_entry)}
        if method == 'operational.apply':
            operation = params['ops'][0]
            version = self.op_entry['version'] if self.op_entry else 0
            if operation['expect'] != version: raise VqroError('conflict', 'conflict')
            self.op_entry = {'version': version + 1, 'value': copy.deepcopy(operation['value'])}
            return {'outcome': 'observed'}
        return super().call(method, params)


def member(resource='r1', session='session-1', host='terminal-1', account='b'):
    return {'resource': resource, 'host': host, 'session': session, 'epoch': 'epoch-1',
            'incarnation': 'incarnation-1', 'account': account}


def source():
    return {'id': 'r1', 'incarnation': 'incarnation-1', 'host': 'terminal-1',
            'data': {'agent': {'v': 0, 'epoch': 'epoch-1', 'session': {'id': 'session-1'}, 'account': {'id': 'a'}}}}


class CutoverClaims(unittest.TestCase):
    def fixture(self):
        core = Core()
        core.entries['doc/settings/accounts-policy']['value']['continuation_text'] = 'Continue new work.'
        target={'id':'b','runtime':'codex','label':'B','kind':'subscription',
                'login_ref':{'sessionId':'login-b','accountId':'account-b'}}
        original=dict(target,id='a',label='A',login_ref={'sessionId':'login-a','accountId':'account-a'})
        core.entries['doc/settings/accounts']={'version':7,'value':{'accounts':[original,target]}}
        return core, Cutovers(core), target

    def claim(self, journal, target, **extra):
        plan = {'setup_id': 'shared-setup', 'membership_revision': 'members-1', 'affected': [member()]}
        return journal.claim(source(), target, 7, plan, True, 'Continue new work.', **extra)

    def test_durable_claim_read_shape_binds_source_target_registry_group_membership(self):
        core, journal, target = self.fixture()
        token = self.claim(journal, target)
        saved = journal.lookup(token)
        self.assertEqual(saved['state'], 'claimed')
        self.assertEqual(saved['resource'], 'r1')
        self.assertEqual(saved['host'], 'terminal-1')
        self.assertEqual(saved['account'], 'b')
        self.assertEqual(saved['source_account'], 'a')
        self.assertEqual(saved['_account_revision'], 7)
        self.assertEqual(saved['setup_id'], 'shared-setup')
        self.assertEqual(saved['membership_revision'], 'members-1')
        self.assertEqual(saved['affected'], [member()])
        self.assertTrue(saved['automatic'])
        self.assertGreater(saved['version'], 0)
        self.assertIsNotNone(core.op_entry)

    def test_shared_setup_claim_serializes_different_sources_and_targets(self):
        _, journal, target = self.fixture()
        self.claim(journal, target)
        other = source(); other['id'] = 'r2'
        plan = {'setup_id': 'shared-setup', 'membership_revision': 'members-2',
                'affected': [member('r2', 'session-2')]}
        with self.assertRaises(VqroError): journal.claim(other, dict(target, id='c'), 8, plan, True, 'New turn.')

    def test_unknown_native_outcome_retains_group_and_never_authorizes_continuation(self):
        _, journal, target = self.fixture()
        token = self.claim(journal, target)
        journal.native_result(token, observed=False)
        with self.assertRaises(VqroError): journal.claim_continuation(token, member())
        with self.assertRaises(VqroError): self.claim(journal, target)

    def test_one_new_continuation_per_same_native_conversation_even_on_unknown_send(self):
        _, journal, target = self.fixture()
        token = self.claim(journal, target)
        journal.native_result(token, observed=True)
        delivery = journal.claim_continuation(token, member())
        self.assertIsInstance(delivery, str)
        journal.continuation_result(token, member(), observed=False)
        with self.assertRaises(VqroError): journal.claim_continuation(token, member('replacement-resource'))
        self.assertEqual(journal.lookup(token)['continuations']['session-1']['state'], 'unknown')

    def test_offer_never_claims_an_automatic_continuation(self):
        _, journal, target = self.fixture()
        plan = {'setup_id': 'shared-setup', 'membership_revision': 'members-1', 'affected': [member()]}
        token = journal.claim(source(), target, 7, plan, False, 'New turn.')
        journal.native_result(token, observed=True)
        with self.assertRaises(VqroError): journal.claim_continuation(token, member())

    def test_known_predispatch_rejection_releases_setup_but_does_not_replay_source(self):
        _, journal, target = self.fixture()
        token = self.claim(journal, target)
        journal.reject_before_effect(token)
        with self.assertRaises(VqroError): self.claim(journal, target)
        other = source(); other['id'] = 'r2'
        plan = {'setup_id': 'shared-setup', 'membership_revision': 'members-2',
                'affected': [member('r2')]}
        second = journal.claim(other, target, 7, plan, True, 'New turn.')
        self.assertEqual(journal.lookup(second)['state'], 'claimed')

    def test_distinct_native_blocked_turn_can_cut_over_again_without_epoch_change(self):
        _, journal, target = self.fixture()
        plan = {'setup_id': 'shared-setup', 'membership_revision': 'members-1',
                'affected': [member()], 'native_episode': 'turn-1'}
        token = journal.claim(source(), target, 7, plan, True, 'Continue new work.')
        journal.native_result(token, observed=True)
        journal.claim_continuation(token, member())
        journal.continuation_result(token, member(), observed=True)
        journal.complete(token)
        with self.assertRaises(VqroError): journal.claim(source(), target, 7, plan, True, 'New turn.')
        clone = source(); clone['id'] = 'second-view'
        with self.assertRaises(VqroError):
            journal.claim(clone, target, 7, dict(plan, affected=[member('second-view')]), True, 'New turn.')
        second = journal.claim(source(), target, 7, dict(plan, native_episode='turn-2'), True, 'New turn.')
        self.assertEqual(journal.lookup(second)['native_episode'], 'turn-2')

    def test_all_prepared_accounts_are_the_target_not_prior_native_identity(self):
        core, journal, target = self.fixture()
        plan = {'setup_id': 'shared-setup', 'membership_revision': 'members-1',
                'affected': [member(account='unrelated')]}
        with self.assertRaises(VqroError): journal.claim(source(), target, 7, plan, True, 'New turn.')
        self.assertIsNone(core.op_entry)

    def test_missing_group_fence_is_not_a_valid_live_claim(self):
        core, journal, target = self.fixture()
        self.claim(journal, target)
        core.op_entry['value']['groups'].clear()
        with self.assertRaises(VqroError): journal.read()

    def test_auto_cannot_complete_without_one_intent_for_each_native_conversation(self):
        _, journal, target = self.fixture()
        token = self.claim(journal, target)
        journal.native_result(token, observed=True)
        with self.assertRaises(VqroError): journal.complete(token)

    def test_unprepared_destination_cannot_claim_a_continuation(self):
        _, journal, target = self.fixture()
        token = self.claim(journal, target)
        journal.native_result(token, observed=True)
        with self.assertRaises(VqroError): journal.claim_continuation(token, member('unrelated-resource'))

    def test_no_data_agent_host_substitution_or_partial_membership_claim(self):
        core, journal, target = self.fixture()
        unbound = source(); del unbound['host']; unbound['data']['agent']['host'] = {'id': 'terminal-1'}
        plan = {'setup_id': 'shared-setup', 'membership_revision': 'members-1', 'affected': [member()]}
        with self.assertRaises(VqroError): journal.claim(unbound, target, 7, plan, True, 'New turn.')
        plan['affected'].append(dict(member(), session=''))
        with self.assertRaises(VqroError): journal.claim(source(), target, 7, plan, True, 'New turn.')
        self.assertIsNone(core.op_entry)


if __name__ == '__main__':
    unittest.main()
