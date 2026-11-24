"""Actual common/Pi admission over fake native effects; never auth/history/model I/O."""
import copy
from pathlib import Path
import sys
import unittest
from unittest.mock import patch
sys.path[:0]=[str(Path(__file__).resolve().parents[1]/'extensions'),str(Path(__file__).resolve().parent)]
from accounts import Accounts
import account_credentials as profiles
import accounts_policy as policy
from test_account_capacity import Core, resource, pi_source, NOW, SESSION
from vqro_ext import VqroError


class CapacityAdmission(unittest.TestCase):
    def fixture(self, *, paid=False, mode='auto_restart'):
        core=Core();accounts=Accounts(core)
        a={'id':'a','label':'A','runtime':'pi','kind':'subscription','provider':'openai-codex','native_provider':'vqro-account-a'}
        b={'id':'b','label':'B','runtime':'pi','kind':'subscription','provider':'openai-codex','native_provider':'vqro-account-b'}
        if paid:
            b={'id':'b','label':'B','runtime':'pi','kind':'api_key','provider':'openai','secret_ref':'d'*32,'host':'terminal-old'}
        accounts.add(a);accounts.add(b)
        configured=dict(policy.DEFAULTS,blocked=mode,api_key_overflow=paid)
        core.apply([{'op':'set','key':policy.POLICY,'value':configured}],'fake policy')
        pi_source(core,'old',a)
        core.resources['target']=resource('target',b,10)
        return core,accounts,a,b,configured

    def test_live_unknown_limits_never_become_partial_positive_but_auto_may_switch(self):
        for shape in ('absent','null','empty'):
            with self.subTest(shape=shape),patch('accounts_policy.now_ms',return_value=NOW):
                core,accounts,a,b,_=self.fixture()
                silent=resource('silent',b)
                if shape=='null':silent['data']['agent']['usage_limits']=None
                if shape=='empty':silent['data']['agent']['usage_limits']=[]
                core.resources['silent']=silent
                self.assertIsNone(policy.capacities(list(core.resources.values()),[b])['b'])
                accounts.handle_blocked('old')
                self.assertEqual(len(core.resumes),1);self.assertEqual(len(core.sends),1)
                self.assertEqual(core.launches,[])
        with patch('accounts_policy.now_ms',return_value=NOW):
            core,accounts,a,b,_=self.fixture()
            core.resources['second']=resource('second',b,20)
            self.assertEqual(policy.capacities(list(core.resources.values()),[b])['b'],80)

    def test_post_claim_changes_preserve_known_blocked_policy_and_no_replay(self):
        # Missing/incomplete/expired B observations become UNKNOWN, not known-blocked.
        # Loss of an unrelated B runtime does not invalidate its registered credential.
        # Exhaustion/blocking, authority/policy and registry changes still refuse effects.
        allowed={'incomplete','expired','unavailable'}
        for change in ('exhausted','incomplete','expired','blocked','unavailable','notify','registry'):
            with self.subTest(change=change),patch('accounts_policy.now_ms',return_value=NOW) as clock:
                core,accounts,a,b,configured=self.fixture()
                original=copy.deepcopy(core.resources['target'])
                source=copy.deepcopy(core.resources['old'])
                initial_rows=copy.deepcopy(core.value(profiles.SETTINGS))
                call=core.call;mutated=False
                def mutate_after_claim(method,params):
                    nonlocal mutated
                    result=call(method,params)
                    if method=='operational.apply' and params['ops'][0]['value'].get('claims') and not mutated:
                        mutated=True;facts=core.resources['target']['data']['agent']
                        if change=='exhausted':facts['usage_limits'][0]['used_percent']=100
                        elif change=='incomplete':facts['incomplete']=['usage_limits']
                        elif change=='expired':clock.return_value=NOW+policy.MAX_AGE_MS+1
                        elif change=='blocked':
                            facts['completion']={'reason':'error','error':{'message':'You have hit your ChatGPT usage limit.'},
                                'native':{'stopReason':'error','timestamp':NOW+1,'error_message_complete':True}}
                        elif change=='unavailable':core.resources['target']['available']=False
                        elif change=='notify':core.apply([{'op':'set','key':policy.POLICY,'value':dict(configured,blocked='notify')}],'changed during claim')
                        elif change=='registry':
                            rows=copy.deepcopy(initial_rows);rows['accounts'][1]['native_provider']='vqro-account-changed'
                            core.apply([{'op':'set','key':profiles.SETTINGS,'value':rows}],'changed during claim')
                    return result
                with patch.object(core,'call',side_effect=mutate_after_claim):accounts.handle_blocked('old')
                self.assertTrue(mutated)
                self.assertEqual(len(core.op_entry['value']['claims']),1)
                self.assertEqual(len(core.resumes),int(change in allowed))
                self.assertEqual(len(core.sends),int(change in allowed))
                self.assertEqual(core.launches,[])
                # Recover all observations/settings AND the stale original source: the
                # protected episode intent, not an ended source, must prevent replay.
                clock.return_value=NOW;core.resources['target']=original;core.resources['old']=source
                core.apply([{'op':'set','key':policy.POLICY,'value':configured},
                            {'op':'set','key':profiles.SETTINGS,'value':initial_rows}],'fake recovery')
                Accounts(core).handle_blocked('old')
                self.assertEqual(len(core.resumes),int(change in allowed))
                self.assertEqual(len(core.sends),int(change in allowed))

    def test_manual_offer_rechecks_blocking_registry_and_authenticated_owner_after_claim(self):
        for change in ('blocked','registry','owner'):
            with self.subTest(change=change),patch('accounts_policy.now_ms',return_value=NOW):
                core,accounts,a,b,configured=self.fixture(mode='offer_restart')
                reply=accounts.offer({'resource':'old'})
                options=[row for row in core.live[reply['client']['popup']['view']]['items'] if row.get('command')=='accounts.restart']
                self.assertTrue(options)
                call=core.call;mutated=False
                def mutate_after_claim(method,params):
                    nonlocal mutated
                    result=call(method,params)
                    if method=='operational.apply' and params['ops'][0]['value'].get('claims') and not mutated:
                        mutated=True
                        if change=='blocked':
                            core.resources['target']['data']['agent']['completion']={
                                'reason':'error','error':{'message':'You have hit your ChatGPT usage limit.'},
                                'native':{'stopReason':'error','timestamp':NOW+1,'error_message_complete':True}}
                        elif change=='owner':core.entries['doc/principals/owner']['version']+=1
                        else:
                            rows=core.value(profiles.SETTINGS);rows['accounts'][1]['native_provider']='vqro-account-changed'
                            core.apply([{'op':'set','key':profiles.SETTINGS,'value':rows}],'late registry')
                    return result
                with patch.object(core,'call',side_effect=mutate_after_claim),self.assertRaises(VqroError):
                    accounts.restart(options[0]['args'])
                self.assertTrue(mutated);self.assertEqual(core.resumes,[])
                self.assertEqual(core.stops,[]);self.assertEqual(core.sends,[])
                self.assertEqual(len(core.op_entry['value']['claims']),1)

    def test_api_key_launch_opt_in_does_not_enable_same_session_cutover(self):
        with patch('accounts_policy.now_ms',return_value=NOW):
            core,accounts,a,b,configured=self.fixture(paid=True,mode='offer_restart')
            self.assertIsNone(profiles.selection_problem(b))
            for enabled in (True,False):
                core.apply([{'op':'set','key':policy.POLICY,'value':dict(configured,api_key_overflow=enabled)}],'launch-only API preference')
                reply=accounts.offer({'resource':'old'})
                items=core.live[reply['client']['popup']['view']]['items']
                self.assertFalse(any(row.get('command')=='accounts.restart' for row in items))
                self.assertIn('API-key account: not yet',str(items))
            self.assertEqual(core.resumes,[]);self.assertEqual(core.sends,[])

    def test_explicit_unknown_offer_preserves_conversation_and_waits_for_human_input(self):
        with patch('accounts_policy.now_ms',return_value=NOW):
            core,accounts,a,b,_=self.fixture(mode='offer_restart')
            del core.resources['target']['data']['agent']['usage_limits']
            reply=accounts.offer({'resource':'old'})
            options=[row for row in core.live[reply['client']['popup']['view']]['items'] if row.get('command')=='accounts.restart']
            self.assertTrue(options);self.assertIn('unknown',options[0]['description'].lower())
            accounts.restart(options[0]['args'])
            self.assertEqual(len(core.resumes),1);self.assertEqual(core.launches,[])
            self.assertEqual(core.resources['resumed-1']['data']['agent']['session']['id'],SESSION)
            self.assertEqual(core.sends,[])


if __name__=='__main__':unittest.main()
