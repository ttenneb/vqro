import copy
import importlib.util
import pathlib
import sys
import unittest
import time
import itertools
from unittest.mock import patch
ROOT=pathlib.Path(__file__).resolve().parents[1]
sys.path.insert(0,str(ROOT/'extensions'))
from vqro_ext import VqroError
from codex_cutover import Cutover, SETTINGS, HOST_COMMANDS
from accounts_policy import episode


def row(ident, host, native_host, state='idle'):
    return {'id':ident,'available':True,'host':host,'incarnation':'inc-'+ident,'status':{'app':'codex'},'data':{'agent':{
        'v':0,'lifecycle':'live','state':state,'run':None if state=='idle' else 'running','epoch':'epoch-'+ident,
        'host':{'id':native_host},'session':{'id':'thread-'+ident},'account_setup':{'id':'shared'},
        'account':{'id':'a','kind':'subscription'},'caps':{'rev':1,'agent.command':{},'agent.availability':{'reasons':['quota','rate_limited']}},'commands':[{'name':n} for n in HOST_COMMANDS],'queue':None,'requests':[], 'background':None,
        'availability':{'state':'blocked','reason':'quota','native':{'threadId':'thread-'+ident,'turnId':'native-turn',
            'codexErrorInfo':'usageLimitExceeded','willRetry':False}}}}}

class Ext:
    def __init__(self):
        self.vqro=self;self.rows=[row('x','execution','native-x'),row('y','execution','native-y')];self.calls=[]
        probe=row('probe','execution','native-probe');p=probe['data']['agent']
        p.update(account_setup={'id':'other-setup'},account={'id':'b'},availability={'state':'available'},incomplete=[])
        now=int(time.time()*1000)
        p['usage_limits']=[{'scope':{'kind':'account','account':'b'},'window':{'id':'primary'},'used_percent':10,
            'source':'codex.account/rateLimits','observed_at_ms':now,'resets_at_ms':now+60000}]
        self.rows.append(probe)
        self.registry={'accounts':[{'id':'b','label':'B','runtime':'codex','kind':'subscription',
            'login_ref':{'sessionId':'saved-b','accountId':'workspace-b'}}]}
        self.consent={'setups':{'shared':{'automatic_login_switch':True}}};self.claim=None;self.fail=None;self.counter=0
        self.owner={'value':'human-owner','version':5}
        self.registry_revision=3;self.hook=None;self.skip_publication=False;self.reject=None
        self.all_active=False;self.missing_on=None
    def command(self,*_):return lambda fn:fn
    def call(self,method,args):
        self.calls.append((method,copy.deepcopy(args)))
        if method=='resource.list':return {'resources':copy.deepcopy(self.rows)}
        if method=='workspace.get':
            if args['key']=='doc/principals/owner':return copy.deepcopy(self.owner)
            return {'version':3 if args['key']==SETTINGS else self.registry_revision,'value':copy.deepcopy(self.consent if args['key']==SETTINGS else self.registry)}
        if method=='command.invoke':
            assert args['name']=='accounts.claim.read'
            return copy.deepcopy(self.claim)
        if method=='resource.invoke':
            op=args['args']['name'];self.counter+=1
            if self.hook:self.hook(op,args)
            if self.fail==op:raise VqroError('unknown native effect','indeterminate')
            if self.reject==op:return {'outcome':'rejected','error':{'kind':'stale','message':'native precondition rejected'}}
            if op=='_account.resume' and not self.skip_publication:
                target=next(r for r in self.rows if r['id']==args['resource'])
                target['data']['agent'].update(epoch='resumed-'+target['id'],account={'id':'b','rev':1})
            return {'outcome':'observed','data':{'quiescent':True,'operation':args['args']['args'].get('operation'),
                'account_confirmed':True,'resumed':True,
                'logins':[] if args['resource']==self.missing_on else [{'sessionId':'saved-b','accountId':'workspace-b','active':self.all_active}]}}
        raise AssertionError((method,args))

class Tests(unittest.TestCase):
    def args(self):return {'resource':'x','episode':episode(row('x','execution','native-x')),'account':'b','_account_revision':3,'automatic':True}
    def claimed(self,ext,prepare,automatic=True):
        args=self.args();common=args['episode'];args.update(episode='native-turn',_claim='opaque-claim',automatic=automatic)
        ext.claim=dict(prepare,state='claimed',resource='x',host='execution',episode=common,native_episode='native-turn',
            source_incarnation='inc-x',source_epoch='epoch-x',source_account='a',session='thread-x',
            account='b',_account_revision=3,automatic=automatic,runtime='codex',version=1,
            authorization={'mode':'auto','principal':None,'owner_revision':None} if automatic else {'mode':'offer','principal':'human-owner','owner_revision':5})
        return args
    def test_prepare_is_readonly_and_native_episode_not_commonhash(self):
        ext=Ext();p=Cutover(ext).prepare(self.args())
        self.assertEqual(p['native_episode'],'native-turn');self.assertEqual(len(p['affected']),2)
        self.assertFalse(any(m=='resource.invoke' for m,_ in ext.calls))
        self.assertTrue(all(r['account']=='b' and r['host']=='execution' for r in p['affected']))
    def test_auto_requires_explicit_setup_consent_manual_offer_does_not(self):
        ext=Ext();ext.consent={'setups':{}}
        with self.assertRaises(VqroError):Cutover(ext).prepare(self.args())
        p=Cutover(ext).prepare(dict(self.args(),automatic=False));self.assertFalse(p['automatic_allowed'])
    def test_group_fenced_before_switches_and_never_sends_continuation(self):
        ext=Ext();c=Cutover(ext);p=c.prepare(self.args());result=c.cutover(self.claimed(ext,p))
        self.assertEqual(result['outcome'],'observed');self.assertEqual(len(result['affected']),2)
        names=[a['args']['name'] for m,a in ext.calls if m=='resource.invoke']
        self.assertEqual(names,['_account.fence']*2+['_account.list']*2+['_account.switch']*2+['_account.resume']*2+['_account.release']*2)
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
        self.assertEqual(names,['_account.fence']*2+['_account.list']*2+['_account.switch'])
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
            with self.assertRaises(VqroError):Cutover(ext).prepare(self.args())
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
    def test_unknown_or_blocked_target_is_never_auto_switched(self):
        for mode in ['unknown','blocked']:
            ext=Ext()
            if mode=='unknown':ext.rows.pop()
            else:ext.rows[-1]['data']['agent']['availability']={'state':'blocked','reason':'quota'}
            with self.assertRaises(VqroError):Cutover(ext).prepare(self.args())
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
        with self.assertRaises(VqroError):c.cutover(args)
        names=[a['args']['name'] for m,a in ext.calls if m=='resource.invoke']
        self.assertEqual(names,['_account.fence']*2+['_account.list']*2+['_account.switch']+['_account.abort']*2)
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
        ext.registry['accounts'].append(alias)
        with self.assertRaises(VqroError):Cutover(ext).prepare(self.args())
    def test_incomplete_target_route_is_visible_but_cannot_be_selected(self):
        ext=Ext();ext.registry['accounts'][0]['host']='unpaired-host'
        with self.assertRaises(VqroError):Cutover(ext).prepare(self.args())
    def test_alias_missing_capacity_poisoning_is_preserved_at_native_boundary(self):
        ext=Ext();alias=copy.deepcopy(ext.registry['accounts'][0]);alias.update(id='b-alias',label='B alias')
        ext.registry['accounts'].append(alias)
        missing=copy.deepcopy(ext.rows[-1]);missing.update(id='alias-observer',incarnation='alias-inc')
        missing['data']['agent'].update(account={'id':'b-alias'},usage_limits=None)
        ext.rows.append(missing)
        with self.assertRaises(VqroError):Cutover(ext).prepare(self.args())
    def test_source_native_thread_witness_required(self):
        ext=Ext();ext.rows[0]['data']['agent']['availability']['native']['threadId']='foreign'
        with self.assertRaises(VqroError):Cutover(ext).prepare(self.args())
if __name__=='__main__':unittest.main()
