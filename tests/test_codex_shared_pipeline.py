"""Real Accounts/Dispatch/Cutover integration, synthetic native operations only."""
import copy
import unittest
from unittest.mock import patch
from test_account_cutover_claims import Core as JournalCore
from accounts import Accounts
from accounts_policy import POLICY, CONTINUATION_TEXT
from account_credentials import SETTINGS as REGISTRY
from codex_cutover import Cutover, SETTINGS as CONSENT
from test_codex_cutover import Ext as Native
from test_account_capacity import NOW
from vqro_ext import VqroError
from public_provider_error import provider_error


class Pipeline(JournalCore):
    def __init__(self, permission=True, next_target=False):
        super().__init__()
        self.native=Native();self.native.rows.pop()
        self.native.quota['b']=permission
        if next_target:
            self.native.registry['accounts'].append({'id':'c','label':'C','runtime':'codex','kind':'subscription',
                'login_ref':{'sessionId':'saved-c','accountId':'workspace-c'}})
        self.resources={row['id']:row for row in self.native.rows}
        for row in self.resources.values():
            row.update(kind='agent',state='live')
            row['data']['agent']['caps']['agent.send']={'modes':{'admit':{}},'origin':[{'as':'attributed_user'}]}
        self.entries[REGISTRY]={'version':3,'value':self.native.registry}
        self.entries[CONSENT]={'version':3,'value':self.native.consent}
        self.entries[POLICY]={'version':4,'value':self.native.policy}
        self.entries['doc/principals/owner']={'version':5,'value':'human-owner'}
        self.caller_record={'principal':'human-owner','initiator':'human-owner'}
        self.sends=[]
        self.codex=Cutover(self)
        self.app=Accounts(self)

    def invoke(self,name,args=None):
        if name in self.handlers:return self.handlers[name](args or {})
        return super().invoke(name,args)

    def call(self,method,params=None):
        params=params or {}
        if method=='command.invoke':return self.invoke(params['name'],params.get('args'))
        if method=='resource.invoke':
            if params['op']=='agent.command':
                result=self.native.call(method,params)
                if params['args']['name'] in ('_account.resume','_account.restore'):
                    self.resources[params['resource']]['data']['agent']['availability']=None
                return result
            if params['op']=='agent.send':
                args=params['args'];native=self.resources[params['resource']]['data']['agent']
                if (args['caps_rev']!=native['caps']['rev'] or args['epoch']!=native['epoch']
                        or args.get('guard',{}).get('session')!=native['session']['id']):
                    return provider_error({'outcome':'rejected','kind':'stale'},method,params,
                                          message='capability revision changed')
                self.sends.append(copy.deepcopy(params));return {'outcome':'accepted'}
        return super().call(method,params)


class SharedPipeline(unittest.TestCase):
    def setUp(self):
        clock=patch('accounts_policy.now_ms',return_value=NOW);clock.start();self.addCleanup(clock.stop)
        native_time=patch('codex_cutover.time.time',return_value=NOW/1000);native_time.start();self.addCleanup(native_time.stop)

    def run_case(self,permission=True,next_target=False):
        core=Pipeline(permission,next_target)
        core.app.handle_blocked('x')
        return core

    def test_quota_changes_at_actual_send_guard_then_owner_gets_one_new_delivery(self):
        core=Pipeline(True);call=core.call;changed=[]
        def at_guard(method,params=None):
            if method=='resource.invoke' and params.get('op')=='agent.send' and not changed:
                changed.append(True);caps=core.resources['x']['data']['agent']['caps']
                caps['rev']+=1;caps['agent.send'].pop('native_included_usage')
            return call(method,params)
        core.call=at_guard;core.app.handle_blocked('x')
        token,record=next((token,record) for token,record in core.op_entry['value']['claims'].items()
                          if record.get('continuation_parent') is None)
        self.assertEqual(core.sends,[]);self.assertEqual(record['state'],'awaiting_owner')
        rejected=record['continuations']['thread-x']['rejected_delivery']
        switches=len([1 for _,args in core.native.calls if args.get('op')=='agent.command'
                      and args['args']['name']=='_account.switch'])
        core.app.confirm_continuation({'claim':token})
        self.assertEqual(len(core.sends),1);self.assertNotEqual(core.sends[0]['args']['delivery'],rejected)
        self.assertEqual(switches,len([1 for _,args in core.native.calls if args.get('op')=='agent.command'
                                     and args['args']['name']=='_account.switch']))
        with self.assertRaises(VqroError):core.app.confirm_continuation({'claim':token})
        self.assertEqual(len(core.sends),1)

    def test_ready_real_worker_claim_effect_and_one_labelled_turn(self):
        core=self.run_case()
        self.assertEqual(len(core.sends),1)
        send=core.sends[0]
        self.assertEqual(send['resource'],'x');self.assertEqual(send['args']['mode'],'admit')
        self.assertEqual(send['args']['guard'],{'session':'thread-x'})
        self.assertEqual(send['args']['origin']['kind'],'tool')
        self.assertEqual(send['args']['content'],[{'type':'text','text':CONTINUATION_TEXT}])
        self.assertTrue(core.app.cutovers.known_blocked(core.native.registry['accounts'][1]))

    def test_unknown_real_worker_restores_and_offers_without_send(self):
        core=self.run_case(None)
        self.assertEqual(core.sends,[])
        self.assertIn('accounts-continuation',core.live)
        self.assertEqual(sum(m=='resource.invoke' and a['args']['name']=='_account.release' for m,a in core.native.calls),2)

    def test_all_blocked_real_worker_restores_and_notifies_without_send(self):
        core=self.run_case(False)
        self.assertEqual(core.sends,[])
        self.assertTrue(any('All accounts are at their limit' in message for message,_ in core.notices))

    def test_unknown_owner_click_is_same_claim_one_new_turn_without_another_switch(self):
        core=self.run_case(None);click=core.live['accounts-continuation']['items'][-1]['args']
        claims=set(core.op_entry['value']['claims']);effects=len(core.native.calls)
        core.invoke('accounts.continuation.confirm',click)
        self.assertEqual(len(core.sends),1);self.assertEqual(set(core.op_entry['value']['claims']),claims)
        self.assertEqual(len(core.native.calls),effects)
        with self.assertRaises(VqroError):core.invoke('accounts.continuation.confirm',click)
        self.assertEqual(len(core.sends),1)

    def test_common_reservation_one_slot_bounds_native_to_b_without_c(self):
        core=Pipeline(False,True);core.entries[POLICY]['value']['auto_cutover_max_per_hour']=1
        core.app.handle_blocked('x')
        record=next(r for r in core.op_entry['value']['claims'].values() if r['attempted_targets'])
        self.assertEqual(record['authorized_targets'],['b']);self.assertEqual(record['attempted_targets'],['b'])
        self.assertEqual(core.sends,[])

    def test_contradictory_host_quota_still_offers_after_confirmed_lifecycle(self):
        core=Pipeline(False,True)
        def hook(op,args):
            if op=='_account.inspect':core.native.quota['b']=args['resource']=='y'
        core.native.hook=hook;core.app.handle_blocked('x')
        self.assertEqual(core.sends,[])
        self.assertIn('accounts-continuation',core.live)
        self.assertEqual(sum(m=='resource.invoke' and a['args']['name']=='_account.release' for m,a in core.native.calls),2)

    def test_mixed_quota_owner_click_preserves_negative_and_never_switches_again(self):
        core=Pipeline(False,True)
        core.native.hook=lambda op,args:core.native.quota.update(b=args['resource']=='y') if op=='_account.inspect' else None
        core.app.handle_blocked('x');target=core.native.registry['accounts'][0]
        self.assertTrue(core.app.cutovers.known_blocked(target))
        effects=len(core.native.calls);click=core.live['accounts-continuation']['items'][-1]['args']
        core.invoke('accounts.continuation.confirm',click)
        self.assertEqual(len(core.sends),1);self.assertEqual(len(core.native.calls),effects)
        self.assertTrue(core.app.cutovers.known_blocked(target))
        with self.assertRaises(VqroError):core.invoke('accounts.continuation.confirm',click)
        self.assertEqual(len(core.sends),1)

    def test_registry_changed_inside_final_common_lookup_prevents_c(self):
        core=Pipeline(False,True);original_call=core.call;original_invoke=core.invoke
        armed=[False];inside=[False];changed=[False]
        def invoke(name,args=None):
            if name=='accounts.claim.read' and armed[0]:
                inside[0]=True
                try:return original_invoke(name,args)
                finally:inside[0]=False
            return original_invoke(name,args)
        def call(method,params=None):
            params=params or {}
            if method=='workspace.get' and params.get('key')==REGISTRY and inside[0] and not changed[0]:
                core.entries[REGISTRY]['version']=4;changed[0]=True
            result=original_call(method,params)
            if method=='resource.invoke' and params.get('op')=='agent.command' and params['args']['name']=='_account.inspect' and params['resource']=='y':armed[0]=True
            return result
        core.invoke=invoke;core.call=call;core.app.handle_blocked('x')
        self.assertTrue(changed[0]);self.assertEqual(core.sends,[])
        self.assertFalse(any(m=='resource.invoke' and a['args']['name']=='_account.switch' and a['args']['args']['account']['id']=='c' for m,a in core.native.calls))
        self.assertEqual(sum(m=='resource.invoke' and a['args']['name']=='_account.release' for m,a in core.native.calls),2)

    def test_consent_revoked_inside_final_common_lookup_prevents_c(self):
        core=Pipeline(False,True);original_call=core.call;original_invoke=core.invoke
        armed=[False];inside=[False];changed=[False]
        def invoke(name,args=None):
            if name=='accounts.claim.read' and armed[0]:
                inside[0]=True
                try:return original_invoke(name,args)
                finally:inside[0]=False
            return original_invoke(name,args)
        def call(method,params=None):
            params=params or {}
            if method=='workspace.get' and params.get('key')==REGISTRY and inside[0] and not changed[0]:
                core.entries[CONSENT]['value']={'setups':{}};changed[0]=True
            result=original_call(method,params)
            if method=='resource.invoke' and params.get('op')=='agent.command' and params['args']['name']=='_account.inspect' and params['resource']=='y':armed[0]=True
            return result
        core.invoke=invoke;core.call=call;core.app.handle_blocked('x')
        self.assertTrue(changed[0]);self.assertEqual(core.sends,[])
        self.assertFalse(any(m=='resource.invoke' and a['args']['name']=='_account.switch' and a['args']['args']['account']['id']=='c' for m,a in core.native.calls))
        self.assertEqual(sum(m=='resource.invoke' and a['args']['name']=='_account.release' for m,a in core.native.calls),2)

    def test_quota_invalidated_during_native_resume_never_auto_continues(self):
        core=Pipeline(True)
        def hook(op,args):
            if op=='_account.resume':core.native.quota['b']=False
        core.native.hook=hook;core.app.handle_blocked('x')
        self.assertEqual(core.sends,[])
        self.assertIn('accounts-continuation',core.live)
        self.assertEqual(sum(m=='resource.invoke' and a['args']['name']=='_account.release' for m,a in core.native.calls),2)

    def test_consent_or_mode_revoked_during_native_discovery_prevents_c(self):
        for changed in ('consent','mode'):
            with self.subTest(changed=changed):
                core=Pipeline(False,True)
                def hook(op,args):
                    if op=='_account.prepare_switch' and args['args']['args']['account']['id']=='c':
                        if changed=='consent':core.entries[CONSENT]['value']={'setups':{}}
                        else:core.entries[POLICY]['value']['blocked']='notify'
                core.native.hook=hook;core.app.handle_blocked('x')
                self.assertEqual(core.sends,[])
                self.assertFalse(any(m=='resource.invoke' and a['args']['name']=='_account.switch' and a['args']['args']['account']['id']=='c' for m,a in core.native.calls))
                self.assertEqual(sum(m=='resource.invoke' and a['args']['name']=='_account.release' for m,a in core.native.calls),2)

    def test_b_blocked_c_ready_is_one_claim_prefix_and_one_new_turn(self):
        core=self.run_case(False,True)
        self.assertEqual(len(core.sends),1)
        switches=[a['args']['args']['account']['id'] for m,a in core.native.calls if m=='resource.invoke' and a['args']['name']=='_account.switch']
        self.assertEqual(switches,['b','b','c','c'])
        record=next(r for r in core.op_entry['value']['claims'].values() if r['attempted_targets'])
        self.assertEqual(record['attempted_targets'],['b','c'])
        self.assertEqual(core.resources['x']['data']['agent']['account']['id'],'c')


if __name__=='__main__':unittest.main()
