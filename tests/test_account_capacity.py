"""Capacity policy with fake core/profiles only; no native/auth/model calls."""
import copy
import json
from pathlib import Path
import sys
import unittest
from unittest.mock import patch
sys.path[:0]=[str(Path(__file__).resolve().parents[1]/'extensions'),str(Path(__file__).resolve().parent)]
from agent_fixture import FakeCore
from vqro_ext import VqroError
from accounts import Accounts
import account_credentials as profiles
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

SESSION='01234567-89ab-4cde-8fab-0123456789ab'


def pi_source(core, rid, account, **kwargs):
    """Synthetic native Pi facts, never a history-file read."""
    row=resource(rid,account,blocked=True,**kwargs)
    row.update(host='terminal-'+rid,provider='pi-runtime-'+rid)
    core.providers[row['provider']]={'role':'runtime','host':row['host'],'capabilities':{'agent.command':{}}}
    native=row['data']['agent']
    native.update(state='idle',queue=[],tool_principal='p:original-work',
        session={'id':SESSION,'native':{'file':'/synthetic/native-history.jsonl'},
                 'model':{'id':'gpt-6-luna','effort':'high','native':{'provider':account['native_provider'],'api':'openai-codex-responses'}}},
        completion={'reason':'error','error':{'message':'You have hit your ChatGPT usage limit.'},
                    'native':{'stopReason':'error','timestamp':NOW,'error_message_complete':True}},
        commands=[{'name':'vqro-account-quiesce'}])
    native['caps'].update(rev=1,**{'agent.command':{},'agent':{'tool_principal':'env'}})
    if kwargs.get('reason')=='auth':native.pop('completion')
    core.resources[rid]=row
    return row


class Core(FakeCore):
    def __init__(self):
        super().__init__('accounts');self.op_entry=None;self.launches=[];self.op_version=0
        self.fail_register=False;self.fail_claim_after=False;self.fail_launch=False
        self.resumes=[];self.stops=[];self.sends=[]
    def call(self, method, params):
        if method=='resource.invoke' and params.get('op')=='agent.command':
            self.calls.append((method,copy.deepcopy(params)))
            row=self.resources[params['resource']];native=row['data']['agent'];wire=params['args']
            if wire['guard']!={'session':native['session']['id']} or wire['v']!=0 \
                    or wire['caps_rev']!=native['caps']['rev'] or wire['epoch']!=native['epoch'] \
                    or wire['name']!='vqro-account-quiesce' or native['state']!='idle' or native['queue']!=[]:
                raise VqroError('native work/session fence changed','stale')
            expected={'session':native['session']['id'],'provider':native['session']['model']['native']['provider'],
                      'model':native['session']['model']['id'],'file':native['session']['native']['file'],
                      'error_timestamp':native['completion']['native']['timestamp']}
            if set(wire)-{'v','caps_rev','epoch','guard','cause','correlation','name','args'} \
                    or not isinstance(wire.get('args'),str) or len(wire['args'].encode('utf-8'))>16*1024:
                raise VqroError('undeclared native command arguments','unsupported')
            if json.loads(wire['args'])!=expected:raise VqroError('native resume/error fence changed','stale')
            self.stops.append(copy.deepcopy(params))
            row['available']=False;row['exit']={'code':0,'success':True}
            row['data']['agent']['lifecycle']='ended'
            return {'outcome':'observed'}
        if method=='resource.invoke' and params.get('op')=='agent.send':
            self.calls.append((method,copy.deepcopy(params)));self.sends.append(copy.deepcopy(params))
            return {'outcome':'accepted'}
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
            if self.fail_claim_after and op['value'].get('claims'):
                self.fail_claim_after=False;raise VqroError('adopted, durability unknown','durability_unconfirmed')
            return {'changes':[{'key':op['key'],'version':self.op_version}]}
        raise AssertionError(method)
    def invoke(self, name, args=None):
        if name in self.handlers:
            self.calls.append((name,copy.deepcopy(args)))
            return self.handlers[name](args)
        if name=='pi.resume':
            # Real Pi preparation/restart and common claims/authority are not mocked.
            # Fake only the public native exit/start boundary; never open history.
            from accounts_cutover import Cutovers
            claim=self.handlers['accounts.claim.read']({'claim':args['_claim']})
            source=self.resources[claim['resource']]
            Cutovers(self).validate(args['_claim'],source,claim['native_episode'])
            rows,revision=profiles.snapshot(self)
            target=next(row for row in rows if row['id']==claim['account'])
            if revision!=claim['_account_revision'] or source.get('exit')!={'code':0,'success':True} \
                    or source['available'] is not False or source['data']['agent']['lifecycle']!='ended':
                raise VqroError('native exit or account changed','stale')
            self.resumes.append(copy.deepcopy(args))
            if self.fail_launch:raise VqroError('unknown native restart outcome','indeterminate')
            rid='resumed-'+str(len(self.resumes));row=copy.deepcopy(source)
            row.update(id=rid,host='terminal-'+rid,provider='pi-runtime-'+rid,available=True)
            row.pop('exit',None);native=row['data']['agent']
            native.update(epoch='resumed-epoch-'+str(len(self.resumes)),lifecycle='live',completion=None,
                          availability={'state':'available'},account=profiles.fact(target))
            native['session']['model']['native']['provider']=target['native_provider']
            native['caps']['agent.send']={'modes':{'start':{}},'origin':[{'as':'attributed_user'}]}
            self.resources[rid]=row
            self.providers[row['provider']]={'role':'runtime','host':row['host'],'capabilities':{'agent.command':{}}}
            return {'outcome':'observed','agent':rid,'host':row['host']}
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
        row={'id':ident,'label':ident.upper(),'runtime':runtime,'kind':kind}
        if kind=='api_key':row.update(provider='openai',secret_ref='d'*32,host='terminal-old')
        elif runtime=='pi':row.update(provider='openai-codex',native_provider='vqro-account-'+ident)
        elif runtime=='codex':row['login_ref']={'sessionId':'saved-'+ident,'accountId':'native-'+ident}
        else:row.update(secret_ref='c'*32,host='terminal-old')
        self.accounts.add(row);return row
    def settings(self, **value):
        self.core.apply([{'op':'set','key':policy.POLICY,'value':dict(policy.DEFAULTS,**value)}], 'fake policy')
    def choose(self): return profiles.choose(self.core,{},'pi','pi.new')
    def blocked(self, **kwargs):
        pi_source(self.core,'old',self.a,**kwargs)
    def test_common_rechecks_pi_effort_after_callback_before_new_continuation(self):
        self.blocked();self.settings(blocked='auto_restart');original=self.core.invoke
        def invoke(name,args=None):
            result=original(name,args)
            if name=='accounts.cutover.pi':
                for item in result['affected']:
                    self.core.resources[item['resource']]['data']['agent']['session']['model']['effort']='medium'
            return result
        self.core.invoke=invoke;self.accounts.handle_blocked('old')
        self.assertEqual(len(self.core.resumes),1);self.assertEqual(self.core.sends,[])
        self.assertEqual(next(iter(self.core.op_entry['value']['claims'].values()))['state'],'unknown')

    def test_latest_pi_destination_read_effort_change_refuses_before_submission(self):
        self.blocked();self.settings(blocked='auto_restart')
        original_invoke=self.core.invoke;original_call=self.core.call;destination=[];reads=[]
        def invoke(name,args=None):
            result=original_invoke(name,args)
            if name=='accounts.cutover.pi':destination.append(result['affected'][0]['resource'])
            return result
        def call(method,params=None):
            if method=='resource.list' and destination and params.get('id')==destination[0]:
                reads.append(True)
                if len(reads)==2:
                    self.core.resources[destination[0]]['data']['agent']['session']['model']['effort']='medium'
            return original_call(method,params)
        self.core.invoke=invoke;self.core.call=call
        self.accounts.handle_blocked('old')
        self.assertEqual(len(reads),2);self.assertEqual(self.core.sends,[])
        self.assertTrue(any('stale' in message for message,_ in self.core.notices))

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
        self.assertEqual(self.core.resumes,[]);self.assertEqual(self.core.stops,[])
        self.assertEqual(self.core.sends,[])

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

    def test_auto_accepts_unknown_capacity_without_fabricating_positive_headroom(self):
        self.settings(blocked='auto_restart');self.blocked()
        self.core.resources['target']=resource('target',self.b,10)
        self.core.resources['target']['data']['agent']['incomplete']=['usage_limits']
        self.assertIsNone(policy.capacities(list(self.core.resources.values()),[self.b])['b'])
        self.accounts.handle_blocked('old')
        self.assertEqual(len(self.core.resumes),1);self.assertEqual(len(self.core.sends),1)
        self.assertEqual(self.core.launches,[])

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

    def test_manual_offer_preserves_session_never_copies_input_and_claim_is_not_history(self):
        self.settings(blocked='offer_restart');self.blocked()
        native=self.core.resources['old']['data']['agent']
        native['queue']=[{'text':'must not copy'}]
        reply=self.accounts.offer({'resource':'old'})
        options=[row for row in self.core.live[reply['client']['popup']['view']]['items'] if row.get('command')=='accounts.restart']
        with self.assertRaises(VqroError):self.accounts.restart(options[0]['args'])
        self.assertEqual(native['queue'],[{'text':'must not copy'}])
        self.assertEqual(self.core.stops,[]);self.assertEqual(self.core.resumes,[])
        self.assertEqual(self.core.op_entry['value']['claims'],{})
        native['queue']=[]  # explicit fixture recovery, never automatic input replay
        self.accounts.restart(dict(options[0]['args'],text='must not copy',session='old-session',model='old-model'))
        self.assertEqual(set(self.core.resumes[0]),{'_claim'})
        self.assertEqual(self.core.resources['resumed-1']['data']['agent']['session']['id'],SESSION)
        self.assertEqual(self.core.sends,[]);self.assertEqual(self.core.launches,[])
        self.assertNotIn('must not copy',str(self.core.stops)+str(self.core.resumes))
        self.assertFalse(any(op['key'].startswith('doc/accounts/blocked') for txn in self.core.transactions for op in txn))
        registrations=[params for method,params in self.core.calls if method=='operational.register']
        self.assertTrue(registrations)
        self.assertTrue(all(row=={'name':'accounts','bytes':128*1024} for row in registrations))

    def test_automatic_runtime_ambiguity_and_resume_configuration_fail_before_claim(self):
        self.settings(blocked='auto_restart');self.blocked();self.core.resources['target']=resource('target',self.b,10)
        variants=[{'name':'one','argv':['pi1','--mode','rpc']},{'name':'two','argv':['pi1','--mode','rpc']}]
        for configured in (variants,[{'name':'one','argv':['pi1','--continue']}]):
            self.core.apply([{'op':'set','key':'doc/settings/pi','value':{'runtimes':configured}}],'fake invalid runtime')
            self.accounts.handle_blocked('old')
            self.assertEqual(self.core.resumes,[]);self.assertEqual(self.core.stops,[])
            self.assertEqual(self.core.op_entry['value']['claims'],{})

    def test_same_native_error_cannot_repeat_even_when_adapter_epoch_returns(self):
        self.settings(blocked='auto_restart');self.blocked()
        original=copy.deepcopy(self.core.resources['old'])
        self.accounts.handle_blocked('old');self.assertEqual(len(self.core.resumes),1)
        for epoch in ('e2','e1'):
            self.core.resources['old']=copy.deepcopy(original)
            self.core.resources['old']['data']['agent']['epoch']=epoch
            self.accounts.handle_blocked('old')
        self.assertEqual(len(self.core.resumes),1);self.assertEqual(len(self.core.sends),1)

    def test_source_change_after_durable_claim_prevents_launch(self):
        self.settings(blocked='auto_restart');self.blocked();self.core.resources['target']=resource('target',self.b,10)
        call=self.core.call
        def change_source(method,params):
            result=call(method,params)
            if method=='operational.apply' and params['ops'][0]['value'].get('claims'):
                self.core.resources['old']['data']['agent']['epoch']='changed-after-claim'
            return result
        with patch.object(self.core,'call',side_effect=change_source):self.accounts.handle_blocked('old')
        self.assertEqual(self.core.resumes,[]);self.assertIsNotNone(self.core.op_entry)
        self.assertTrue(self.core.op_entry['value']['claims'])

    def test_unrelated_resource_updates_do_not_reschedule_limit_work(self):
        self.blocked();self.accounts.changed('resource',{'id':'old'});self.assertEqual(self.accounts.pending,{'old'})
        self.accounts.pending.clear()
        self.core.resources['other']=resource('other',self.b)
        self.accounts.changed('resource',{'id':'other'});self.assertEqual(self.accounts.pending,set())
        self.accounts.changed('entries',{'changes':[{'key':'doc/unrelated'}]});self.assertEqual(self.accounts.pending,set())
        self.accounts.changed('resource',{'id':'old'});self.assertEqual(self.accounts.pending,set())

    def test_completeness_and_availability_declaration_changes_reschedule_work(self):
        self.settings(blocked='auto_restart');self.blocked()
        target=resource('target',self.b,10);target['data']['agent']['incomplete']=['usage_limits']
        self.core.resources['target']=target
        self.accounts.changed('resource',{'id':'target'});self.accounts.pending.clear()
        target['data']['agent']['incomplete']=[]
        self.accounts.changed('resource',{'id':'target'});self.assertEqual(self.accounts.pending,{'old'})
        self.accounts.pending.clear();self.accounts.observations.clear()
        native=self.core.resources['old']['data']['agent']
        completion=native.pop('completion');native['caps']['agent.availability']={'reasons':[]}
        self.accounts.changed('resource',{'id':'old'});self.assertEqual(self.accounts.pending,set())
        native['completion']=completion;native['caps']['agent.availability']={'reasons':['quota']}
        self.accounts.changed('resource',{'id':'old'});self.assertEqual(self.accounts.pending,{'old'})

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
        self.assertEqual(self.core.resumes,[]);self.assertEqual(self.core.sends,[])
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
    def test_offer_same_conversation_refuses_stale_source_before_any_effect(self):
        self.settings(blocked='offer_restart');self.blocked()
        self.accounts.handle_blocked('old');self.assertEqual(self.core.launches,[])
        answer=self.accounts.offer({'resource':'old'})
        view=self.core.live[answer['client']['popup']['view']]
        options=[row for row in view['items'] if row.get('command')=='accounts.restart']
        self.assertTrue(options)
        args=options[0]['args'];self.core.resources['old']['data']['agent']['epoch']='changed'
        with self.assertRaises(VqroError):self.accounts.restart(args)
        self.assertEqual(self.core.launches,[]);self.assertEqual(self.core.resumes,[])
        self.assertEqual(self.core.stops,[]);self.assertEqual(self.core.sends,[])
    def test_auto_restart_claim_survives_extension_restart_and_never_replays_source_input(self):
        self.settings(blocked='auto_restart');self.blocked()
        original=copy.deepcopy(self.core.resources['old'])
        self.core.resources['old']['data']['agent']['old_input']='NEVER REPLAY THIS'
        self.accounts.handle_blocked('old');self.assertEqual(len(self.core.resumes),1)
        new=self.core.resources['resumed-1']['data']['agent']
        self.assertEqual(new['session']['id'],SESSION);self.assertEqual(new['account']['id'],'b')
        self.assertEqual(len(self.core.sends),1);wire=self.core.sends[0]['args']
        self.assertEqual(wire['origin']['kind'],'tool');self.assertEqual(wire['guard'],{'session':SESSION})
        self.assertEqual(wire['content'],[{'type':'text','text':policy.CONTINUATION_TEXT}])
        self.assertNotIn('NEVER REPLAY THIS',str(wire))
        self.core.resources['old']=original
        Accounts(self.core).handle_blocked('old');self.assertEqual(len(self.core.resumes),1)
        self.assertEqual(len(self.core.sends),1);self.assertEqual(self.core.launches,[])
    def test_uncertain_claim_or_native_restart_is_never_retried(self):
        for stage in ('claim','restart'):
            with self.subTest(stage=stage):
                self.core=Core();self.accounts=Accounts(self.core)
                self.a=self.add('a');self.b=self.add('b')
                self.settings(blocked='auto_restart');self.blocked()
                original=copy.deepcopy(self.core.resources['old'])
                self.core.fail_claim_after=stage=='claim';self.core.fail_launch=stage=='restart'
                self.accounts.handle_blocked('old');expected=0 if stage=='claim' else 1
                self.assertEqual(len(self.core.resumes),expected)
                self.core.resources['old']=original
                Accounts(self.core).handle_blocked('old')
                self.assertEqual(len(self.core.resumes),expected);self.assertEqual(self.core.sends,[])
    def test_new_episodes_switch_same_conversation_up_to_hourly_budget_not_a_child_chain(self):
        self.settings(blocked='auto_restart');self.blocked()
        for ident in ('d','e','f'):self.add(ident)
        source='old'
        for count in range(1,5):
            if count>1:
                native=self.core.resources[source]['data']['agent']
                native['completion']={'reason':'error','error':{'message':'You have hit your ChatGPT usage limit.'},
                    'native':{'stopReason':'error','timestamp':NOW+count,'error_message_complete':True}}
                native['availability']={'state':'blocked','reason':'quota'}
            self.accounts.handle_blocked(source)
            self.assertEqual(len(self.core.resumes),min(count,3))
            self.assertEqual(len(self.core.sends),min(count,3))
            if count<=3:source='resumed-'+str(count)
        self.assertEqual(self.core.launches,[])
        self.assertTrue(all(row['args']['guard']=={'session':SESSION} for row in self.core.sends))
    def test_auth_block_and_missing_protected_store_never_auto_restart(self):
        self.settings(blocked='auto_restart');self.blocked(reason='auth')
        self.accounts.handle_blocked('old');self.assertEqual(self.core.resumes,[])
        self.blocked();self.core.fail_register=True
        self.accounts.handle_blocked('old');self.assertEqual(self.core.resumes,[])
        self.assertEqual(self.core.sends,[])
    def test_source_and_known_blocked_credential_aliases_are_not_alternatives(self):
        self.settings(blocked='auto_restart');self.blocked()
        self.accounts.add(dict(self.a,id='alias-a',label='Alias A'))
        self.accounts.add(dict(self.b,id='alias-b',label='Alias B'))
        source=self.core.resources['old']
        candidates,_,_,_=self.accounts.alternatives(source,policy.settings(self.core))
        self.assertEqual([row['id'] for row in candidates],['b'])
        self.core.resources['target']=resource('target',self.b,100)
        candidates,_,_,_=self.accounts.alternatives(source,policy.settings(self.core))
        self.assertEqual(candidates,[])
        self.accounts.handle_blocked('old')
        self.assertEqual(self.core.resumes,[]);self.assertEqual(self.core.sends,[])

    def test_cross_runtime_and_api_key_accounts_never_become_same_session_targets(self):
        self.settings(blocked='auto_restart');self.blocked()
        api=self.add('paid',runtime='pi',kind='api_key');codex=self.add('codex',runtime='codex')
        rows,_=profiles.snapshot(self.core)
        self.core.apply([{'op':'set','key':profiles.SETTINGS,'value':{'accounts':[row for row in rows if row['id']!='b']}}],'remove Pi subscription alternative')
        for overflow in (False,True):
            self.settings(blocked='auto_restart',api_key_overflow=overflow)
            candidates,_,_,_=self.accounts.alternatives(self.core.resources['old'],policy.settings(self.core))
            self.assertEqual(candidates,[]);self.accounts.handle_blocked('old')
        self.assertIsNone(profiles.selection_problem(api));self.assertIsNone(profiles.selection_problem(codex))
        self.assertEqual(self.core.resumes,[]);self.assertEqual(self.core.launches,[])
        self.assertEqual(self.core.sends,[])

if __name__=='__main__':unittest.main()
