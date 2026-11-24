"""Pi cutover preparation uses only synthetic public resource/native metadata."""
import copy
import json
from pathlib import Path
import sys
import unittest
from unittest.mock import patch
sys.path[:0]=[str(Path(__file__).resolve().parents[1]/'extensions'),str(Path(__file__).resolve().parent)]
from agent_fixture import FakeCore
from account_credentials import SETTINGS, fact
import accounts_policy as policy
from accounts_pi import PiCutover
from vqro_ext import VqroError

SESSION='01234567-89ab-4cde-8fab-0123456789ab'


class PiPrepare(unittest.TestCase):
    def setUp(self):
        self.core=FakeCore('accounts')
        self.core.entries[policy.POLICY]={'version':1,'value':{'blocked':'auto_restart'}}
        self.a={'id':'a','label':'A','runtime':'pi','kind':'subscription','provider':'openai-codex','native_provider':'alias-a'}
        self.b=dict(self.a,id='b',label='B',native_provider='alias-b')
        self.core.entries[SETTINGS]={'version':7,'value':{'accounts':[self.a,self.b]}}
        self.core.entries['doc/settings/pi']={'version':3,'value':{'runtimes':[{'name':'pi','argv':['pi1','--mode','rpc']}]}}
        self.core.providers['pi-runtime']={'role':'runtime','host':'terminal-1','capabilities':{'agent.command':{}}}
        self.row={'id':'r1','kind':'agent','available':True,'incarnation':'incarnation-1','provider':'pi-runtime','host':'terminal-1','cwd':'/fake/project',
          'data':{'agent':{'v':0,'state':'idle','lifecycle':'live','epoch':'epoch-1','account':fact(self.a),'queue':[],'tool_principal':'p:original-work',
            'session':{'id':SESSION,'native':{'file':'/synthetic/native-history.jsonl'},'model':{'id':'gpt-6-luna','effort':'high','native':{'provider':'alias-a','api':'openai-codex-responses'}}},
            'completion':{'id':'local-run','reason':'error','error':{'message':'You have hit your ChatGPT usage limit.'},
              'native':{'stopReason':'error','timestamp':123456,'error_message_complete':True}},
            'caps':{'rev':1,'agent.command':{},'agent':{'tool_principal':'env'}},'commands':[{'name':'vqro-account-quiesce'}]}}}
        self.core.resources={'r1':self.row,'terminal-1':{'id':'terminal-1','kind':'terminal','provider':'mux','available':True}}
        self.callback=PiCutover(self.core)
    def args(self, automatic=True):
        return {'resource':'r1','episode':policy.episode(self.row),'account':'b','_account_revision':7,'automatic':automatic}
    def no_effects(self):
        self.assertFalse(any(name in ('resource.invoke','resource.spawn','pi.resume') for name,_ in self.core.calls))
    def test_known_message_is_labelled_inference_not_native_quota(self):
        plan=self.callback.prepare(self.args())
        self.assertTrue(plan['automatic_allowed'])
        self.assertEqual(plan['inference']['label'],'switched because the error looked like a usage limit')
        self.assertEqual(plan['affected'][0]['session'],SESSION)
        self.assertEqual(plan['affected'][0]['account'],'b')
        self.assertNotIn('availability',self.row['data']['agent'])
        self.no_effects()
    def test_ephemeral_or_unknown_work_scope_never_stops_a(self):
        native=self.row['data']['agent']
        original=copy.deepcopy(native)
        for key in ('native','tool_principal'):
            native.clear();native.update(copy.deepcopy(original))
            if key=='native':native['session'].pop('native')
            else:native.pop('tool_principal')
            with self.assertRaises(VqroError):self.callback.prepare(self.args())
        self.no_effects()

    def test_public_resume_frontend_preserves_native_file_model_backend_and_work_scope(self):
        from pi import Pi
        claim=self.arm_restart()
        self.row['data']['agent']['lifecycle']='ended'
        self.row['exit']={'code':0,'success':True};self.row['available']=False
        frontend=Pi(self.core)
        with patch('pi.launch',return_value={'outcome':'observed','agent':'r2','host':'terminal-2'}) as launch:
            frontend.resume({'_claim':'a'*32})
        params=launch.call_args.args[1]
        self.assertEqual(params['provider'],'mux')
        self.assertEqual(params['env']['VQRO_PRINCIPAL'],'p:original-work')
        argv=params['argv']
        self.assertEqual(argv[argv.index('--session')+1],'/synthetic/native-history.jsonl')
        self.assertEqual(argv[argv.index('--model')+1],'gpt-6-luna')
        self.assertNotIn('--continue',argv)
        self.assertNotIn('prompt',str(params))
        with self.assertRaises(VqroError):frontend.resume({'_claim':'a'*32,'account':'a'})

    def test_stable_native_error_episode_is_independent_of_adapter_epoch(self):
        first=self.callback.prepare(self.args())
        self.row['data']['agent']['epoch']='new-epoch'
        second=self.callback.prepare(self.args())
        self.assertEqual(first['native_episode'],second['native_episode'])
        self.assertNotEqual(first['membership_revision'],second['membership_revision'])
    def test_unknown_or_truncated_message_is_offer_only(self):
        for message, complete in [('Unrecognized error',True),('You have hit your ChatGPT usage limit.',False)]:
            self.row['data']['agent']['completion']['error']['message']=message
            self.row['data']['agent']['completion']['native']['error_message_complete']=complete
            plan=self.callback.prepare(self.args(False))
            self.assertFalse(plan['automatic_allowed']);self.assertIsNone(plan['inference'])
        self.no_effects()
    def test_running_unknown_queue_or_newer_success_refuse_before_claim(self):
        original=copy.deepcopy(self.row)
        for patch in ({'state':'running'},{'queue':None},{'queue':[{'delivery':'queued'}]},
                      {'completion':{'reason':'end_turn'}}):
            self.row.clear();self.row.update(copy.deepcopy(original));self.row['data']['agent'].update(patch)
            with self.subTest(patch=patch),self.assertRaises(VqroError):self.callback.prepare(self.args())
        self.no_effects()
    def arm_restart(self, automatic=True):
        plan=self.callback.prepare(self.args(automatic))
        native=self.row['data']['agent']
        claim={'max_switches':3,'known_blocked_targets':[],'state':'claimed','resource':'r1','episode':policy.episode(self.row),'host':'terminal-1',
          'source_history':'/synthetic/native-history.jsonl','source_principal':'p:original-work','session':SESSION,'source_incarnation':self.row['incarnation'],'source_epoch':native['epoch'],
          'source_account':'a','native_episode':plan['native_episode'],'account':'b','_account_revision':7,
          'automatic':automatic,'runtime':'pi','setup_id':plan['setup_id'],
          'membership_revision':plan['membership_revision'],'affected':plan['affected'],'inference':plan['inference'],
          'authorization':{'mode':'auto','principal':None,'owner_revision':None} if automatic else
                          {'mode':'offer','principal':'owner','owner_revision':1}}
        self.resumes=[];self.stops=[];self.stop_unknown=False
        original_call=self.core.call;original_invoke=self.core.invoke
        def call(method,params=None):
            if method=='resource.invoke':
                self.stops.append(copy.deepcopy(params))
                if self.stop_unknown:raise VqroError('uncertain native stop','indeterminate')
                self.row['available']=False;self.row['exit']={'code':0,'success':True}
                self.row['data']['agent']['lifecycle']='ended'
                return {'outcome':'accepted'}
            return original_call(method,params)
        def invoke(name,args=None):
            if name=='accounts.claim.read':return copy.deepcopy(claim)
            if name=='accounts.claim.resume.consume':
                if claim.get('resume_launch') is not None:raise VqroError('already consumed','conflict')
                claim['resume_launch']={'state':'claimed'};return {'consumed':True}
            if name=='accounts.claim.resume.result':
                claim['resume_launch']['state']='observed' if args['observed'] else 'unknown';return {'recorded':True}
            if name=='pi.resume':
                self.resumes.append(copy.deepcopy(args))
                new=copy.deepcopy(self.row);new.update(id='r2',host='terminal-2',provider='pi-new',available=True)
                new['data']['agent'].update(epoch='epoch-2',lifecycle='live',account=fact(self.b),completion=None)
                new['data']['agent']['session']['model']['native']['provider']='alias-b'
                self.core.resources['r2']=new
                self.core.providers['pi-new']={'role':'runtime','host':'terminal-2','capabilities':{'agent.command':{}}}
                return {'outcome':'observed','agent':'r2','host':'terminal-2'}
            return original_invoke(name,args)
        self.core.call=call;self.core.invoke=invoke
        return claim

    def test_restart_observes_old_exit_and_exact_native_session_before_reporting_resumed(self):
        self.arm_restart();args=dict(self.args(),_claim='a'*32)
        result=self.callback.restart(args)
        self.assertEqual(len(self.stops),1);self.assertEqual(len(self.resumes),1)
        request=self.stops[0]['args']
        self.assertEqual(request['guard'],{'session':SESSION})
        self.assertEqual(json.loads(request['text'])['error_timestamp'],123456)
        self.assertEqual(result['affected'][0]['session'],SESSION)
        self.assertTrue(result['affected'][0]['session_confirmed'])
        with self.assertRaises(VqroError):self.callback.restart(args)
        self.assertEqual(len(self.resumes),1)

    def test_resume_requires_positive_exact_logical_work_principal_readback(self):
        for principal,proof in ((None,'env'),('p:wrong-work','env'),('p:original-work','unknown'),('p:original-work',None)):
            with self.subTest(principal=principal,proof=proof):
                self.setUp();self.arm_restart();original=self.core.invoke
                def invoke(name,args=None):
                    result=original(name,args)
                    if name=='pi.resume':
                        native=self.core.resources['r2']['data']['agent']
                        native['tool_principal']=principal
                        native['caps']['agent']['tool_principal']=proof
                    return result
                self.core.invoke=invoke
                with self.assertRaises(VqroError) as caught:self.callback.restart(dict(self.args(),_claim='a'*32))
                self.assertEqual(caught.exception.kind,'indeterminate')
                self.assertEqual(len(self.resumes),1)
                self.assertFalse(any(method=='resource.invoke' and params.get('op')=='agent.send' for method,params in self.core.calls))

    def test_unknown_stop_or_owner_change_never_starts_b(self):
        self.arm_restart();self.stop_unknown=True
        with self.assertRaises(VqroError):self.callback.restart(dict(self.args(),_claim='a'*32))
        self.assertEqual(self.resumes,[])
        self.arm_restart(False);self.core.entries['doc/principals/owner']['version']=2
        with self.assertRaises(VqroError):self.callback.restart(dict(self.args(False),_claim='a'*32))
        self.assertEqual(self.stops,[]);self.assertEqual(self.resumes,[])

    def test_virtual_model_and_known_source_alias_are_not_switch_targets(self):
        self.row['data']['agent']['session']['model']['native']['api']='pi-virtual'
        with self.assertRaises(VqroError):self.callback.prepare(self.args())
        self.row['data']['agent']['session']['model']['native']['api']='openai-codex-responses'
        self.b['native_provider']='alias-a'
        with self.assertRaises(VqroError):self.callback.prepare(self.args())
        self.no_effects()


if __name__=='__main__':unittest.main()
