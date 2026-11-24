"""Shared cutover dispatch with fake core/native callbacks, never a runtime."""
import copy
import unittest
from unittest.mock import patch
from test_account_cutover_claims import Core as JournalCore
from test_account_capacity import resource, NOW
from accounts_cutover import Cutovers
from account_credentials import SETTINGS
from accounts_dispatch import Dispatch
import accounts_policy as policy
from vqro_ext import VqroError


class Core(JournalCore):
    def __init__(self):
        super().__init__()
        self.entries[policy.POLICY] = {'version': 1, 'value': {'blocked': 'auto_restart'}}
        self.a = {'id': 'a', 'label': 'A', 'runtime': 'codex', 'kind': 'subscription',
                  'login_ref': {'sessionId': 'saved-a', 'accountId': 'native-a'}}
        self.b = dict(self.a, id='b', label='B', login_ref={'sessionId':'saved-b','accountId':'native-b'})
        self.entries[SETTINGS] = {'version':7,'value':{'accounts':[self.a,self.b]}}
        a = resource('r1', self.a, 100, blocked=True)
        a['host'] = 'terminal-1'
        a['data']['agent'].update(session={'id': 'session-1'}, state='idle')
        a['data']['agent']['availability']['native'] = {'turnId': 'native-turn-1'}
        b = resource('capacity-b', self.b, 10)
        self.resources = {'r1': a, 'capacity-b': b}
        self.fail_prepare = False
        self.result_outcome = 'observed'
        self.send_unknown = False
        self.effect_count = 0
        self.sends = []

    def invoke(self, name, args=None):
        if name == 'accounts.prepare.codex':
            if self.fail_prepare: raise VqroError('busy sibling', 'busy')
            row = self.resources['r1']; native = policy.facts(row)
            return {'setup_id': 'shared', 'membership_revision': 'members-1', 'automatic_allowed': True,
                    'native_episode': 'native-turn-1', 'affected': [{'resource': 'r1', 'host': row['host'],
                    'session': native['session']['id'], 'epoch': native['epoch'], 'incarnation': row['incarnation'], 'account': 'b'}]}
        if name == 'accounts.cutover.codex':
            self.effect_count += 1
            self.assert_effect_args = copy.deepcopy(args)
            native = self.resources['r1']['data']['agent']
            native.update(epoch='new-epoch', availability={'state': 'available'}, account={'id': 'b'},
                          caps={'rev': 2, 'agent.send': {'modes': {'admit': {}}, 'origin': [{'as': 'attributed_user'}]}})
            return {'outcome': self.result_outcome, 'account': 'b', 'affected': [{'resource': 'r1', 'host': 'terminal-1',
                'session': 'session-1', 'epoch': 'new-epoch', 'account': 'b', 'session_confirmed': True, 'resumed': True}]}
        return super().invoke(name, args)

    def call(self, method, params=None):
        if method == 'resource.invoke':
            self.sends.append(copy.deepcopy(params))
            if self.send_unknown: raise VqroError('uncertain send', 'indeterminate')
            return {'outcome': 'accepted'}
        return super().call(method, params)


class Owner:
    def __init__(self, core):
        self.ext = core
        self.cutovers = Cutovers(core)
    def continuation_offer(self,claim,target):self.offered=claim
    def source(self, rid): return copy.deepcopy(self.ext.resources[rid])
    def alternatives(self, source, configured):
        observed = list(self.ext.resources.values())
        return [self.ext.b], 7, policy.capacities(observed, [self.ext.b]), observed


class CutoverDispatch(unittest.TestCase):
    def setUp(self):
        self.core = Core(); self.owner = Owner(self.core); self.dispatch = Dispatch(self.owner)
        self.clock = patch('accounts_policy.now_ms', return_value=NOW)
        self.clock.start(); self.addCleanup(self.clock.stop)
    def execute(self, automatic=True):
        return self.dispatch.execute(self.owner.source('r1'), self.core.b, 7, automatic=automatic)
    def test_hourly_cap_four_still_builds_at_most_three_authorized_targets(self):
        self.core.entries[policy.POLICY]['value']['auto_cutover_max_per_hour']=4
        choices=[self.core.b]+[dict(self.core.b,id=name,label=name,
            login_ref={'sessionId':'saved-'+name,'accountId':'native-'+name}) for name in ('c','d','e')]
        self.core.entries[SETTINGS]['value']['accounts']=[self.core.a,*choices]
        self.owner.alternatives=lambda source,configured:(choices,7,{},list(self.core.resources.values()))
        original=self.core.invoke
        def invoke(name,args=None):
            if name=='accounts.cutover.codex':raise VqroError('synthetic unresolved callback','indeterminate')
            return original(name,args)
        self.core.invoke=invoke
        with self.assertRaises(VqroError):self.execute()
        record=next(iter(self.owner.cutovers.read()[0]['claims'].values()))
        self.assertEqual(record['authorized_targets'],['b','c','d'])
        self.assertEqual(self.owner.cutovers.remaining('codex',record['affected']),1)

    def test_auto_switches_same_session_then_sends_exactly_one_new_tool_turn(self):
        self.core.resources['r1']['data']['agent']['old_input'] = 'NEVER REPLAY THIS'
        self.execute()
        self.assertEqual(self.core.effect_count, 1)
        self.assertEqual(self.core.assert_effect_args['episode'], 'native-turn-1')
        self.assertEqual(len(self.core.sends), 1)
        send = self.core.sends[0]
        self.assertEqual(send['op'], 'agent.send')
        self.assertEqual(send['args']['mode'], 'admit')
        self.assertNotIn('run', send['args']['guard'])
        self.assertEqual(send['args']['origin']['kind'], 'tool')
        self.assertEqual(send['args']['content'], [{'type': 'text', 'text': policy.CONTINUATION_TEXT}])
        self.assertEqual(send['args']['guard'], {'session': 'session-1'})
    def test_hourly_zero_changed_during_prepare_prevents_any_native_effect(self):
        invoke = self.core.invoke
        calls = [0]
        def changed(name, args=None):
            result = invoke(name, args)
            if name == 'accounts.prepare.codex':
                calls[0] += 1
                if calls[0] == 2:
                    self.core.entries[policy.POLICY]['value']['auto_cutover_max_per_hour'] = 0
            return result
        self.core.invoke = changed
        with self.assertRaises(VqroError): self.execute()
        self.assertEqual(self.core.effect_count, 0)
        self.assertEqual(self.core.sends, [])

    def test_unknown_alias_capacity_remains_unknown_but_human_allows_auto(self):
        alias = dict(self.core.b, id='alias-b')
        self.core.entries[SETTINGS]['value']['accounts'].append(alias)
        self.core.resources['unknown-alias'] = resource('unknown-alias', alias)
        self.execute()
        self.assertEqual(self.core.effect_count, 1)
        self.assertEqual(len(self.core.sends), 1)

    def test_group_move_does_not_prompt_sibling_without_its_own_episode_claim(self):
        sibling=copy.deepcopy(self.core.resources['r1']);sibling['id']='r2'
        sibling['data']['agent']['session']={'id':'session-2'}
        sibling['data']['agent']['availability']={'state':'available'}
        self.core.resources['r2']=sibling
        original=self.core.invoke
        def invoke(name,args=None):
            result=original(name,args)
            if name=='accounts.prepare.codex':
                item=copy.deepcopy(result['affected'][0]);item.update(resource='r2',session='session-2')
                result['affected'].append(item)
            if name=='accounts.cutover.codex':
                sibling['data']['agent'].update(epoch='new-epoch-2',account={'id':'b'},caps=self.core.resources['r1']['data']['agent']['caps'])
                item=copy.deepcopy(result['affected'][0]);item.update(resource='r2',session='session-2',epoch='new-epoch-2')
                result['affected'].append(item)
            return result
        self.core.invoke=invoke;self.execute()
        self.assertEqual(len(self.core.sends),1)
        self.assertEqual(self.core.sends[0]['args']['guard'],{'session':'session-1'})
        self.assertEqual(self.owner.cutovers.remaining('codex',[{'session':'session-2'}]),2)

    def test_two_limited_agents_get_own_claims_but_only_one_group_switch(self):
        sibling=copy.deepcopy(self.core.resources['r1']);sibling['id']='r2'
        sibling['data']['agent']['session']={'id':'session-2'}
        sibling['data']['agent']['availability']['native']['turnId']='native-turn-2'
        self.core.resources['r2']=sibling
        original=self.core.invoke
        def invoke(name,args=None):
            result=original(name,args)
            if name=='accounts.prepare.codex':
                item=copy.deepcopy(result['affected'][0]);item.update(resource='r2',session='session-2')
                result['affected'].append(item)
            if name=='accounts.cutover.codex':
                sibling['data']['agent'].update(epoch='new-epoch-2',account={'id':'b'},availability={'state':'available'},
                                                caps=self.core.resources['r1']['data']['agent']['caps'])
                item=copy.deepcopy(result['affected'][0]);item.update(resource='r2',session='session-2',epoch='new-epoch-2')
                result['affected'].append(item)
            return result
        self.core.invoke=invoke;self.execute()
        self.assertEqual(self.core.effect_count,1);self.assertEqual(len(self.core.sends),1)
        value,_=self.owner.cutovers.read();self.assertEqual(len(value['claims']),2)
        child=self.owner.offered;record=value['claims'][child]
        self.assertEqual(record['source_account'],'a');self.assertEqual(record['source_epoch'],'e1')
        self.assertEqual(record['native_episode'],'native-turn-2')
        self.assertEqual(record['attempted_targets'],[])
        self.dispatch.confirm_continuation(child)
        self.assertEqual(len(self.core.sends),2);self.assertEqual(self.core.effect_count,1)
        self.assertEqual(self.core.sends[-1]['args']['guard'],{'session':'session-2'})
        self.assertEqual(self.owner.cutovers.remaining('codex',[{'session':'session-2'}]),2)
        with self.assertRaises(VqroError):self.dispatch.confirm_continuation(child)

    def nonready(self,disposition):
        original=self.core.invoke
        def invoke(name,args=None):
            result=original(name,args)
            if name=='accounts.cutover.codex':
                result.update(disposition=disposition,continuation_allowed=False,fenced=False,attempted_targets=['b'],
                              inspected_blocked=[] if disposition=='offer' else [{'account':'b','observed_at_ms':1234,
                              'reset_at_ms':None,'source':'codex.account/rateLimits'}])
            return result
        self.core.invoke=invoke

    def test_unknown_usage_restores_releases_then_owner_sends_once_same_claim(self):
        self.nonready('offer');self.execute()
        self.assertEqual(self.core.effect_count,1);self.assertEqual(self.core.sends,[])
        token=self.owner.offered;value,_=self.owner.cutovers.read()
        self.assertEqual(value['groups'],{});self.assertEqual(value['claims'][token]['state'],'awaiting_owner')
        self.dispatch.confirm_continuation(token)
        self.assertEqual(len(self.core.sends),1);self.assertEqual(self.core.effect_count,1)
        self.assertEqual(len(self.owner.cutovers.read()[0]['claims']),1)
        self.assertEqual(self.core.sends[0]['args']['guard'],{'session':'session-1'})
        with self.assertRaises(VqroError):self.dispatch.confirm_continuation(token)
        self.assertEqual(len(self.core.sends),1)

    def test_mixed_final_quota_preserves_negative_evidence_and_same_claim_owner_offer(self):
        self.nonready('offer');original=self.core.invoke
        def invoke(name,args=None):
            result=original(name,args)
            if name=='accounts.cutover.codex':
                result['inspected_blocked']=[{'account':'b','observed_at_ms':1234,'reset_at_ms':None,
                                             'source':'codex.account/rateLimits'}]
            return result
        self.core.invoke=invoke;self.execute()
        self.assertIsNotNone(self.owner.offered)
        token=self.owner.offered;value,_=self.owner.cutovers.read()
        self.assertEqual(value['groups'],{});self.assertEqual(value['claims'][token]['state'],'awaiting_owner')
        self.assertTrue(self.owner.cutovers.known_blocked(self.core.b))
        self.assertEqual(self.core.sends,[]);self.assertEqual(self.core.effect_count,1)
        self.dispatch.confirm_continuation(token)
        self.assertEqual(len(self.core.sends),1);self.assertEqual(self.core.effect_count,1)
        self.assertTrue(self.owner.cutovers.known_blocked(self.core.b))

    def test_all_blocked_restores_releases_notifies_and_never_auto_continues(self):
        self.nonready('all_blocked');self.execute()
        value,_=self.owner.cutovers.read();self.assertEqual(value['groups'],{})
        self.assertEqual(next(iter(value['claims'].values()))['state'],'completed')
        self.assertEqual(self.core.sends,[]);self.assertTrue(self.owner.cutovers.known_blocked(self.core.b))
        self.assertIn('All accounts are at their limit',str(self.core.notices))

    def test_consent_revoked_before_any_switch_restores_original_account_without_turn(self):
        original=self.core.invoke
        def invoke(name,args=None):
            result=original(name,args)
            if name=='accounts.cutover.codex':
                self.core.resources['r1']['data']['agent']['account']={'id':'a'}
                result.update(restored_only=True,continuation_allowed=False,fenced=False,attempted_targets=[],account=None)
                result['affected'][0]['account']='a'
            return result
        self.core.invoke=invoke;self.execute()
        value,_=self.owner.cutovers.read();self.assertEqual(value['groups'],{})
        record=next(iter(value['claims'].values()))
        self.assertEqual(record['state'],'completed');self.assertEqual(record['attempted_targets'],[])
        self.assertEqual(self.core.sends,[])

    def test_owner_offer_cannot_be_spoofed_or_replayed_after_unknown_send(self):
        self.nonready('offer');self.execute();token=self.owner.offered
        self.core.caller_record={'principal':'agent','initiator':'owner'}
        with self.assertRaises(VqroError):self.dispatch.confirm_continuation(token)
        self.assertEqual(self.core.sends,[])
        self.core.caller_record={'principal':'owner'};self.core.send_unknown=True
        with self.assertRaises(VqroError):self.dispatch.confirm_continuation(token)
        with self.assertRaises(VqroError):self.dispatch.confirm_continuation(token)
        self.assertEqual(len(self.core.sends),1);self.assertEqual(self.core.effect_count,1)

    def test_readonly_busy_prepare_consumes_no_intent(self):
        self.core.fail_prepare = True
        with self.assertRaises(VqroError): self.execute()
        self.assertIsNone(self.core.op_entry)
        self.assertEqual(self.core.effect_count, 0)
        self.assertEqual(self.core.sends, [])
    def test_callback_revoking_auto_after_switch_causes_zero_new_turns(self):
        original=self.core.invoke
        def revoke(name,args=None):
            result=original(name,args)
            if name=='accounts.cutover.codex':self.core.entries[policy.POLICY]['value']['blocked']='notify'
            return result
        self.core.invoke=revoke
        self.execute()
        self.assertEqual(self.core.effect_count,1)
        self.assertEqual(self.core.sends,[])
        saved=next(iter(self.core.op_entry['value']['claims'].values()))
        self.assertEqual(saved['state'],'completed')
        self.assertTrue(saved['continuation_cancelled'])

    def test_policy_revoked_after_child_intent_never_submits_that_turn(self):
        original=self.owner.cutovers.write
        def revoke(value,revision):
            result=original(value,revision)
            if any(record['continuations'] for record in value['claims'].values()):
                self.core.entries[policy.POLICY]['value']['blocked']='notify'
            return result
        self.owner.cutovers.write=revoke
        self.execute()
        self.assertEqual(self.core.sends,[])
        saved=next(iter(self.core.op_entry['value']['claims'].values()))
        self.assertEqual(saved['continuations']['session-1']['state'],'cancelled')

    def test_unknown_group_result_never_sends_any_continuation(self):
        self.core.result_outcome = 'accepted'
        self.execute()
        self.assertEqual(self.core.sends, [])
        self.assertEqual(next(iter(self.core.op_entry['value']['claims'].values()))['state'], 'unknown')
    def test_notify_has_no_native_effect_even_for_an_owner_click(self):
        self.core.entries[policy.POLICY]['value']['blocked'] = 'notify'
        with self.assertRaises(VqroError): self.execute(automatic=False)
        self.assertEqual(self.core.effect_count, 0)
        self.assertEqual(self.core.sends, [])
        self.assertIsNone(self.core.op_entry)
    def test_offer_requires_owner_and_never_sends_automatic_continuation(self):
        self.core.entries[policy.POLICY]['value']['blocked'] = 'offer_restart'
        self.core.caller_record = {'principal': 'agent', 'initiator': 'owner'}
        with self.assertRaises(VqroError): self.execute(automatic=False)
        self.assertEqual(self.core.effect_count, 0)
        self.core.caller_record = {'principal': 'owner'}
        self.execute(automatic=False)
        self.assertEqual(self.core.effect_count, 1)
        self.assertEqual(self.core.sends, [])
    def test_unknown_new_turn_submission_is_never_retried(self):
        self.core.send_unknown = True
        with self.assertRaises(VqroError): self.execute()
        self.assertEqual(len(self.core.sends), 1)
        saved = next(iter(self.core.op_entry['value']['claims'].values()))
        self.assertEqual(saved['continuations']['session-1']['state'], 'unknown')


if __name__ == '__main__': unittest.main()
