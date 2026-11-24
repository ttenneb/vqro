import copy
import importlib.util
import pathlib
import sys
import unittest
import time
import itertools
import threading
from unittest.mock import patch
ROOT=pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0,str(ROOT/'extensions'))
from vqro_ext import Vqro, VqroError
from codex_cutover import Cutover, SETTINGS, HOST_COMMANDS
from accounts_policy import episode, DEFAULTS, POLICY


def row(ident, host, native_host, state='idle'):
    return {'id':ident,'available':True,'host':host,'incarnation':'inc-'+ident,'status':{'app':'codex'},'data':{'agent':{
        'v':0,'lifecycle':'live','state':state,'run':None if state=='idle' else 'running','epoch':'epoch-'+ident,
        'host':{'id':native_host},'session':{'id':'thread-'+ident},'account_setup':{'id':'shared'},
        '_codex':{'input_ledger':{'v':0,'source':'vqro-ledger','epoch':'epoch-'+ident,'complete':True,'pending':0,'unknown':0}},
        'account':{'id':'a','kind':'subscription','rev':1,'native':{'login_ref':{'sessionId':'saved-a','accountId':'workspace-a'}}},'caps':{'rev':1,'agent.command':{},'agent.availability':{'reasons':['quota','rate_limited']}},'commands':[{'name':n} for n in HOST_COMMANDS],'queue':None,'requests':[], 'background':None,
        'availability':{'state':'blocked','reason':'quota','native':{'threadId':'thread-'+ident,'turnId':'native-turn',
            'codexErrorInfo':'usageLimitExceeded','willRetry':False}}}}}

class Ext:
    def __init__(self):
        self.vqro=self;self.rows=[row('x','execution','native-x'),row('y','execution','native-y')];self.calls=[]
        probe=row('probe','execution','native-probe');p=probe['data']['agent']
        p.update(account_setup={'id':'other-setup'},account={'id':'b','kind':'subscription','rev':1,'native':{'login_ref':{'sessionId':'saved-b','accountId':'workspace-b'}}},availability={'state':'available'},incomplete=[])
        now=int(time.time()*1000)
        p['usage_limits']=[{'scope':{'kind':'account','account':'b'},'window':{'id':'primary'},'used_percent':10,
            'source':'codex.account/rateLimits','observed_at_ms':now,'resets_at_ms':now+60000}]
        self.rows.append(probe)
        self.registry={'accounts':[{'id':'b','label':'B','runtime':'codex','kind':'subscription',
            'login_ref':{'sessionId':'saved-b','accountId':'workspace-b'}},
            {'id':'a','label':'A','runtime':'codex','kind':'subscription','login_ref':{'sessionId':'saved-a','accountId':'workspace-a'}}]}
        self.consent={'setups':{'shared':{'automatic_login_switch':True}}};self.claim=None;self.fail=None;self.counter=0
        self.owner={'value':'human-owner','version':5}
        self.policy=dict(DEFAULTS,blocked='auto_restart')
        self.registry_revision=3;self.hook=None;self.skip_publication=False;self.reject=None
        self.all_active=False;self.missing_on=None;self.selected={'x':'a','y':'a'};self.quota={'b':True,'c':True}
        self.max_switches=3;self.known_blocked=[];self.released=True
    def command(self,*_):return lambda fn:fn
    def call(self,method,args):
        self.calls.append((method,copy.deepcopy(args)))
        if method=='resource.list':return {'resources':copy.deepcopy(self.rows)}
        if method=='workspace.get':
            if args['key']=='doc/principals/owner':return copy.deepcopy(self.owner)
            if args['key']==POLICY:return {'version':4,'value':copy.deepcopy(self.policy)}
            return {'version':3 if args['key']==SETTINGS else self.registry_revision,'value':copy.deepcopy(self.consent if args['key']==SETTINGS else self.registry)}
        if method=='command.invoke':
            assert args['name']=='accounts.claim.read'
            self.call('workspace.get',{'key':'doc/settings/accounts'}) # Actual protected lookup blocks on this read.
            if isinstance(self.claim,dict) and self.registry_revision!=self.claim['_account_revision']:raise VqroError('registry changed','stale')
            return dict(copy.deepcopy(self.claim),blocked_mode=self.policy['blocked'],max_switches=self.max_switches if self.policy['blocked']=='auto_restart' else 0,known_blocked_targets=copy.deepcopy(self.known_blocked)) if isinstance(self.claim,dict) else None
        if method=='resource.invoke':
            op=args['args']['name'];self.counter+=1
            if self.hook:self.hook(op,args)
            if self.fail==op:raise VqroError('unknown native effect','indeterminate')
            if self.reject==op:return {'outcome':'rejected','error':{'kind':'stale','message':'native precondition rejected'}}
            if op=='_account.switch':self.selected[args['resource']]=args['args']['args']['account']['id']
            ident=self.selected[args['resource']]
            account={'id':ident,'label':ident.upper(),'kind':'subscription','rev':1,'native':{'login_ref':{'sessionId':'saved-'+ident,'accountId':'workspace-'+ident}}}
            if op in ('_account.resume','_account.restore') and not self.skip_publication:
                target=next(r for r in self.rows if r['id']==args['resource'])
                target['data']['agent'].update(epoch='resumed-'+target['id'],account=account)
                target['data']['agent']['_codex']['input_ledger']['epoch']='resumed-'+target['id']
            if op=='_account.release':
                target=next(r for r in self.rows if r['id']==args['resource'])
                send=target['data']['agent']['caps'].setdefault('agent.send',{})
                if self.quota.get(ident) is True:send['native_included_usage']=True
                else:send.pop('native_included_usage',None)
            return {'outcome':'observed','data':{'quiescent':True,'operation':args['args']['args'].get('operation'),
                'account_confirmed':True,'resumed':True,'released':self.released,'aborted':True,'account':account,
                'prepared_ticket':'prepared-'+args['resource']+'-'+str(self.counter),
                'admissions':[{'session':'thread-'+args['resource'],'epoch':'resumed-'+args['resource'],'caps_rev':1,'expires_at_ms':int(time.time()*1000)+30000}] if op=='_account.release' and self.quota.get(ident) is True else [],
                'ordinary_usage_allowed':self.quota.get(ident),'observed_at_ms':int(time.time()*1000),'reset_at_ms':None,'source':'codex.account/rateLimits',
                'logins':[] if args['resource']==self.missing_on else [
                    {'sessionId':'saved-a','accountId':'workspace-a','active':ident=='a' and not self.all_active},
                    {'sessionId':'saved-b','accountId':'workspace-b','active':ident=='b' or self.all_active},
                    {'sessionId':'saved-c','accountId':'workspace-c','active':ident=='c'}]}}
        raise AssertionError((method,args))

def synthetic_native_reject():
    # Drive the real public SDK parser, with a local decoded transport reply only.
    rpc=Vqro.__new__(Vqro);rpc.lock=threading.Lock();rpc.waiting={};rpc.next=1;rpc.patient=False
    def send(message):
        slot=rpc.waiting[message['id']]
        slot[1]={'error':{'code':-32000,'message':'prepared auth revision changed','data':{'kind':'stale','outcome':'rejected'}}}
        slot[0].set()
    rpc._send=send
    return rpc.call('resource.invoke',{'op':'agent.command'})

class Tests(unittest.TestCase):
    def args(self):return {'resource':'x','episode':episode(row('x','execution','native-x')),'account':'b','_account_revision':3,'automatic':True}
    def claimed(self,ext,prepare,automatic=True):
        args=self.args();common=args['episode'];args.update(episode='native-turn',_claim='opaque-claim',automatic=automatic)
        ext.claim=dict(prepare,state='claimed',resource='x',host='execution',episode=common,native_episode='native-turn',
            source_incarnation='inc-x',source_epoch='epoch-x',source_account='a',session='thread-x',
            account='b',authorized_targets=['b'],_account_revision=3,automatic=automatic,runtime='codex',version=1,
            authorization={'mode':'auto','principal':None,'owner_revision':None} if automatic else {'mode':'offer','principal':'human-owner','owner_revision':5})
        return args
    def test_one_setup_unknown_target_inspection_settles_ready_offer_or_all_blocked(self):
        for permission,disposition in ((True,'ready'),(None,'offer'),(False,'all_blocked')):
            with self.subTest(permission=permission):
                ext=Ext();ext.rows.pop();ext.quota['b']=permission;c=Cutover(ext)
                p=c.prepare(self.args());result=c.cutover(self.claimed(ext,p))
                self.assertEqual(result['disposition'],disposition);self.assertEqual(result['continuation_allowed'],permission is True)
                self.assertEqual(result['attempted_targets'],['b']);self.assertFalse(result['fenced'])
                self.assertEqual(len(result['inspected_blocked']),1 if permission is False else 0)
                names=[a['args']['name'] for m,a in ext.calls if m=='resource.invoke']
                self.assertLess(max(i for i,n in enumerate(names) if n=='_account.inspect'),names.index('_account.resume'))
                self.assertEqual(names.count('_account.resume'),2);self.assertEqual(names.count('_account.release'),2)
                self.assertFalse(any(a.get('op')=='agent.send' for m,a in ext.calls if m=='resource.invoke'))
    def test_ordered_authorized_walk_b_blocked_then_c_ready_and_unknown_stops(self):
        for permission in (False,None):
            ext=Ext();ext.rows.pop();ext.registry['accounts'].append({'id':'c','label':'C','runtime':'codex','kind':'subscription','login_ref':{'sessionId':'saved-c','accountId':'workspace-c'}})
            ext.quota['b']=permission;c=Cutover(ext);p=c.prepare(self.args());args=self.claimed(ext,p);ext.claim['authorized_targets']=['b','c']
            result=c.cutover(args)
            self.assertEqual(result['attempted_targets'],['b','c'] if permission is False else ['b'])
            self.assertEqual(result['account'],'c' if permission is False else 'b')
            self.assertEqual(result['disposition'],'ready' if permission is False else 'offer')
            switches=[a['args']['args']['account']['id'] for m,a in ext.calls if m=='resource.invoke' and a['args']['name']=='_account.switch']
            self.assertEqual(switches,['b','b','c','c'] if permission is False else ['b','b'])
    def walk(self,quota=False):
        ext=Ext();ext.rows.pop();ext.registry['accounts'].append({'id':'c','label':'C','runtime':'codex','kind':'subscription','login_ref':{'sessionId':'saved-c','accountId':'workspace-c'}})
        ext.quota['b']=quota;c=Cutover(ext);p=c.prepare(self.args());args=self.claimed(ext,p);ext.claim['authorized_targets']=['b','c']
        return ext,c,args
    def test_reserved_prefix_bound_never_selects_unreserved_c(self):
        ext,c,args=self.walk();ext.claim['authorized_targets']=['b']
        result=c.cutover(args)
        self.assertEqual(result['disposition'],'all_blocked');self.assertEqual(result['attempted_targets'],['b'])
        self.assertFalse(any(m=='resource.invoke' and a['args']['name']=='_account.switch' and a['args']['args']['account']['id']=='c' for m,a in ext.calls))
    def test_malformed_or_source_alias_ordered_plans_have_no_native_effect(self):
        for ids in (None,[],['b','b'],['c','b'],['b','a'],['b','missing'],['b','c','a','missing']):
            ext,c,args=self.walk();ext.claim['authorized_targets']=ids
            with self.assertRaises(VqroError):c.cutover(args)
            self.assertFalse(any(m=='resource.invoke' for m,_ in ext.calls))
    def test_failed_quota_read_restores_known_b_and_offers_without_c(self):
        ext,c,args=self.walk();ext.fail='_account.inspect';result=c.cutover(args)
        self.assertEqual(result['disposition'],'offer');self.assertEqual(result['attempted_targets'],['b'])
        self.assertFalse(result['continuation_allowed']);self.assertFalse(result['fenced'])
        self.assertEqual(sum(m=='resource.invoke' and a['args']['name']=='_account.release' for m,a in ext.calls),2)
    def test_all_ordered_targets_blocked_restore_last_c_without_model(self):
        ext,c,args=self.walk();ext.quota['c']=False;result=c.cutover(args)
        self.assertEqual(result['disposition'],'all_blocked');self.assertEqual(result['account'],'c')
        self.assertEqual([r['account'] for r in result['inspected_blocked']],['b','c'])
        self.assertEqual(result['attempted_targets'],['b','c']);self.assertFalse(result['continuation_allowed']);self.assertFalse(result['fenced'])
    def test_quota_contradiction_is_offer_not_another_switch_or_auto_turn(self):
        ext,c,args=self.walk()
        def hook(op,args):
            if op=='_account.inspect':ext.quota['b']=args['resource']=='y'
        ext.hook=hook;result=c.cutover(args)
        self.assertEqual(result['disposition'],'offer');self.assertEqual(result['attempted_targets'],['b'])
        self.assertEqual([r['account'] for r in result['inspected_blocked']],['b'])
        self.assertFalse(result['continuation_allowed'])
    def test_revoked_authority_or_registry_during_inspection_stops_walk_and_restores(self):
        for changed in ('policy','registry','claim'):
            ext,c,args=self.walk()
            def hook(op,_):
                if op=='_account.inspect':
                    if changed=='policy':ext.consent={'setups':{}}
                    elif changed=='registry':ext.registry_revision=4
                    else:ext.claim['authorized_targets']=['b']
            ext.hook=hook;result=c.cutover(args)
            self.assertTrue(result['restored_only']);self.assertFalse(result['continuation_allowed'])
            self.assertEqual(result['attempted_targets'],['b']);self.assertFalse(result['fenced'])
            self.assertEqual([r['account'] for r in result['inspected_blocked']],['b'])
    def test_rejected_next_target_restores_known_lifecycle_and_does_not_charge_c(self):
        ext,c,args=self.walk()
        def hook(op,args):
            if op=='_account.switch' and args['args']['args']['account']['id']=='c':ext.reject=op
        ext.hook=hook;result=c.cutover(args)
        self.assertEqual(result['attempted_targets'],['b']);self.assertEqual(result['account'],'b')
        self.assertTrue(result['restored_only']);self.assertFalse(result['continuation_allowed']);self.assertFalse(result['fenced'])
    def test_uncertain_next_credential_effect_keeps_shared_fences_and_never_replays(self):
        ext,c,args=self.walk()
        def hook(op,args):
            if op=='_account.switch' and args['args']['args']['account']['id']=='c':ext.fail=op
        ext.hook=hook
        with self.assertRaises(VqroError):c.cutover(args)
        names=[a['args']['name'] for m,a in ext.calls if m=='resource.invoke']
        self.assertEqual(names.count('_account.switch'),3);self.assertNotIn('_account.resume',names);self.assertNotIn('_account.release',names)
    def test_common_live_budget_and_block_projection_stop_next_switch(self):
        for changed in ('budget','blocked'):
            ext,c,args=self.walk()
            def hook(op,_):
                if op=='_account.inspect':
                    if changed=='budget':ext.max_switches=1
                    else:ext.known_blocked=['c']
            ext.hook=hook;result=c.cutover(args)
            self.assertEqual(result['attempted_targets'],['b']);self.assertTrue(result['restored_only'])
            self.assertFalse(result['continuation_allowed']);self.assertFalse(result['fenced'])
    def test_unrelated_live_block_projection_does_not_mutate_frozen_plan(self):
        ext,c,args=self.walk(True)
        ext.hook=lambda op,_:setattr(ext,'known_blocked',['unrelated']) if op=='_account.switch' else None
        result=c.cutover(args)
        self.assertEqual(result['disposition'],'ready');self.assertTrue(result['continuation_allowed'])
    def test_common_expired_block_allows_target_despite_old_raw_blocked_fact(self):
        ext=Ext();ext.rows[-1]['data']['agent']['availability']={'state':'blocked','reason':'quota'}
        ext.known_blocked=[];c=Cutover(ext);p=c.prepare(self.args());result=c.cutover(self.claimed(ext,p))
        self.assertEqual(result['disposition'],'ready');self.assertEqual(result['attempted_targets'],['b'])
    def test_malformed_or_exhausted_common_live_permissions_have_no_native_effect(self):
        for maximum,blocked in ((True,[]),(None,[]),(-1,[]),(4,[]),(0,[]),(3,None),(3,[''])):
            ext,c,args=self.walk();ext.max_switches=maximum;ext.known_blocked=blocked
            with self.assertRaises(VqroError):c.cutover(args)
            self.assertFalse(any(m=='resource.invoke' for m,_ in ext.calls))
    def test_false_release_ack_never_reports_unfenced_and_processes_known_peer(self):
        ext,c,args=self.walk(True)
        def hook(op,args):
            if op=='_account.release':ext.released=args['resource']=='y'
        ext.hook=hook
        with self.assertRaises(VqroError):c.cutover(args)
        self.assertEqual([a['resource'] for m,a in ext.calls if m=='resource.invoke' and a['args']['name']=='_account.release'],['x','y'])
    def test_manual_offer_uses_owner_singleton_not_auto_budget_or_block_grant(self):
        ext=Ext();ext.max_switches=0;ext.known_blocked=['b'];ext.policy['blocked']='offer_restart'
        c=Cutover(ext);p=c.prepare(dict(self.args(),automatic=False));result=c.cutover(self.claimed(ext,p,automatic=False))
        self.assertEqual(result['attempted_targets'],['b']);self.assertEqual(result['account'],'b')
        ext,c,_=self.walk();ext.policy['blocked']='offer_restart';p=c.prepare(dict(self.args(),automatic=False));args=self.claimed(ext,p,automatic=False);ext.claim['authorized_targets']=['b','c']
        with self.assertRaises(VqroError):c.cutover(args)
        self.assertFalse(any(m=='resource.invoke' for m,_ in ext.calls))
    def test_registry_read_cannot_mix_old_allowance_with_new_positive_cap(self):
        ext,c,args=self.walk();original=ext.call;armed=[False];changed=[False]
        def call(method,arguments):
            if method=='workspace.get' and arguments['key']=='doc/settings/accounts' and armed[0] and not changed[0]:
                ext.max_switches=1;ext.policy['auto_cutover_max_per_hour']=1;changed[0]=True
            result=original(method,arguments)
            if method=='resource.invoke' and arguments['args']['name']=='_account.inspect' and arguments['resource']=='y':armed[0]=True
            return result
        ext.call=call;result=c.cutover(args)
        self.assertTrue(changed[0]);self.assertEqual(result['attempted_targets'],['b'])
        self.assertTrue(result['restored_only']);self.assertFalse(result['continuation_allowed'])
        self.assertFalse(any(m=='resource.invoke' and a['args']['name']=='_account.switch' and a['args']['args']['account']['id']=='c' for m,a in ext.calls))
    def registry_revocation_control(self,changed):
        ext,c,args=self.walk();original=ext.call;armed=[False];revoked=[False]
        def call(method,arguments):
            if method=='workspace.get' and arguments['key']=='doc/settings/accounts' and armed[0] and not revoked[0]:
                if changed=='consent':ext.consent={'setups':{}}
                else:ext.policy['blocked']='notify'
                revoked[0]=True
            result=original(method,arguments)
            if method=='resource.invoke' and arguments['args']['name']=='_account.inspect' and arguments['resource']=='y':armed[0]=True
            return result
        ext.call=call;result=c.cutover(args)
        self.assertTrue(revoked[0]);self.assertEqual(result['attempted_targets'],['b'])
        self.assertTrue(result['restored_only']);self.assertFalse(result['continuation_allowed'])
        self.assertFalse(any(m=='resource.invoke' and a['args']['name']=='_account.switch' and a['args']['args']['account']['id']=='c' for m,a in ext.calls))
    def test_registry_read_revocation_refreshes_private_consent_before_c(self):self.registry_revocation_control('consent')
    def test_registry_read_revocation_refreshes_policy_before_c(self):self.registry_revocation_control('policy')
    def test_quota_false_or_unknown_during_resume_restores_without_auto_permission(self):
        for value in (False,None):
            ext,c,args=self.walk(True)
            ext.hook=lambda op,_:ext.quota.update(b=value) if op=='_account.resume' else None
            result=c.cutover(args)
            self.assertEqual(result['disposition'],'offer');self.assertFalse(result['continuation_allowed']);self.assertFalse(result['fenced'])
            self.assertTrue(all('admission_caps_rev' not in row for row in result['affected']))
    def test_missing_stale_or_ambiguous_release_permission_is_not_ready(self):
        for mode in ('missing','expired','wrong_epoch','duplicate','boolean'):
            ext,c,args=self.walk(True);original=ext.call
            def call(method,arguments):
                result=original(method,arguments)
                if method=='resource.invoke' and arguments['args']['name']=='_account.release':
                    proofs=result['data']['admissions']
                    if mode=='missing':result['data']['admissions']=[]
                    elif mode=='expired':proofs[0]['expires_at_ms']=0
                    elif mode=='wrong_epoch':proofs[0]['epoch']='foreign'
                    elif mode=='duplicate':proofs.append(copy.deepcopy(proofs[0]))
                    else:proofs[0]['caps_rev']=True
                return result
            ext.call=call;result=c.cutover(args)
            self.assertEqual(result['disposition'],'offer');self.assertFalse(result['continuation_allowed']);self.assertFalse(result['fenced'])
    def test_manual_auto_to_offer_live_mode_is_not_immutable_claim_staleness(self):
        ext=Ext();c=Cutover(ext);p=c.prepare(dict(self.args(),automatic=False));args=self.claimed(ext,p,automatic=False);original=ext.call
        def call(method,arguments):
            result=original(method,arguments)
            if method=='command.invoke' and arguments['name']=='accounts.claim.read':result['blocked_mode']=ext.policy['blocked']
            return result
        ext.call=call
        ext.hook=lambda op,_:ext.policy.update(blocked='offer_restart') if op=='_account.fence' else None
        result=c.cutover(args)
        self.assertEqual(result['outcome'],'observed');self.assertEqual(result['attempted_targets'],['b']);self.assertFalse(result['fenced'])
        self.assertFalse(any(m=='resource.invoke' and a.get('op')=='agent.send' for m,a in ext.calls))
    def test_unknown_next_preparation_restores_known_hosts_without_credential_retry(self):
        for failed_host in ('x','y'):
            with self.subTest(failed_host=failed_host):
                ext,c,args=self.walk()
                def hook(op,arguments):
                    if op=='_account.prepare_switch' and arguments['args']['args']['account']['id']=='c' and arguments['resource']==failed_host:
                        raise VqroError('unknown read-only preparation','indeterminate')
                ext.hook=hook;result=c.cutover(args)
                self.assertEqual(result['attempted_targets'],['b'] if failed_host=='x' else ['b','c'])
                self.assertEqual(result['disposition'],'offer');self.assertFalse(result['continuation_allowed']);self.assertFalse(result['fenced'])
                if failed_host=='x':self.assertFalse(result['restored_only']);self.assertEqual(result['account'],'b')
                names=[a['args']['name'] for m,a in ext.calls if m=='resource.invoke']
                self.assertEqual(names.count('_account.restore'),2);self.assertEqual(names.count('_account.release'),2)
                self.assertNotIn('_account.abort',names)
    def test_zero_effect_after_final_authority_refusal_requires_all_abort_acks(self):
        for uncertain_abort in (False,True):
            with self.subTest(uncertain_abort=uncertain_abort):
                ext=Ext();c=Cutover(ext);p=c.prepare(self.args());args=self.claimed(ext,p)
                def hook(op,_):
                    if op=='_account.prepare_switch':ext.consent={'setups':{}}
                    if uncertain_abort and op=='_account.abort':raise VqroError('unknown local abort','indeterminate')
                ext.hook=hook
                if uncertain_abort:
                    with self.assertRaises(VqroError):c.cutover(args)
                else:
                    result=c.cutover(args)
                    self.assertEqual(result['outcome'],'rejected');self.assertEqual(result['attempted_targets'],[])
                    self.assertFalse(result['fenced']);self.assertFalse(result['continuation_allowed'])
                names=[a['args']['name'] for m,a in ext.calls if m=='resource.invoke']
                self.assertNotIn('_account.switch',names);self.assertEqual(names.count('_account.abort'),2)
                self.assertNotIn('_account.resume',names)
    def test_first_readonly_prepare_failure_is_zero_only_after_literal_all_abort_acks(self):
        for ack in (True,False,1,None,'missing','unknown'):
            with self.subTest(ack=ack):
                ext,c,args=self.walk();ext.fail='_account.prepare_switch';original=ext.call
                def call(method,arguments):
                    if method=='resource.invoke' and arguments['args']['name']=='_account.abort' and arguments['resource']=='x' and ack=='unknown':raise VqroError('unknown abort','indeterminate')
                    result=original(method,arguments)
                    if method=='resource.invoke' and arguments['args']['name']=='_account.abort' and arguments['resource']=='x':
                        if ack=='missing':result['data'].pop('aborted')
                        else:result['data']['aborted']=ack
                    return result
                ext.call=call
                if ack is True:
                    result=c.cutover(args)
                    self.assertEqual(result['outcome'],'rejected');self.assertEqual(result['attempted_targets'],[]);self.assertFalse(result['fenced']);self.assertFalse(result['continuation_allowed'])
                else:
                    with self.assertRaises(VqroError):c.cutover(args)
                names=[a['args']['name'] for m,a in ext.calls if m=='resource.invoke']
                self.assertNotIn('_account.switch',names);self.assertNotIn('_account.resume',names);self.assertEqual(names.count('_account.abort'),1 if ack=='unknown' else 2)
    def test_authoritative_raised_stale_commit_rejection_restores_known_b(self):
        ext,c,args=self.walk()
        def hook(op,arguments):
            if op=='_account.switch' and arguments['args']['args']['account']['id']=='c':
                synthetic_native_reject()
        ext.hook=hook;result=c.cutover(args)
        self.assertEqual(result['attempted_targets'],['b']);self.assertEqual(result['account'],'b')
        self.assertTrue(result['restored_only']);self.assertFalse(result['fenced']);self.assertFalse(result['continuation_allowed'])
        names=[a['args']['name'] for m,a in ext.calls if m=='resource.invoke']
        self.assertEqual(names.count('_account.restore'),2);self.assertEqual(names.count('_account.release'),2)
        self.assertNotIn('_account.abort',names)
    def test_raised_kind_alone_unknown_or_malformed_commit_error_never_restores(self):
        for kind,data in (('stale',None),('stale',{}),('stale',{'outcome':'unknown'}),('stale',{'outcome':True}),('backend',{'outcome':'rejected'}),('indeterminate',{'outcome':'rejected'}),('stale',[]),('stale',{'outcome':'rejected','kind':'backend'}),('stale',{'outcome':'rejected'})):
            with self.subTest(kind=kind,data=data):
                ext,c,args=self.walk()
                def hook(op,arguments):
                    if op=='_account.switch' and arguments['args']['args']['account']['id']=='c':raise VqroError('unconfirmed native result',kind,data=data)
                ext.hook=hook
                with self.assertRaises(VqroError):c.cutover(args)
                names=[a['args']['name'] for m,a in ext.calls if m=='resource.invoke']
                self.assertNotIn('_account.restore',names);self.assertNotIn('_account.release',names);self.assertNotIn('_account.abort',names)
    def test_raised_rejected_marker_requires_prepared_commit_context(self):
        for name,payload in (('_account.status',{'prepared_ticket':'irrelevant'}),('_account.switch',{})):
            ext=Ext();c=Cutover(ext)
            def hook(op,args):raise VqroError('stale elsewhere','stale',data={'outcome':'rejected','kind':'stale'})
            ext.hook=hook
            with self.assertRaises(VqroError) as error:c._invoke(ext.rows[0],name,payload)
            self.assertIs(type(error.exception),VqroError)
    def test_prepare_is_readonly_and_native_episode_not_commonhash(self):
        ext=Ext();p=Cutover(ext).prepare(self.args())
        self.assertEqual(p['native_episode'],'native-turn');self.assertEqual(len(p['affected']),2)
        self.assertFalse(any(m=='resource.invoke' for m,_ in ext.calls))
        self.assertTrue(all(r['account']=='b' and r['host']=='execution' for r in p['affected']))
    def test_all_hosts_require_complete_empty_epoch_bound_own_ledger_in_auto_and_offer(self):
        for automatic in (True,False):
            for delta in ({'complete':False},{'pending':1},{'pending':True},{'unknown':1},{'epoch':'old'},{'source':'native'},{'v':True},None):
                with self.subTest(automatic=automatic,delta=delta):
                    ext=Ext();private=ext.rows[1]['data']['agent']['_codex']
                    if delta is None: private.clear()
                    else: private['input_ledger'].update(delta)
                    with self.assertRaises(VqroError) as error:Cutover(ext).prepare(dict(self.args(),automatic=automatic))
                    self.assertEqual(error.exception.kind,'busy')
                    self.assertFalse(any(m=='resource.invoke' for m,_ in ext.calls))
    def test_incomplete_own_ledger_required_cuts_and_malformed_markers_reject(self):
        cuts=[['_codex'],['_codex.input_ledger'],['_codex.input_ledger.pending'],['_codex.input_ledger[0]'],['epoch'],None,'_codex.input_ledger',[None],[''],['bad\u0000marker']]
        for automatic in (True,False):
            for markers in cuts:
                with self.subTest(automatic=automatic,markers=markers):
                    ext=Ext();ext.rows[1]['data']['agent']['incomplete']=markers
                    with self.assertRaises(VqroError) as error:Cutover(ext).prepare(dict(self.args(),automatic=automatic))
                    self.assertEqual(error.exception.kind,'busy')
                    self.assertFalse(any(m=='resource.invoke' for m,_ in ext.calls))
        for markers in ([],['config.model'],['_codex.other'],['_codex.input_ledger_extra']):
            ext=Ext();ext.rows[1]['data']['agent']['incomplete']=markers
            self.assertEqual(len(Cutover(ext).prepare(self.args())['affected']),2)
    def test_auto_requires_explicit_setup_consent_manual_offer_does_not(self):
        ext=Ext();ext.consent={'setups':{}};c=Cutover(ext)
        p=c.prepare(self.args());self.assertFalse(p['automatic_allowed'])
        result=c.cutover(self.claimed(ext,p))
        self.assertEqual(result['outcome'],'rejected');self.assertEqual(result['attempted_targets'],[])
        self.assertFalse(result['fenced']);self.assertEqual(result['reason'],'offer_required')
        self.assertFalse(any(m=='resource.invoke' for m,_ in ext.calls))
        p=c.prepare(dict(self.args(),automatic=False));self.assertFalse(p['automatic_allowed'])
    def test_group_fenced_before_switches_and_never_sends_continuation(self):
        ext=Ext();c=Cutover(ext);p=c.prepare(self.args());result=c.cutover(self.claimed(ext,p))
        self.assertEqual(result['outcome'],'observed');self.assertEqual(len(result['affected']),2)
        names=[a['args']['name'] for m,a in ext.calls if m=='resource.invoke']
        self.assertEqual(names,['_account.fence']*2+['_account.list']*2+['_account.prepare_switch','_account.switch']*2+['_account.inspect']*2+['_account.resume']*2+['_account.release']*2)
        self.assertFalse(any(a.get('op')=='agent.send' for m,a in ext.calls if m=='resource.invoke'))
    def test_changed_group_or_claim_refuses_before_effect(self):
        for field in ['membership_revision','account','native_episode','state']:
            ext=Ext();c=Cutover(ext);p=c.prepare(self.args());args=self.claimed(ext,p);ext.claim[field]='changed'
            with self.assertRaises(VqroError):c.cutover(args)
            self.assertFalse(any(m=='resource.invoke' for m,_ in ext.calls))
    def test_unknown_switch_consumes_intent_no_replay_no_release_or_send(self):
        ext=Ext();c=Cutover(ext);p=c.prepare(self.args());args=self.claimed(ext,p);ext.fail='_account.switch'
        with self.assertRaises(VqroError):c.cutover(args)
        names=[a['args']['name'] for m,a in ext.calls if m=='resource.invoke']
        self.assertEqual(names,['_account.fence']*2+['_account.list']*2+['_account.prepare_switch','_account.switch'])
    def test_prepare_waits_for_all_running_siblings_without_claim_or_fence(self):
        ext=Ext();ext.rows[1]['data']['agent'].update(state='running',run='other-turn')
        with self.assertRaises(VqroError) as error:Cutover(ext).prepare(self.args())
        self.assertEqual(error.exception.kind,'busy')
        self.assertFalse(any(m in ('resource.invoke','command.invoke') for m,_ in ext.calls))
    def test_duplicate_native_conversation_writers_are_refused(self):
        ext=Ext();ext.rows[1]['data']['agent']['session']={'id':'thread-x'}
        with self.assertRaises(VqroError):Cutover(ext).prepare(self.args())
        self.assertFalse(any(m=='resource.invoke' for m,_ in ext.calls))
    def test_malformed_consent_never_enables_auto(self):
        for value in [None, [], {'setups':[]}, {'setups':{'shared':True}}]:
            ext=Ext();ext.consent=value
            self.assertFalse(Cutover(ext).prepare(self.args())['automatic_allowed'])
    def test_manual_effect_requires_current_recorded_human_authority(self):
        for changed in [False,True]:
            ext=Ext();c=Cutover(ext);p=c.prepare(dict(self.args(),automatic=False));args=self.claimed(ext,p,automatic=False)
            if changed:ext.owner['version']=6
            else:ext.claim['authorization']=None
            with self.assertRaises(VqroError):c.cutover(args)
            self.assertFalse(any(m=='resource.invoke' for m,_ in ext.calls))
    def test_valid_human_offer_can_resume_without_auto_consent_or_continuation(self):
        ext=Ext();ext.consent={'setups':{}};c=Cutover(ext)
        p=c.prepare(dict(self.args(),automatic=False));result=c.cutover(self.claimed(ext,p,automatic=False))
        self.assertEqual(result['outcome'],'observed')
        self.assertFalse(any(a.get('op')=='agent.send' for m,a in ext.calls if m=='resource.invoke'))
    def test_unverified_or_mismatched_native_binding_cannot_supply_target_headroom(self):
        for mode in ('hint','wrong_ref'):
            ext=Ext();account=ext.rows[-1]['data']['agent']['account']
            if mode=='hint':account.pop('native');account.pop('rev')
            else:account['native']['login_ref']['sessionId']='another-login'
            ext.quota['b']=None;c=Cutover(ext);p=c.prepare(self.args())
            result=c.cutover(self.claimed(ext,p))
            self.assertEqual(result['disposition'],'offer');self.assertFalse(result['continuation_allowed'])
            self.assertEqual(sum(m=='resource.invoke' and a['args']['name']=='_account.inspect' for m,a in ext.calls),2)
    def test_unknown_target_may_switch_but_current_blocked_target_is_rejected(self):
        ext=Ext();ext.rows.pop();ext.quota['b']=None;c=Cutover(ext);p=c.prepare(self.args())
        self.assertEqual(c.cutover(self.claimed(ext,p))['disposition'],'offer')
        ext=Ext();ext.rows[-1]['data']['agent']['availability']={'state':'blocked','reason':'quota'};ext.known_blocked=['b']
        c=Cutover(ext);p=c.prepare(self.args());args=self.claimed(ext,p)
        with self.assertRaises(VqroError):c.cutover(args)
        self.assertFalse(any(m=='resource.invoke' for m,_ in ext.calls))
    def test_post_fence_target_change_aborts_only_local_fences_no_switch(self):
        ext=Ext();c=Cutover(ext);p=c.prepare(self.args());args=self.claimed(ext,p)
        ext.hook=lambda op,_:setattr(ext,'registry_revision',4) if op=='_account.fence' else None
        with self.assertRaises(VqroError):c.cutover(args)
        names=[a['args']['name'] for m,a in ext.calls if m=='resource.invoke']
        self.assertEqual(names,['_account.fence']*2+['_account.abort']*2)
    def test_unconfirmed_core_publication_does_not_authorize_continuation(self):
        ext=Ext();c=Cutover(ext);p=c.prepare(self.args());args=self.claimed(ext,p);ext.skip_publication=True
        with patch('codex_cutover.time.monotonic',side_effect=itertools.count(0,10)):
            with self.assertRaises(VqroError) as error:c.cutover(args)
        self.assertEqual(error.exception.kind,'indeterminate')
        self.assertFalse(any(a.get('op')=='agent.send' for m,a in ext.calls if m=='resource.invoke'))
    def test_uncooperative_group_join_is_partial_not_observed_success(self):
        ext=Ext();c=Cutover(ext);p=c.prepare(self.args());args=self.claimed(ext,p)
        def join(op,_):
            if op=='_account.resume' and not any(r['id']=='late' for r in ext.rows):ext.rows.append(row('late','execution','native-late'))
        ext.hook=join
        with self.assertRaises(VqroError) as error:c.cutover(args)
        self.assertEqual(error.exception.kind,'indeterminate')
    def test_unavailable_source_or_missing_native_command_refuses_before_claim(self):
        for change in ['cached','commands']:
            ext=Ext()
            if change=='cached':ext.rows[0]['available']=False
            else:ext.rows[1]['data']['agent']['commands']=[]
            with self.assertRaises(VqroError):Cutover(ext).prepare(self.args())
            self.assertFalse(any(m=='resource.invoke' for m,_ in ext.calls))
    def test_known_first_switch_rejection_aborts_local_fences_but_never_replays(self):
        ext=Ext();c=Cutover(ext);p=c.prepare(self.args());args=self.claimed(ext,p);ext.reject='_account.switch'
        result=c.cutover(args)
        self.assertEqual(result['outcome'],'rejected');self.assertEqual(result['attempted_targets'],[]);self.assertFalse(result['fenced'])
        names=[a['args']['name'] for m,a in ext.calls if m=='resource.invoke']
        self.assertEqual(names,['_account.fence']*2+['_account.list']*2+['_account.prepare_switch','_account.switch']+['_account.abort']*2)
    def test_target_discovered_on_every_host_before_any_switch(self):
        ext=Ext();c=Cutover(ext);p=c.prepare(self.args());args=self.claimed(ext,p);ext.missing_on='y'
        with self.assertRaises(VqroError):c.cutover(args)
        names=[a['args']['name'] for m,a in ext.calls if m=='resource.invoke']
        self.assertEqual(names,['_account.fence']*2+['_account.list']*2+['_account.abort']*2)
    def test_auto_refuses_native_target_already_active_everywhere(self):
        ext=Ext();c=Cutover(ext);p=c.prepare(self.args());args=self.claimed(ext,p);ext.all_active=True
        with self.assertRaises(VqroError):c.cutover(args)
        self.assertFalse(any(a['args']['name']=='_account.switch' for m,a in ext.calls if m=='resource.invoke'))
    def test_known_source_credential_alias_is_not_a_different_target(self):
        ext=Ext();alias=copy.deepcopy(ext.registry['accounts'][0]);alias.update(id='a',label='A alias')
        ext.registry['accounts'][1]=alias
        with self.assertRaises(VqroError):Cutover(ext).prepare(self.args())
    def test_incomplete_target_route_is_visible_but_cannot_be_selected(self):
        ext=Ext();ext.registry['accounts'][0]['host']='unpaired-host'
        with self.assertRaises(VqroError):Cutover(ext).prepare(self.args())
    def test_alias_missing_capacity_cannot_invent_positive_native_permission(self):
        ext=Ext();alias=copy.deepcopy(ext.registry['accounts'][0]);alias.update(id='b-alias',label='B alias')
        ext.registry['accounts'].append(alias)
        missing=copy.deepcopy(ext.rows[-1]);missing.update(id='alias-observer',incarnation='alias-inc')
        missing['data']['agent'].update(account={'id':'b-alias'},usage_limits=None)
        ext.rows.append(missing);ext.quota['b']=None
        c=Cutover(ext);p=c.prepare(self.args());result=c.cutover(self.claimed(ext,p))
        self.assertEqual(result['disposition'],'offer');self.assertFalse(result['continuation_allowed'])
        ext=Ext();ext.registry['accounts'].append(alias);c=Cutover(ext);p=c.prepare(self.args());args=self.claimed(ext,p)
        ext.claim['authorized_targets']=['b','b-alias']
        with self.assertRaises(VqroError):c.cutover(args)
        self.assertFalse(any(m=='resource.invoke' for m,_ in ext.calls))
    def test_notify_policy_revocation_prevents_any_native_restart(self):
        ext=Ext();c=Cutover(ext);p=c.prepare(self.args());args=self.claimed(ext,p);ext.policy['blocked']='notify'
        result=c.cutover(args)
        self.assertEqual(result['outcome'],'rejected');self.assertEqual(result['attempted_targets'],[]);self.assertFalse(result['fenced'])
        self.assertFalse(any(m=='resource.invoke' for m,_ in ext.calls))
    def test_policy_revocation_during_readonly_native_discovery_aborts_before_switch(self):
        ext=Ext();c=Cutover(ext);p=c.prepare(self.args());args=self.claimed(ext,p)
        def revoke(op,_):
            if op=='_account.list':ext.policy['blocked']='notify'
        ext.hook=revoke
        with self.assertRaises(VqroError):c.cutover(args)
        names=[a['args']['name'] for m,a in ext.calls if m=='resource.invoke']
        self.assertEqual(names,['_account.fence']*2+['_account.list']*2+['_account.abort']*2)
    def test_consent_revocation_between_switches_restores_b_and_a_without_rollback_or_continuation(self):
        ext=Ext();c=Cutover(ext);p=c.prepare(self.args());args=self.claimed(ext,p)
        def revoke(op,_):
            if op=='_account.switch':ext.consent={'setups':{}}
        ext.hook=revoke
        result=c.cutover(args)
        self.assertTrue(result['restored_only']);self.assertFalse(result['continuation_allowed'])
        self.assertEqual([r['account'] for r in result['affected']],['b','a'])
        names=[a['args']['name'] for m,a in ext.calls if m=='resource.invoke']
        self.assertEqual(names,['_account.fence']*2+['_account.list']*2+['_account.prepare_switch','_account.switch','_account.prepare_switch']+['_account.restore']*2+['_account.release']*2)
        self.assertFalse(any(a.get('op')=='agent.send' for m,a in ext.calls if m=='resource.invoke'))
    def test_consent_revocation_after_last_switch_restores_all_b_without_continuation(self):
        ext=Ext();c=Cutover(ext);p=c.prepare(self.args());args=self.claimed(ext,p);switched=[]
        def revoke(op,_):
            if op=='_account.switch':
                switched.append(op)
                if len(switched)==2:ext.consent={'setups':{}}
        ext.hook=revoke
        result=c.cutover(args)
        self.assertTrue(result['restored_only']);self.assertFalse(result['continuation_allowed'])
        self.assertEqual([r['account'] for r in result['affected']],['b','b'])
        names=[a['args']['name'] for m,a in ext.calls if m=='resource.invoke']
        self.assertEqual(names,['_account.fence']*2+['_account.list']*2+['_account.prepare_switch','_account.switch']*2+['_account.restore']*2+['_account.release']*2)
        self.assertFalse(any(a.get('op')=='agent.send' for m,a in ext.calls if m=='resource.invoke'))
    def test_resume_failure_after_revocation_releases_known_peer_without_retrying_unknown(self):
        ext=Ext();c=Cutover(ext);p=c.prepare(self.args());args=self.claimed(ext,p)
        def revoke_and_fail(op,args):
            if op=='_account.resume' and args['resource']=='x':ext.consent={'setups':{}}
            if op=='_account.resume' and args['resource']=='y':raise VqroError('unknown resume','indeterminate')
        ext.hook=revoke_and_fail
        with self.assertRaises(VqroError):c.cutover(args)
        calls=[a for m,a in ext.calls if m=='resource.invoke']
        self.assertEqual([a['resource'] for a in calls if a['args']['name']=='_account.resume'],['x','y'])
        self.assertEqual([a['resource'] for a in calls if a['args']['name']=='_account.release'],['x'])
        self.assertTrue(next(a for a in calls if a['args']['name']=='_account.release')['args']['args']['revoked'])
        self.assertFalse(any(a.get('op')=='agent.send' for a in calls))
    def test_release_failure_still_releases_other_known_peer_once(self):
        ext=Ext();c=Cutover(ext);p=c.prepare(self.args());args=self.claimed(ext,p)
        def revoke_and_fail(op,args):
            if op=='_account.release' and args['resource']=='x':
                ext.consent={'setups':{}};raise VqroError('unknown release','indeterminate')
        ext.hook=revoke_and_fail
        with self.assertRaises(VqroError):c.cutover(args)
        calls=[a for m,a in ext.calls if m=='resource.invoke']
        self.assertEqual([a['resource'] for a in calls if a['args']['name']=='_account.resume'],['x','y'])
        self.assertEqual([a['resource'] for a in calls if a['args']['name']=='_account.release'],['x','y'])
        self.assertTrue([a for a in calls if a['args']['name']=='_account.release'][-1]['args']['args']['revoked'])
        self.assertFalse(any(a.get('op')=='agent.send' for a in calls))
    def test_unknown_restore_keeps_uncertainty_but_releases_other_confirmed_host(self):
        ext=Ext();c=Cutover(ext);p=c.prepare(self.args());args=self.claimed(ext,p)
        def revoke_and_fail(op,args):
            if op=='_account.switch':ext.consent={'setups':{}}
            if op=='_account.restore' and args['resource']=='x':raise VqroError('uncertain restore','indeterminate')
        ext.hook=revoke_and_fail
        with self.assertRaises(VqroError) as error:c.cutover(args)
        self.assertEqual(error.exception.kind,'indeterminate')
        calls=[a for m,a in ext.calls if m=='resource.invoke']
        self.assertEqual(sum(a['args']['name']=='_account.switch' for a in calls),1)
        self.assertEqual(sum(a['args']['name']=='_account.restore' for a in calls),2)
        self.assertEqual([a['resource'] for a in calls if a['args']['name']=='_account.release'],['y'])
        self.assertFalse(any(a.get('op')=='agent.send' for a in calls))
    def test_source_native_thread_witness_required(self):
        ext=Ext();ext.rows[0]['data']['agent']['availability']['native']['threadId']='foreign'
        with self.assertRaises(VqroError):Cutover(ext).prepare(self.args())
if __name__=='__main__':unittest.main()
