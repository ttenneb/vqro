"""Known credential groups, expiry and one-setup UNKNOWN targets: fake core only."""
import copy
import hashlib
import json
import unittest
from unittest.mock import patch
from account_credentials import SETTINGS
from accounts_cutover import Cutovers
from test_account_cutover_claims import Core, source, member
from accounts_policy import POLICY
from vqro_ext import VqroError


class KnownBlocked(unittest.TestCase):
    def setUp(self):
        self.core=Core();self.now=[10000.0]
        self.journal=Cutovers(self.core,clock=lambda:self.now[0])
        self.a={'id':'a','label':'A','runtime':'codex','kind':'subscription',
                'login_ref':{'sessionId':'saved-a','accountId':'native-a'}}
        self.b=dict(self.a,id='b',label='B',login_ref={'sessionId':'saved-b','accountId':'native-b'})
        self.alias=dict(self.a,id='alias-a',label='Alias A')
        self.core.entries[SETTINGS]={'version':7,'value':{'accounts':[self.a,self.b,self.alias]}}
        self.core.entries[POLICY]={'version':1,'value':{'blocked':'auto_restart','continuation_text':'One NEW turn.'}}

    def observe(self,reset=None,turn='turn-1'):
        row=source();row.update(kind='agent',available=True);native=row['data']['agent']
        native.update(lifecycle='live',caps={'agent.availability':{'reasons':['quota']}})
        native['availability']={'state':'blocked','reason':'quota','native':{'turnId':turn}}
        if reset is not None:
            native['usage_limits']=[{'scope':{'kind':'account','account':'a'},'used_percent':100,
                'observed_at_ms':self.now[0]*1000,'resets_at_ms':reset*1000,'source':'rpc:fixture'}]
        self.journal.observe_limits([row],[self.a,self.b,self.alias])
        return row

    def test_final_projection_rejects_concurrent_block_observation_during_policy_read(self):
        plan={'setup_id':'shared','membership_revision':'members','native_episode':'turn-1','affected':[member()]}
        token=self.journal.claim(source(),self.b,7,plan,True,'One NEW turn.')
        original=self.core.call;changed=[]
        def call(method,params=None):
            if method=='workspace.get' and params['key']==POLICY and not changed:
                changed.append(True);row=source();row.update(kind='agent',available=True)
                row['data']['agent'].update(account={'id':'b'},lifecycle='live',
                    caps={'agent.availability':{'reasons':['quota']}},
                    availability={'state':'blocked','reason':'quota','native':{'turnId':'new-b-limit'}})
                self.journal.observe_limits([row],[self.a,self.b,self.alias])
            return original(method,params)
        self.core.call=call
        with self.assertRaises(VqroError) as caught:self.journal.lookup(token)
        self.assertEqual(caught.exception.kind,'stale');self.assertTrue(changed)
        self.assertTrue(self.journal.known_blocked(self.b))

    def test_projection_exposes_mode_separately_from_manual_zero_budget(self):
        plan={'setup_id':'shared','membership_revision':'members','native_episode':'turn-1','affected':[member()]}
        token=self.journal.claim(source(),self.b,7,plan,False,'One NEW turn.')
        self.core.entries[POLICY]['value'].update(blocked='notify',auto_cutover_max_per_hour=0)
        self.assertEqual(self.journal.lookup(token)['blocked_mode'],'notify')
        self.assertEqual(self.journal.lookup(token)['max_switches'],0)

    def test_installed_legacy_provider_key_stays_blocked_until_its_own_expiry(self):
        from accounts_cutover import credential_key
        self.b['provider']='openai-codex';alias=dict(self.b,id='alias-b',provider='openai')
        self.core.entries[SETTINGS]['value']['accounts'].append(alias)
        row=source();row.update(kind='agent',available=True)
        row['data']['agent'].update(account={'id':'b'},lifecycle='live',
            caps={'agent.availability':{'reasons':['quota']}},
            availability={'state':'blocked','reason':'quota','native':{'turnId':'legacy-b-limit'}})
        self.journal.observe_limits([row],[self.a,self.b,alias])
        value,revision=self.journal.read();canonical=credential_key(self.b)
        # EXACT f1 identity tuple/hash, independent of the new implementation.
        old_identity=('codex','openai-codex','subscription',None,'login_ref','saved-b','native-b')
        legacy=hashlib.sha256(json.dumps(old_identity,separators=(',',':')).encode()).hexdigest()
        evidence=value['blocks'].pop(canonical);value['blocks'][legacy]=copy.deepcopy(evidence)
        self.journal.write(value,revision)
        self.assertTrue(self.journal.known_blocked(self.b));self.assertTrue(self.journal.known_blocked(alias))
        self.assertEqual(self.journal.blocked_accounts([self.b,alias]),{'b','alias-b'})
        self.now[0]+=3590
        self.journal.observe_limits([row],[self.a,self.b,alias])
        value,_=self.journal.read()
        self.assertEqual(value['blocks'][canonical],evidence);self.assertEqual(value['blocks'][legacy],evidence)
        self.now[0]+=11
        self.assertFalse(self.journal.known_blocked(self.b));self.assertFalse(self.journal.known_blocked(alias))

    def test_matching_legacy_episode_does_not_renew_a_different_canonical_episode(self):
        from accounts_cutover import credential_key
        self.b['provider']='openai-codex'
        row=source();row.update(kind='agent',available=True)
        row['data']['agent'].update(account={'id':'b'},lifecycle='live',
            caps={'agent.availability':{'reasons':['quota']}},
            availability={'state':'blocked','reason':'quota','native':{'turnId':'same-legacy-E1'}})
        self.journal.observe_limits([row],[self.a,self.b])
        value,revision=self.journal.read();canonical=credential_key(self.b)
        old_identity=('codex','openai-codex','subscription',None,'login_ref','saved-b','native-b')
        legacy=hashlib.sha256(json.dumps(old_identity,separators=(',',':')).encode()).hexdigest()
        old=copy.deepcopy(value['blocks'][canonical]);old['observed_at']=self.now[0]-100
        earlier=dict(old,observed_at=self.now[0]-200,episode='different-E0')
        value['blocks'][legacy]=old;value['blocks'][canonical]=earlier
        self.journal.write(value,revision)
        self.journal.observe_limits([row],[self.a,self.b])
        value,_=self.journal.read()
        self.assertEqual(value['blocks'][canonical],earlier);self.assertEqual(value['blocks'][legacy],old)
        self.now[0]=old['observed_at']+3601
        self.assertFalse(self.journal.known_blocked(self.b))

    def test_first_source_claim_preserves_legacy_episode_and_other_canonical_reset(self):
        from accounts_cutover import credential_key
        self.a['provider']='openai-codex';self.now[0]=9900
        row=self.observe(turn='turn-1')
        value,revision=self.journal.read();canonical=credential_key(self.a)
        old_identity=('codex','openai-codex','subscription',None,'login_ref','saved-a','native-a')
        legacy=hashlib.sha256(json.dumps(old_identity,separators=(',',':')).encode()).hexdigest()
        evidence=value['blocks'].pop(canonical)
        earlier=dict(evidence,observed_at=9800,reset_at=13300,episode='earlier-E0')
        value['blocks'].update({legacy:copy.deepcopy(evidence),canonical:copy.deepcopy(earlier)})
        self.journal.write(value,revision);self.now[0]=10000
        self.journal.observe_limits([row],[self.a,self.b,self.alias])
        plan={'setup_id':'shared','membership_revision':'members','native_episode':'turn-1','affected':[member()]}
        self.journal.claim(row,self.b,7,plan,True,'One NEW turn.')
        value,_=self.journal.read()
        self.assertEqual(value['blocks'][canonical],earlier);self.assertEqual(value['blocks'][legacy],evidence)
        self.now[0]=13501
        self.assertFalse(self.journal.known_blocked(self.a))

    def test_known_blocked_saved_codex_login_excludes_singleton_provider_spelling_alias(self):
        from account_credentials import eligible_alternatives
        self.b['provider']='openai';alias=dict(self.b,id='alias-b',provider='openai-codex')
        self.core.entries[SETTINGS]['value']['accounts'].append(alias)
        row=source();row.update(kind='agent',available=True)
        row['data']['agent'].update(account={'id':'b'},lifecycle='live',
            caps={'agent.availability':{'reasons':['quota']}},
            availability={'state':'blocked','reason':'quota','native':{'turnId':'known-b-limit'}})
        self.journal.observe_limits([row],[self.a,self.b,alias])
        self.assertTrue(self.journal.known_blocked(alias))
        # Even if only the spelling alias is offered, it is still blocked B.
        blocked=self.journal.blocked_accounts([self.a,alias])
        self.assertIn('alias-b',blocked)
        self.assertEqual(eligible_alternatives([self.a,alias],'a',blocked,False),[])

    def test_native_limit_blocks_all_known_aliases_not_unknown_b(self):
        self.observe()
        self.assertEqual(self.journal.blocked_accounts([self.a,self.alias,self.b]),{'a','alias-a'})

    def test_no_reset_expires_after_hour_without_refreshing_same_old_episode(self):
        row=self.observe()
        self.now[0]+=3600
        self.journal.observe_limits([row],[self.a,self.b,self.alias])
        self.assertFalse(self.journal.known_blocked(self.a))
        self.observe(turn='turn-2')
        self.assertTrue(self.journal.known_blocked(self.a))

    def test_percent_hint_alone_is_not_a_native_limit_verdict(self):
        row=self.observe()
        value,version=self.journal.read();value['blocks']={};self.journal.write(value,version)
        native=row['data']['agent'];native['availability']={'state':'available'}
        native['usage_limits']=[{'used_percent':100,'resets_at_ms':(self.now[0]+1000)*1000}]
        self.journal.observe_limits([row],[self.a,self.b,self.alias])
        self.assertFalse(self.journal.known_blocked(self.a))

    def test_authoritative_allowed_overrides_complete_percent_exhaustion_hint(self):
        from test_account_capacity import resource
        row=resource('meter',self.a,100,at=int(self.now[0]*1000),reset=int((self.now[0]+1000)*1000))
        row['data']['agent']['availability']={'state':'available','native':{'ordinaryUsageAllowed':True}}
        with patch('accounts_policy.now_ms',return_value=int(self.now[0]*1000)):
            self.journal.observe_limits([row],[self.a])
        self.assertFalse(self.journal.known_blocked(self.a))

    def test_native_reset_deadline_not_elapsed_time_controls_known_block(self):
        self.observe(reset=10060)
        self.now[0]=10059;self.assertTrue(self.journal.known_blocked(self.a))
        self.now[0]=10060;self.assertFalse(self.journal.known_blocked(self.alias))

    def test_source_becomes_known_blocked_at_cutover_never_auto_ping_pongs(self):
        plan={'setup_id':'shared','membership_revision':'members-1','native_episode':'turn-1','affected':[member()]}
        token=self.journal.claim(source(),self.b,7,plan,True,'One NEW turn.')
        self.assertTrue(self.journal.known_blocked(self.a))
        self.journal.native_result(token,observed=True)
        self.journal.claim_continuation(token,member())
        self.journal.continuation_result(token,member(),observed=True)
        self.journal.complete(token)
        row=source();row['data']['agent']['account']={'id':'b'}
        row['data']['agent']['epoch']='epoch-2'
        retry=copy.deepcopy(plan);retry['native_episode']='turn-2';retry['affected'][0].update(epoch='epoch-2',account='alias-a')
        with self.assertRaises(VqroError) as caught:self.journal.claim(row,self.alias,7,retry,True,'One NEW turn.')
        self.assertEqual(caught.exception.kind,'offer_required')


if __name__=='__main__':unittest.main()
