"""Real public resume + owner journal; only native launch is mocked."""
import copy
import unittest
import threading
from unittest.mock import patch
from test_accounts_pi import PiPrepare
from test_account_cutover_claims import Core
from accounts_cutover import Cutovers
from accounts_pi import PiCutover, displayed_error_binding, membership_revision
from pi import Pi
from vqro_ext import VqroError


class ResumeConsumption(unittest.TestCase):
    def fixture(self,automatic=True,message=None):
        setup=PiPrepare();setup.setUp()
        core=Core()
        core.entries=copy.deepcopy(setup.core.entries)
        core.resources=copy.deepcopy(setup.core.resources)
        core.providers=copy.deepcopy(setup.core.providers)
        core.caller_record={'principal':'extension:accounts'} if automatic else {'principal':'owner','initiator':'owner'}
        journal=Cutovers(core)
        callback=PiCutover(core)
        source=core.resources['r1']
        if message is not None:source['data']['agent']['completion']['error']['message']=message
        args=setup.args(automatic=automatic);plan=callback.prepare(args)
        token=journal.claim(source,setup.b,7,plan,automatic,'Continue where you left off.')
        source['available']=False;source['exit']={'code':0,'success':True}
        source['data']['agent']['lifecycle']='ended'
        original=core.invoke
        def invoke(name,args=None):
            if name=='accounts.claim.read':return journal.lookup(args['claim'])
            if name=='accounts.claim.resume.consume':return journal.consume_resume(args['claim'])
            if name=='accounts.claim.resume.result':journal.resume_result(args['claim'],args['observed']);return {'recorded':True}
            return original(name,args)
        core.invoke=invoke
        return core,journal,Pi(core),token

    def test_real_manual_resume_accepts_produced_claim_for_known_and_unknown_errors(self):
        for message in ('You have hit your ChatGPT usage limit.','Synthetic unrecognised native error.'):
            with self.subTest(message=message):
                core,journal,frontend,token=self.fixture(automatic=False,message=message)
                with patch('pi.launch',return_value={'outcome':'observed','agent':'new','host':'new-host'}) as launch:
                    frontend.resume({'_claim':token})
                self.assertEqual(launch.call_count,1)
                self.assertEqual(journal.lookup(token)['resume_launch']['state'],'observed')
                self.assertFalse(any(method=='resource.invoke' and params.get('op')=='agent.send' for method,params in core.calls))

    def test_manual_callback_reaches_real_resume_then_replacement_without_input(self):
        for message in ('You have hit your ChatGPT usage limit.','Synthetic unrecognised native error.'):
            with self.subTest(message=message):
                core,journal,frontend,token=self.fixture(automatic=False,message=message)
                source=core.resources['r1'];source['available']=True;source.pop('exit');source['data']['agent']['lifecycle']='live'
                call=core.call;invoke=core.invoke;stops=[]
                def public_call(method,params=None):
                    if method=='resource.invoke' and params.get('op')=='agent.command':
                        stops.append(copy.deepcopy(params));source['available']=False;source['exit']={'code':0,'success':True}
                        source['data']['agent']['lifecycle']='ended';return {'outcome':'accepted'}
                    return call(method,params)
                def public_invoke(name,args=None):
                    return frontend.resume(args) if name=='pi.resume' else invoke(name,args)
                def native_launch(*args,**kwargs):
                    new=copy.deepcopy(source);new.update(id='new',host='new-host',provider='new-runtime',available=True)
                    new.pop('exit');native=new['data']['agent']
                    native.update(epoch='new-epoch',lifecycle='live',completion=None)
                    from account_credentials import fact
                    setup=PiPrepare();setup.setUp();native['account']=fact(setup.b)
                    native['session']['model']['native']['provider']='alias-b'
                    core.resources['new']=new
                    core.providers['new-runtime']={'role':'runtime','host':'new-host','capabilities':{'agent.command':{}}}
                    return {'outcome':'observed','agent':'new','host':'new-host'}
                core.call=public_call;core.invoke=public_invoke
                setup=PiPrepare();setup.setUp()
                with patch('pi.launch',side_effect=native_launch) as launch:
                    result=PiCutover(core).restart(dict(setup.args(False),_claim=token))
                self.assertEqual(len(stops),1);self.assertEqual(launch.call_count,1)
                self.assertEqual(result['outcome'],'observed')
                self.assertTrue(result['affected'][0]['session_confirmed'])
                self.assertEqual(result['affected'][0]['session'],result['affected'][0]['requested_session'])
                self.assertEqual(journal.lookup(token)['resume_launch']['state'],'observed')
                self.assertFalse(any(method=='resource.invoke' and params.get('op')=='agent.send' for method,params in core.calls))

    def test_display_fence_is_separate_from_shared_native_membership(self):
        core,journal,frontend,token=self.fixture(automatic=False)
        claim=journal.lookup(token);source=core.resources['r1'];native=source['data']['agent']
        self.assertEqual(claim['displayed_error'],displayed_error_binding(source))
        entry=core.entries['doc/settings/pi']
        self.assertEqual(claim['membership_revision'],membership_revision(claim['affected'],claim['native_episode'],
            entry['version'],native['session']['model'],claim['source_history'],claim['source_principal']))
        value,revision=journal.read();value['claims'][token].pop('displayed_error')
        journal.write(value,revision)  # Historical records remain readable, not upgraded/replayed.
        self.assertNotIn('displayed_error',journal.lookup(token))
        source['available']=True;source.pop('exit');native['lifecycle']='live'
        setup=PiPrepare();setup.setUp()
        with self.assertRaises(VqroError):PiCutover(core).restart(dict(setup.args(False),_claim=token))
        self.assertFalse(any(method=='resource.invoke' for method,_ in core.calls))

    def test_real_manual_resume_keeps_native_model_guard(self):
        core,journal,frontend,token=self.fixture(automatic=False)
        core.resources['r1']['data']['agent']['session']['model']['id']='changed-native-model'
        with patch('pi.launch') as launch,self.assertRaises(VqroError) as caught:
            frontend.resume({'_claim':token})
        self.assertEqual(caught.exception.kind,'stale');launch.assert_not_called()
        self.assertIsNone(journal.lookup(token)['resume_launch'])

    def test_lowered_protected_allowance_blocks_pi_before_shutdown(self):
        core,journal,_,token=self.fixture()
        value,revision=journal.read();prior=copy.deepcopy(value['claims'][token])
        prior.update(episode='prior-episode',native_episode='prior-native',state='completed')
        value['claims']['b'*32]=prior;journal.write(value,revision)
        core.entries['doc/settings/accounts-policy']['value']['auto_cutover_max_per_hour']=1
        self.assertEqual(journal.lookup(token)['max_switches'],0)
        row=core.resources['r1'];row['available']=True;row.pop('exit')
        row['data']['agent']['lifecycle']='live'
        setup=PiPrepare();setup.setUp();args=dict(setup.args(),_claim=token)
        with self.assertRaises(VqroError) as caught:PiCutover(core).restart(args)
        self.assertEqual(caught.exception.kind,'offer_required')
        self.assertFalse(any(method=='resource.invoke' for method,_ in core.calls))

    def test_final_blocking_registry_read_cannot_mix_old_allowance_with_new_cap(self):
        core,journal,_,token=self.fixture()
        value,revision=journal.read();prior=copy.deepcopy(value['claims'][token])
        prior.update(episode='prior-episode',native_episode='prior-native',state='completed')
        value['claims']['b'*32]=prior;journal.write(value,revision)
        row=core.resources['r1'];row['available']=True;row.pop('exit');row['data']['agent']['lifecycle']='live'
        original=core.call;reads=[]
        def call(method,params=None):
            if method=='workspace.get' and params['key']=='doc/settings/accounts':
                reads.append(True)
                if len(reads)==4:core.entries['doc/settings/accounts-policy']['value']['auto_cutover_max_per_hour']=1
            return original(method,params)
        core.call=call
        setup=PiPrepare();setup.setUp()
        with self.assertRaises(VqroError):PiCutover(core).restart(dict(setup.args(),_claim=token))
        self.assertEqual(len(reads),4)
        self.assertFalse(any(method=='resource.invoke' for method,_ in core.calls))
        self.assertEqual(journal.lookup(token)['max_switches'],0)

    def test_final_projection_rejects_changed_registry_before_pi_shutdown(self):
        core,journal,_,token=self.fixture()
        row=core.resources['r1'];row['available']=True;row.pop('exit');row['data']['agent']['lifecycle']='live'
        original=core.call;reads=[]
        def call(method,params=None):
            if method=='workspace.get' and params['key']=='doc/settings/accounts':
                reads.append(True)
                if len(reads)==4:
                    entry=core.entries['doc/settings/accounts'];entry['version']=8
                    entry['value']['accounts'][1]['native_provider']='vqro_changed_b'
            return original(method,params)
        core.call=call
        setup=PiPrepare();setup.setUp()
        with self.assertRaises(VqroError) as caught:PiCutover(core).restart(dict(setup.args(),_claim=token))
        self.assertEqual(caught.exception.kind,'stale');self.assertEqual(len(reads),4)
        self.assertFalse(any(method=='resource.invoke' for method,_ in core.calls))

    def test_same_acknowledged_claim_can_launch_only_once(self):
        core,journal,frontend,token=self.fixture()
        with patch('pi.launch',return_value={'outcome':'observed','agent':'new','host':'new-host'}) as launch:
            frontend.resume({'_claim':token})
            with self.assertRaises(VqroError):frontend.resume({'_claim':token})
        self.assertEqual(launch.call_count,1)
        self.assertEqual(journal.lookup(token)['resume_launch']['state'],'observed')

    def test_unknown_launch_consumes_intent_and_is_never_replayed(self):
        core,journal,frontend,token=self.fixture()
        with patch('pi.launch',side_effect=VqroError('unknown launch','indeterminate')) as launch:
            with self.assertRaises(VqroError):frontend.resume({'_claim':token})
            with self.assertRaises(VqroError):frontend.resume({'_claim':token})
        self.assertEqual(launch.call_count,1)
        self.assertEqual(journal.lookup(token)['resume_launch']['state'],'unknown')

    def test_concurrent_resume_calls_race_on_the_owner_cas_not_native_launch(self):
        core,journal,frontend,token=self.fixture()
        barrier=threading.Barrier(2);local=threading.local();errors=[]
        read=journal.read;consume=journal.consume_resume
        def raced_read():
            result=read()
            if getattr(local,'consuming',False) and not getattr(local,'waited',False):
                local.waited=True;barrier.wait(timeout=5)
            return result
        def consuming(value):
            local.consuming=True
            try:return consume(value)
            finally:local.consuming=False
        journal.read=raced_read;journal.consume_resume=consuming
        def run():
            try:frontend.resume({'_claim':token})
            except VqroError as error:errors.append(error.kind)
        with patch('pi.launch',return_value={'outcome':'observed','agent':'new','host':'new-host'}) as launch:
            threads=[threading.Thread(target=run) for _ in range(2)]
            for thread in threads:thread.start()
            for thread in threads:thread.join(timeout=6)
            self.assertFalse(any(thread.is_alive() for thread in threads))
        self.assertEqual(launch.call_count,1);self.assertEqual(len(errors),1)

    def test_tool_caller_cannot_consume_an_auto_invocation(self):
        core,journal,frontend,token=self.fixture();core.caller_record={'principal':'agent','initiator':'extension:accounts'}
        with patch('pi.launch') as launch,self.assertRaises(VqroError):frontend.resume({'_claim':token})
        launch.assert_not_called()
        self.assertIsNone(journal.lookup(token)['resume_launch'])
