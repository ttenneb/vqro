"""Capacity policy with fake core/profiles only; no native/auth/model calls."""
import copy
from pathlib import Path
import sys
import unittest
from unittest.mock import patch
sys.path[:0]=[str(Path(__file__).resolve().parents[1]/'extensions'),str(Path(__file__).resolve().parent)]
from agent_fixture import FakeCore
from vqro_ext import VqroError
from accounts import Accounts
import account_profiles as profiles
import accounts_policy as policy

NOW=200_000

def resource(rid, account, used=None, *, blocked=False, reason='quota', epoch='e1', at=NOW, reset=NOW+30_000):
    facts={'v':0,'epoch':epoch,'lifecycle':'live','account':profiles.fact(account),
           'caps':{'agent.availability':{'reasons':['quota','rate_limited','auth']}}}
    if blocked: facts['availability']={'state':'blocked','reason':reason}
    if used is not None:
        facts['usage_limits']=[{'scope':{'kind':'account','account':account['id']},'used_percent':used,
                              'observed_at_ms':at,'resets_at_ms':reset,'source':'rpc:fixture'}]
    return {'id':rid,'kind':'agent','available':True,'incarnation':'i1','data':{'agent':facts}}

class Core(FakeCore):
    def __init__(self):
        super().__init__('accounts');self.op_entry=None;self.launches=[];self.op_version=0
        self.fail_register=False;self.fail_claim_after=False;self.fail_launch=False
    def call(self, method, params):
        if not method.startswith('operational.'): return super().call(method,params)
        self.calls.append((method,copy.deepcopy(params)))
        if method=='operational.register':
            if self.fail_register: raise VqroError('not available','unsupported')
            return {'domain':'accounts'}
        if method=='operational.get': return {'entry':copy.deepcopy(self.op_entry)}
        if method=='operational.apply':
            op=params['ops'][0]
            if op['expect'] != (self.op_entry or {}).get('version',0): raise VqroError('changed','conflict')
            self.op_version+=1;self.op_entry={'version':self.op_version,'value':copy.deepcopy(op['value'])}
            if self.fail_claim_after:
                self.fail_claim_after=False;raise VqroError('adopted, durability unknown','durability_unconfirmed')
            return {'changes':[{'key':op['key'],'version':self.op_version}]}
        raise AssertionError(method)
    def invoke(self, name, args=None):
        if name in ('pi.new','codex.new','tier1.claude'):
            self.launches.append((name,copy.deepcopy(args)))
            if self.fail_launch: raise VqroError('unknown outcome','indeterminate')
            return {'outcome':'observed','agent':'fresh-'+str(len(self.launches)),'host':'host-'+str(len(self.launches))}
        return super().invoke(name,args)

class CapacityPolicy(unittest.TestCase):
    def setUp(self):
        self.core=Core();self.accounts=Accounts(self.core)
        self.a=self.add('a');self.b=self.add('b');self.c=self.add('c',runtime='claude')
        self.clock=patch('accounts_policy.now_ms',return_value=NOW);self.clock.start();self.addCleanup(self.clock.stop)
    def add(self, ident, runtime='pi', kind='subscription'):
        row={'id':ident,'label':ident.upper(),'runtime':runtime,'kind':kind,'profile':'/fake/'+ident}
        self.accounts.add(row);return row
    def settings(self, **value):
        self.core.apply([{'op':'set','key':policy.POLICY,'value':dict(policy.DEFAULTS,**value)}], 'fake policy')
    def choose(self): return profiles.choose(self.core,{},'pi','pi.new')
    def blocked(self, **kwargs):
        self.core.resources['old']=resource('old',self.a,blocked=True,**kwargs)
    def test_fresh_launchers_refuse_resume_or_prompt_argv_before_spawn(self):
        from pi import Pi
        from codex import Codex
        for cls,runtime,argv in [(Pi,'pi',['pi1','--mode','rpc','--session','old']),
                                 (Codex,'codex',['codex','app-server','--resume','old'])]:
            core=Core();launcher=cls(core)
            core.apply([{'op':'set','key':'doc/settings/'+runtime,'value':{'runtimes':[{'name':runtime,'argv':argv}]}}],'fake config')
            with self.subTest(runtime=runtime),self.assertRaises(VqroError): launcher.new({'account':'normal','fresh':True})
            self.assertFalse(any(method=='resource.spawn' for method,args in core.calls))
        for runtime,argv in [('pi',['pi1','--continue']),('claude',['claude','--resume','old'])]:
            with self.assertRaises(VqroError): profiles.require_fresh(runtime,argv)

    def test_empty_registry_capacity_mode_asks_instead_of_guessing_normal_capacity(self):
        self.settings(new_agents='most_remaining_capacity')
        self.core.apply([{'op':'set','key':profiles.SETTINGS,'value':{'accounts':[]}}],'fake empty registry')
        account,popup=self.choose();self.assertIsNone(account);self.assertIn('popup',popup['client'])

    def test_unknown_policy_falls_back_to_ask_notify_and_can_be_replaced(self):
        self.core.apply([{'op':'set','key':policy.POLICY,'value':{'new_agents':'future_mode','blocked':'auto_restart'}}],'future policy')
        self.assertEqual(policy.settings(self.core),policy.DEFAULTS)
        revision=self.core.version(policy.POLICY)
        self.accounts.configure_policy({'field':'new_agents','value':'round_robin','expect':revision})
        self.assertEqual(policy.settings(self.core)['new_agents'],'round_robin')
        with self.assertRaises(VqroError):self.accounts.configure_policy({'field':'blocked','value':'auto_restart','expect':revision})

    def test_future_or_corrupt_journal_fails_closed_without_native_action(self):
        self.settings(blocked='auto_restart');self.blocked();self.core.resources['target']=resource('target',self.b,10)
        self.core.op_entry={'version':1,'value':{'attempts':{'old':'unsupported future row'},'children':[],'hosts':[],'uncertain':{}}}
        self.accounts.handle_blocked('old');self.assertEqual(self.core.launches,[])

    def test_implicit_new_agent_choices_do_not_use_api_key_profiles_without_opt_in(self):
        paid=self.add('paid',kind='api_key')
        self.settings(new_agents='round_robin')
        with self.subTest(mode='round_robin'):
            self.assertEqual([self.choose()[0]['id'] for _ in range(4)],['a','b','a','b'])
        self.settings(new_agents='most_remaining_capacity')
        self.core.resources['free']=resource('free',self.b,50)
        self.core.resources['paid']=resource('paid',paid,0)
        with self.subTest(mode='most_remaining_capacity'):self.assertEqual(self.choose()[0]['id'],'b')
        self.settings(new_agents='most_remaining_capacity',api_key_overflow=True)
        self.assertEqual(self.choose()[0]['id'],'paid')
        explicit,_=profiles.choose(self.core,{'account':'paid'},'pi','pi.new')
        self.assertEqual(explicit['id'],'paid')

    def test_incomplete_and_mixed_unknown_windows_never_become_partial_capacity(self):
        base=resource('target',self.b,10)
        incomplete=copy.deepcopy(base);incomplete['data']['agent']['incomplete']=['usage_limits']
        cases=[incomplete]
        template=base['data']['agent']['usage_limits'][0]
        for changes in [{'observed_at_ms':NOW-60_001,'used_percent':100},
                        {'resets_at_ms':NOW-1},{'observed_at_ms':NOW+10000},
                        {'source':''},{'used_percent':None,'remaining':50}]:
            row=copy.deepcopy(base)
            row['data']['agent']['usage_limits'].append(dict(template,window={'id':'week'},**changes))
            cases.append(row)
        for row in cases:
            with self.subTest(row=row):self.assertIsNone(policy.capacities([row],[self.b])['b'])
        replaced=copy.deepcopy(cases[1])
        replaced['data']['agent']['usage_limits'].append(dict(template,window={'id':'week'},used_percent=20))
        self.assertEqual(policy.capacities([replaced],[self.b])['b'],80)  # newer authoritative SAME window
        incomplete['data']['agent']['incomplete']=[]
        self.assertEqual(policy.capacities([incomplete],[self.b])['b'],90)

    def test_auto_restart_does_not_use_incomplete_limit_set(self):
        self.settings(blocked='auto_restart');self.blocked()
        self.core.resources['target']=resource('target',self.b,10)
        self.core.resources['target']['data']['agent']['incomplete']=['usage_limits']
        self.accounts.handle_blocked('old');self.assertEqual(self.core.launches,[])

    def test_capacity_picker_does_not_drop_a_stale_restrictive_window(self):
        self.settings(new_agents='most_remaining_capacity');self.blocked()
        self.core.resources['target']=resource('target',self.b,10)
        limits=self.core.resources['target']['data']['agent']['usage_limits']
        limits.append(dict(limits[0],window={'id':'week'},used_percent=100,observed_at_ms=NOW-60_001))
        account,popup=self.choose();self.assertIsNone(account);self.assertIn('popup',popup['client'])

    def test_round_robin_cas_failure_does_not_start_anything(self):
        self.settings(new_agents='round_robin');apply=self.core.apply
        def fail_cursor(ops,note):
            if ops[0]['key'].startswith(policy.CURSORS):raise VqroError('changed','conflict')
            return apply(ops,note)
        with patch.object(self.core,'apply',side_effect=fail_cursor),self.assertRaises(VqroError):self.choose()
        self.assertEqual(self.core.launches,[])

    def test_manual_offer_is_deliberately_empty_and_claim_is_not_workspace_history(self):
        self.settings(blocked='offer_restart');self.blocked()
        self.core.resources['old']['data']['agent']['queue']={'follow_up':[{'text':'fake original input'}]}
        original=copy.deepcopy(self.core.resources['old'])
        reply=self.accounts.offer({'resource':'old'})
        options=[row for row in self.core.live[reply['client']['popup']['view']]['items'] if row.get('command')=='accounts.restart']
        self.accounts.restart(dict(options[0]['args'],text='must not copy',session='old-session',model='old-model'))
        self.assertEqual(set(self.core.launches[0][1]),{'account','_account_revision','fresh'})
        self.assertEqual(self.core.resources['old'],original)
        self.assertFalse(any(op['key'].startswith('doc/accounts/blocked') for txn in self.core.transactions for op in txn))
        registrations=[params for method,params in self.core.calls if method=='operational.register']
        self.assertEqual(registrations,[{'name':'accounts','bytes':128*1024}])

    def test_automatic_runtime_ambiguity_and_resume_configuration_fail_before_claim(self):
        self.settings(blocked='auto_restart');self.blocked();self.core.resources['target']=resource('target',self.b,10)
        variants=[{'name':'one','argv':['pi1','--mode','rpc']},{'name':'two','argv':['pi1','--mode','rpc']}]
        self.core.apply([{'op':'set','key':'doc/settings/pi','value':{'runtimes':variants}}],'fake variants')
        self.accounts.handle_blocked('old');self.assertEqual(self.core.launches,[]);self.assertIsNone(self.core.op_entry)
        self.core.apply([{'op':'set','key':'doc/settings/pi','value':{'runtimes':[{'name':'one','argv':['pi1','--continue']}]}}],'fake resume')
        self.accounts.handle_blocked('old');self.assertEqual(self.core.launches,[]);self.assertIsNone(self.core.op_entry)

    def test_returning_to_an_old_native_epoch_does_not_repeat_its_fresh_launch(self):
        self.settings(blocked='auto_restart');self.blocked();self.core.resources['target']=resource('target',self.b,10)
        self.accounts.handle_blocked('old');self.assertEqual(len(self.core.launches),1)
        self.core.resources['old']['data']['agent']['epoch']='e2'
        self.accounts.handle_blocked('old');self.assertEqual(len(self.core.launches),2)
        self.core.resources['old']['data']['agent']['epoch']='e1'
        self.accounts.handle_blocked('old');self.assertEqual(len(self.core.launches),2)

    def test_source_change_after_durable_claim_prevents_launch(self):
        self.settings(blocked='auto_restart');self.blocked();self.core.resources['target']=resource('target',self.b,10)
        call=self.core.call
        def change_source(method,params):
            result=call(method,params)
            if method=='operational.apply':self.core.resources['old']['data']['agent']['epoch']='changed-after-claim'
            return result
        with patch.object(self.core,'call',side_effect=change_source):self.accounts.handle_blocked('old')
        self.assertEqual(self.core.launches,[]);self.assertIsNotNone(self.core.op_entry)

    def test_unrelated_resource_updates_do_not_reschedule_limit_work(self):
        self.blocked();self.accounts.changed('resource',{'id':'old'});self.assertEqual(self.accounts.pending,{'old'})
        self.accounts.pending.clear()
        self.core.resources['other']=resource('other',self.b)
        self.accounts.changed('resource',{'id':'other'});self.assertEqual(self.accounts.pending,set())
        self.accounts.changed('entries',{'changes':[{'key':'doc/unrelated'}]});self.assertEqual(self.accounts.pending,set())
        self.accounts.changed('resource',{'id':'old'});self.assertEqual(self.accounts.pending,set())

    def test_fresh_flag_survives_runtime_picker(self):
        from pi import Pi
        core=Core();launcher=Pi(core)
        core.apply([{'op':'set','key':'doc/settings/pi','value':{'runtimes':[
            {'name':'one','argv':['pi1','--mode','rpc']},{'name':'two','argv':['pi1','--mode','rpc']}]}}],'fake variants')
        reply=launcher.new({'account':'normal','fresh':True})
        options=core.live[reply['client']['popup']['view']]['items']
        self.assertTrue(options)
        self.assertTrue(all(row['args']['fresh'] is True for row in options if row.get('command')=='pi.new'))

    def test_defaults_are_ask_and_notify(self):
        self.assertEqual(policy.settings(self.core)['new_agents'],'ask')
        self.assertEqual(policy.settings(self.core)['blocked'],'notify')
        account,popup=self.choose();self.assertIsNone(account);self.assertIn('popup',popup['client'])
        self.blocked();self.accounts.handle_blocked('old');self.accounts.handle_blocked('old')
        self.assertEqual(len(self.core.launches),0)
        self.assertEqual(len([text for text,_ in self.core.notices if 'limit' in text.lower()]),1)
    def test_most_remaining_uses_restrictive_reported_window(self):
        self.settings(new_agents='most_remaining_capacity')
        self.core.resources['one']=resource('one',self.a,20)
        self.core.resources['one']['data']['agent']['usage_limits'].append(dict(
            self.core.resources['one']['data']['agent']['usage_limits'][0],window={'id':'week'},used_percent=95))
        self.core.resources['two']=resource('two',self.b,40)
        account,popup=self.choose();self.assertEqual(account['id'],'b');self.assertIsNone(popup)
    def test_unknown_stale_expired_future_and_unscoped_limits_never_become_capacity(self):
        self.settings(new_agents='most_remaining_capacity')
        cases=[resource('x',self.a),resource('x',self.a,1,at=NOW-60_001),resource('x',self.a,1,reset=NOW-1),
               resource('x',self.a,1,at=NOW+10_000),resource('x',self.a,True)]
        unscoped=resource('x',self.a,1);del unscoped['data']['agent']['usage_limits'][0]['scope']['account'];cases.append(unscoped)
        counts=resource('x',self.a);counts['data']['agent']['usage_limits']=[{'scope':{'kind':'account','account':'a','unit':'tokens'},
            'remaining':10000,'source':'rpc:fixture','observed_at_ms':NOW}];cases.append(counts)
        for candidate in cases:
            self.core.resources={'x':candidate}
            with self.subTest(candidate=candidate):
                account,popup=self.choose();self.assertIsNone(account);self.assertIn('popup',popup['client'])
    def test_round_robin_cursor_persists_and_explicit_choice_does_not_advance_it(self):
        self.settings(new_agents='round_robin')
        self.assertEqual([self.choose()[0]['id'] for _ in range(3)],['a','b','a'])
        profiles.choose(self.core,{'account':'a'},'pi','pi.new')
        self.assertEqual(self.choose()[0]['id'],'b')
    def test_known_blocked_account_is_not_implicitly_selected(self):
        self.settings(new_agents='round_robin');self.blocked()
        self.assertEqual(self.choose()[0]['id'],'b')
    def test_offer_is_fresh_empty_work_and_stale_source_is_refused(self):
        self.settings(blocked='offer_restart');self.blocked()
        self.accounts.handle_blocked('old');self.assertEqual(self.core.launches,[])
        answer=self.accounts.offer({'resource':'old'})
        view=self.core.live[answer['client']['popup']['view']]
        options=[row for row in view['items'] if row.get('command')=='accounts.restart']
        self.assertTrue(options)
        args=options[0]['args'];self.core.resources['old']['data']['agent']['epoch']='changed'
        with self.assertRaises(VqroError):self.accounts.restart(args)
        self.assertEqual(self.core.launches,[])
    def test_auto_restart_claim_survives_extension_restart_and_never_replays_source_input(self):
        self.settings(blocked='auto_restart');self.blocked()
        self.core.resources['target']=resource('target',self.b,10)
        self.accounts.handle_blocked('old');self.assertEqual(len(self.core.launches),1)
        command,args=self.core.launches[0];self.assertEqual(command,'pi.new')
        self.assertEqual(set(args),{'account','_account_revision','fresh'})
        self.assertTrue(args['fresh']);self.assertEqual(args['account'],'b')
        self.assertEqual(self.core.resources['old']['data']['agent']['epoch'],'e1')
        Accounts(self.core).handle_blocked('old');self.assertEqual(len(self.core.launches),1)
        self.assertFalse(any(m in ('resource.kill','resource.invoke','agent.send') for m,p in self.core.calls))
    def test_uncertain_claim_or_spawn_is_never_retried(self):
        self.settings(blocked='auto_restart');self.blocked();self.core.resources['target']=resource('target',self.b,10)
        self.core.fail_claim_after=True
        self.accounts.handle_blocked('old');self.assertEqual(self.core.launches,[])
        Accounts(self.core).handle_blocked('old');self.assertEqual(self.core.launches,[])
        self.core.resources['old']['data']['agent']['epoch']='e2';self.core.fail_launch=True
        self.core.resources['other']=resource('other',self.c,10)  # original target remains uncertain
        self.accounts.handle_blocked('old');self.assertEqual(len(self.core.launches),1)
        Accounts(self.core).handle_blocked('old');self.assertEqual(len(self.core.launches),1)
    def test_auto_created_agents_do_not_form_restart_chains(self):
        self.settings(blocked='auto_restart');self.blocked();self.core.resources['target']=resource('target',self.b,10)
        self.accounts.handle_blocked('old')
        self.core.resources['fresh-1']=resource('fresh-1',self.b,blocked=True)
        self.core.resources['other']=resource('other',self.c,1)
        self.accounts.handle_blocked('fresh-1');self.assertEqual(len(self.core.launches),1)
    def test_auth_block_unknown_capacity_and_missing_protected_store_never_auto_launch(self):
        self.settings(blocked='auto_restart');self.blocked(reason='auth')
        self.core.resources['target']=resource('target',self.b,10)
        self.accounts.handle_blocked('old');self.assertEqual(self.core.launches,[])
        self.blocked();del self.core.resources['target']
        self.accounts.handle_blocked('old');self.assertEqual(self.core.launches,[])
        self.core.resources['target']=resource('target',self.b,10);self.core.fail_register=True
        self.accounts.handle_blocked('old');self.assertEqual(self.core.launches,[])
    def test_cross_provider_fresh_restart_and_api_key_overflow_opt_in(self):
        self.settings(blocked='auto_restart');self.blocked()
        api=self.add('paid',runtime='codex',kind='api_key')
        self.core.resources['paid']=resource('paid',api,0)
        self.accounts.handle_blocked('old');self.assertEqual(self.core.launches,[])
        self.settings(blocked='auto_restart',api_key_overflow=True)
        self.accounts.handle_blocked('old');self.assertEqual(self.core.launches[0][0],'codex.new')
        self.assertEqual(self.core.launches[0][1]['account'],'paid')

if __name__=='__main__':unittest.main()
