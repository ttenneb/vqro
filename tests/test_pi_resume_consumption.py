"""Real public resume + owner journal; only native launch is mocked."""
import copy
import unittest
import threading
from unittest.mock import patch
from test_accounts_pi import PiPrepare
from test_account_cutover_claims import Core
from accounts_cutover import Cutovers
from accounts_pi import PiCutover
from pi import Pi
from vqro_ext import VqroError


class ResumeConsumption(unittest.TestCase):
    def fixture(self):
        setup=PiPrepare();setup.setUp()
        core=Core()
        core.entries=copy.deepcopy(setup.core.entries)
        core.resources=copy.deepcopy(setup.core.resources)
        core.providers=copy.deepcopy(setup.core.providers)
        core.caller_record={'principal':'extension:accounts'}
        journal=Cutovers(core)
        callback=PiCutover(core)
        source=core.resources['r1']
        args=setup.args();plan=callback.prepare(args)
        token=journal.claim(source,setup.b,7,plan,True,'Continue where you left off.')
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
