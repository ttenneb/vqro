"""Actual SDK dispatch/_call context propagation; public Core/native effects mocked."""
import copy
import unittest
from unittest.mock import patch
import test_pi_resume_consumption as fixtures
import test_pi_resume_recovery as recovery
import test_vqro_ext as sdk_fixture
from accounts import Accounts
from vqro_ext import Extension, VqroError


class PiPublicEnvelope(unittest.TestCase):
    def fixture(self,failed=False):
        core,journal,frontend,token=fixtures.ResumeConsumption().fixture(automatic=False)
        if failed:journal.native_result(token,observed=False)
        else:
            row=core.resources['r1'];row['available']=True;row.pop('exit');row['data']['agent']['lifecycle']='live'
        app=Accounts(core);backend=core.call;wire=[];routes={};auth={'principal':'owner','initiator':'owner','author':'owner','actor':'fixture-ui'}
        sdk,_=sdk_fixture.InvocationAttribution().vqro(lambda method,params:Extension._handle(core,method,params))
        def reply(ident,value):
            slot=sdk.waiting[ident];slot[1]=value;slot[0].set()
        def send(message):
            wire.append(copy.deepcopy(message))
            if 'method' not in message:
                reply(routes.pop(message['id']),{key:value for key,value in message.items() if key in ('result','error')});return
            ident=message['id'];method=message['method'];params=message.get('params') or {}
            if method=='command.invoke':
                incoming=10000+ident;routes[incoming]=ident
                sdk._dispatch({'id':incoming,'method':method,'params':dict(params,**auth,invocation='public-proof')})
                return
            if method=='resource.invoke' and params.get('op')=='agent.command':
                row=core.resources['r1'];row['available']=False;row['exit']={'code':0,'success':True};row['data']['agent']['lifecycle']='ended'
                core.calls.append((method,copy.deepcopy(params)));reply(ident,{'result':{'outcome':'accepted'}});return
            try:reply(ident,{'result':backend(method,params)})
            except VqroError as error:reply(ident,{'error':{'message':str(error),'data':{'kind':error.kind}}})
        sdk._send=send;core.vqro=sdk
        core.invoke=lambda name,args=None:Extension.invoke(core,name,args)
        core.caller=lambda:Extension.caller(core)
        return core,journal,frontend,token,app,sdk,wire,auth

    def invoke(self,sdk,name,args):
        # src/tui/mod.rs run_command + Window.invocation_context: non-authority UI metadata.
        return sdk.call('command.invoke',{'name':name,'args':dict(args,context={'layout':'fixture-layout','layout_version':1,'path':[0]})})

    def test_manual_callback_uses_real_sdk_layout_envelopes_through_resume(self):
        core,journal,frontend,token,app,sdk,wire,auth=self.fixture()
        setup=fixtures.PiPrepare();setup.setUp()
        with patch('pi.launch',side_effect=lambda *a,**k:recovery.PiResumeRecovery().launch(core)) as launch:
            result=self.invoke(sdk,'accounts.cutover.pi',dict(setup.args(False),_claim=token))
        self.assertEqual(result['outcome'],'observed');self.assertEqual(launch.call_count,1)
        commands=[m['params'] for m in wire if m.get('method')=='command.invoke']
        for name in ('pi.resume.prepare','accounts.claim.resume.validate','pi.resume','accounts.claim.resume.consume','accounts.claim.resume.result'):
            rows=[p for p in commands if p['name']==name];self.assertTrue(rows,name)
            self.assertTrue(all(p['args']['context']['layout']=='fixture-layout' for p in rows),name)
        self.assertFalse(any(method=='resource.invoke' and params.get('op')=='agent.send' for method,params in core.calls))

    def test_recovery_click_accepts_tui_context_and_keeps_once_only_slot(self):
        core,journal,frontend,token,app,sdk,wire,auth=self.fixture(failed=True)
        with patch('pi.launch',side_effect=lambda *a,**k:recovery.PiResumeRecovery().launch(core)) as launch:
            self.invoke(sdk,'accounts.recover.pi',{'_claim':token})
            with self.assertRaises(VqroError):self.invoke(sdk,'accounts.recover.pi',{'_claim':token})
        self.assertEqual(launch.call_count,1)

    def test_strict_semantic_keys_and_context_shape_at_every_payload_boundary(self):
        core,journal,frontend,token,app,sdk,wire,auth=self.fixture()
        for name,args in [('pi.resume.prepare',{'_claim':token}),('accounts.recover.pi',{'_claim':token}),
                ('accounts.claim.resume.validate',{'claim':token}),('accounts.claim.resume.consume',{'claim':token}),
                ('accounts.claim.resume.result',{'claim':token,'observed':True})]:
            with self.subTest(command=name),self.assertRaises(VqroError) as caught:
                self.invoke(sdk,name,dict(args,unexpected=True))
            self.assertEqual(caught.exception.kind,'invalid')
            with self.subTest(command=name,context='malformed'),self.assertRaises(VqroError) as caught:
                sdk.call('command.invoke',{'name':name,'args':dict(args,context='not-an-object')})
            self.assertEqual(caught.exception.kind,'invalid')

    def test_readable_backend_refusals_prevent_stop_and_launch_consumption(self):
        cases=({'connected':False,'capabilities':{'terminal.spawn':{'env':True}}},
               {'connected':True,'capabilities':{'terminal.spawn':{'env':False}}},
               {'connected':True,'capabilities':{}})
        for provider in cases:
            with self.subTest(provider=provider):
                core,journal,frontend,token,app,sdk,wire,auth=self.fixture()
                core.providers['mux']=copy.deepcopy(provider)
                setup=fixtures.PiPrepare();setup.setUp()
                with patch('pi.launch',side_effect=VqroError('Core refused known unavailable spawn','unavailable')) as launch:
                    with self.assertRaises(VqroError):
                        self.invoke(sdk,'accounts.cutover.pi',dict(setup.args(False),_claim=token))
                self.assertTrue(core.resources['r1']['available'])
                self.assertEqual(core.resources['r1']['data']['agent']['lifecycle'],'live')
                self.assertFalse(any(method=='resource.invoke' for method,_ in core.calls))
                launch.assert_not_called();self.assertIsNone(journal.lookup(token)['resume_launch'])

    def test_backend_is_rechecked_after_exit_before_launch_slot_consumption(self):
        for provider in ({'connected':False,'capabilities':{'terminal.spawn':{'env':True}}},
                         {'connected':True,'capabilities':{'terminal.spawn':{'env':False}}}):
            with self.subTest(provider=provider):
                core,journal,frontend,token,app,sdk,wire,auth=self.fixture()
                row=core.resources['r1'];row['available']=False;row['exit']={'code':0,'success':True};row['data']['agent']['lifecycle']='ended'
                core.providers['mux']=copy.deepcopy(provider)
                with patch('pi.launch') as launch,self.assertRaises(VqroError):self.invoke(sdk,'pi.resume',{'_claim':token})
                launch.assert_not_called();self.assertIsNone(journal.lookup(token)['resume_launch'])

    def test_connected_spawn_accepts_core_eligible_environment_contracts(self):
        for spawn in ({'env':True},{},{'env':None}):
            with self.subTest(spawn=spawn):
                core,journal,frontend,token,app,sdk,wire,auth=self.fixture()
                core.providers['mux']={'connected':True,'capabilities':{'terminal.spawn':spawn}}
                setup=fixtures.PiPrepare();setup.setUp()
                with patch('pi.launch',side_effect=lambda *a,**k:recovery.PiResumeRecovery().launch(core)) as launch:
                    result=self.invoke(sdk,'accounts.cutover.pi',dict(setup.args(False),_claim=token))
                self.assertEqual(result['outcome'],'observed');self.assertEqual(launch.call_count,1)

    def test_context_is_not_authority_and_unknown_semantic_keys_still_refuse(self):
        core,journal,frontend,token,app,sdk,wire,auth=self.fixture(failed=True)
        with self.assertRaises(VqroError) as caught:self.invoke(sdk,'accounts.recover.pi',{'_claim':token,'extra':True})
        self.assertEqual(caught.exception.kind,'invalid')
        auth['principal']='other'
        with self.assertRaises(VqroError) as caught:
            sdk.call('command.invoke',{'name':'accounts.recover.pi','args':{'_claim':token,'context':{'layout':'fixture-layout','principal':'owner'}}})
        self.assertEqual(caught.exception.kind,'forbidden')
