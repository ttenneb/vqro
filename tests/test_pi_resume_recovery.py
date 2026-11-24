"""Owner recovery uses the ORIGINAL unused launch CAS; real frontend, fake native launch."""
import copy
import json
import unittest
from unittest.mock import patch
import test_pi_resume_consumption as fixture
from accounts import Accounts
from account_credentials import fact
from vqro_ext import VqroError


class PiResumeRecovery(unittest.TestCase):
    def fixture(self,automatic=False):
        core,journal,frontend,token=fixture.ResumeConsumption().fixture(automatic=automatic)
        journal.native_result(token,observed=False)
        core.caller_record={'principal':'owner','initiator':'owner'}
        app=Accounts(core);invoke=core.invoke
        def public(name,args=None):
            if name=='pi.resume.recover':return frontend.recover_resume(args)
            if name=='pi.resume':return frontend.resume(args)
            return invoke(name,args)
        core.invoke=public
        return core,journal,frontend,token,app

    def actions(self,core,app):
        app.show({})
        return [item for item in core.live['accounts']['items'] if item.get('command')=='accounts.recover.pi']

    def launch(self,core):
        source=core.resources['r1'];new=copy.deepcopy(source)
        new.update(id='new',host='new-host',provider='new-runtime',available=True);new.pop('exit')
        native=new['data']['agent'];native.update(epoch='new-epoch',lifecycle='live',completion=None)
        setup=fixture.PiPrepare();setup.setUp();native['account']=fact(setup.b)
        native['session']['model']['native']['provider']='alias-b'
        core.resources['new']=new
        core.providers['new-runtime']={'role':'runtime','host':'new-host','capabilities':{'agent.command':{}}}
        return {'outcome':'observed','agent':'new','host':'new-host'}

    def test_unused_slot_offers_one_click_on_intended_account_and_never_rearms(self):
        core,journal,frontend,token,app=self.fixture()
        actions=self.actions(core,app);self.assertEqual(len(actions),1)
        self.assertIn('B',actions[0]['text']);self.assertEqual(actions[0]['args'],{'_claim':token})
        with patch('pi.launch',side_effect=lambda *a,**k:self.launch(core)) as launch:
            app.recover_pi(actions[0]['args'])
            with self.assertRaises(VqroError):app.recover_pi(actions[0]['args'])
        self.assertEqual(launch.call_count,1)
        params=launch.call_args.args[1];claim=journal.lookup(token)
        self.assertEqual(json.loads(params['env']['VQRO_ACCOUNT'])['id'],'b')
        self.assertEqual(params['argv'][params['argv'].index('--session')+1],claim['source_history'])
        self.assertEqual(claim['state'],'unknown')  # No switch/continuation fence rearmed.
        self.assertEqual(len(journal.read()[0]['claims']),1)
        self.assertEqual(claim['resume_launch']['state'],'observed')
        self.assertEqual(self.actions(core,app),[])
        self.assertFalse(any(method=='resource.invoke' for method,_ in core.calls))

    def test_post_stop_backend_failure_reports_session_and_real_recovery_action(self):
        core,journal,frontend,token,app=self.fixture()
        value,revision=journal.read();value['claims'][token]['state']='claimed';journal.write(value,revision)
        source=core.resources['r1'];source['available']=True;source.pop('exit');source['data']['agent']['lifecycle']='live'
        call=core.call;backend=copy.deepcopy(core.resources['terminal-1'])
        def disappear(method,params=None):
            if method=='resource.invoke' and params.get('op')=='agent.command':
                source['available']=False;source['exit']={'code':0,'success':True};source['data']['agent']['lifecycle']='ended'
                core.resources.pop('terminal-1');return {'outcome':'accepted'}
            return call(method,params)
        core.call=disappear;setup=fixture.PiPrepare();setup.setUp()
        with patch('pi.launch') as launch,self.assertRaises(VqroError) as caught:
            app.pi.restart(dict(setup.args(False),_claim=token))
        launch.assert_not_called();self.assertIn('Stopped, not resumed:',str(caught.exception))
        self.assertIn(source['data']['agent']['session']['id'],str(caught.exception))
        journal.native_result(token,observed=False)
        actions=self.actions(core,app);self.assertEqual(len(actions),1)
        core.resources['terminal-1']=backend
        with patch('pi.launch',side_effect=lambda *a,**k:self.launch(core)) as launch:
            app.recover_pi(actions[0]['args'])
        self.assertEqual(launch.call_count,1)

    def test_owner_recovery_of_auto_claim_sends_no_automatic_turn(self):
        core,journal,frontend,token,app=self.fixture(automatic=True)
        action=self.actions(core,app)[0]
        with patch('pi.launch',side_effect=lambda *a,**k:self.launch(core)):
            app.recover_pi(action['args'])
        claim=journal.lookup(token)
        self.assertTrue(claim['automatic']);self.assertEqual(claim['state'],'unknown')
        self.assertFalse(any(method=='resource.invoke' for method,_ in core.calls))

    def test_unconfirmed_stop_does_not_offer_a_recovery_launch(self):
        core,journal,frontend,token,app=self.fixture()
        core.resources['r1']['data']['agent']['lifecycle']='live';core.resources['r1']['available']=True
        self.assertEqual(self.actions(core,app),[])
        with patch('pi.launch') as launch,self.assertRaises(VqroError):frontend.recover_resume({'_claim':token})
        launch.assert_not_called();self.assertIsNone(journal.lookup(token)['resume_launch'])

    def test_uncertain_recovery_launch_stays_fenced_and_is_reported_honestly(self):
        core,journal,frontend,token,app=self.fixture();action=self.actions(core,app)[0]
        with patch('pi.launch',side_effect=VqroError('backend acceptance unknown','indeterminate')) as launch:
            with self.assertRaises(VqroError) as caught:app.recover_pi(action['args'])
        self.assertEqual(launch.call_count,1)
        self.assertIn('replacement launch unconfirmed',str(caught.exception))
        self.assertEqual(journal.lookup(token)['resume_launch']['state'],'unknown')
        self.assertEqual(self.actions(core,app),[])

    def test_unknown_launch_never_offers_or_permits_recovery(self):
        core,journal,frontend,token,app=self.fixture()
        value,revision=journal.read();value['claims'][token]['resume_launch']={'state':'unknown','principal':'owner'}
        journal.write(value,revision)
        self.assertEqual(self.actions(core,app),[])
        with patch('pi.launch') as launch,self.assertRaises(VqroError):app.recover_pi({'_claim':token})
        launch.assert_not_called()
        with patch('pi.launch') as launch,self.assertRaises(VqroError):frontend.recover_resume({'_claim':token})
        launch.assert_not_called()

    def test_non_owner_cannot_use_unused_recovery_slot(self):
        core,journal,frontend,token,app=self.fixture();core.caller_record={'principal':'other','initiator':'other'}
        with patch('pi.launch') as launch,self.assertRaises(VqroError):app.recover_pi({'_claim':token})
        launch.assert_not_called();self.assertIsNone(journal.lookup(token)['resume_launch'])
