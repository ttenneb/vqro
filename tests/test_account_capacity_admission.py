"""Admission/completeness regressions; fake core only, never native/auth/profile calls."""
import copy
from pathlib import Path
import sys
import unittest
from unittest.mock import patch
sys.path[:0]=[str(Path(__file__).resolve().parents[1]/'extensions'),str(Path(__file__).resolve().parent)]
from accounts import Accounts
import account_profiles as profiles
import accounts_policy as policy
from test_account_capacity import Core, resource, NOW

class CapacityAdmission(unittest.TestCase):
    def fixture(self, *, paid=False, mode='auto_restart'):
        core=Core();accounts=Accounts(core)
        a={'id':'a','label':'A','runtime':'pi','kind':'subscription','profile':'/fake/a'}
        b={'id':'b','label':'B','runtime':'pi','kind':'api_key' if paid else 'subscription','profile':'/fake/b'}
        accounts.add(a);accounts.add(b)
        configured=dict(policy.DEFAULTS,blocked=mode,api_key_overflow=paid)
        core.apply([{'op':'set','key':policy.POLICY,'value':configured}],'fake policy')
        core.resources['old']=resource('old',a,blocked=True)
        core.resources['target']=resource('target',b,10)
        return core,accounts,a,b,configured

    def test_any_same_account_live_unknown_limits_prevents_partial_positive_capacity(self):
        for shape in ('absent','null','empty'):
            with self.subTest(shape=shape),patch('accounts_policy.now_ms',return_value=NOW):
                core,accounts,a,b,_=self.fixture()
                silent=resource('silent',b)
                if shape=='null':silent['data']['agent']['usage_limits']=None
                if shape=='empty':silent['data']['agent']['usage_limits']=[]
                core.resources['silent']=silent
                self.assertIsNone(policy.capacities(list(core.resources.values()),[b])['b'])
                accounts.handle_blocked('old');self.assertEqual(core.launches,[])
        with patch('accounts_policy.now_ms',return_value=NOW):
            core,accounts,a,b,_=self.fixture()
            core.resources['second']=resource('second',b,20)
            self.assertEqual(policy.capacities(list(core.resources.values()),[b])['b'],80)

    def test_post_claim_mutations_consume_claim_without_launch_or_replay(self):
        for change in ('exhausted','incomplete','expired','blocked','unavailable','notify','api_disabled','registry'):
            with self.subTest(change=change),patch('accounts_policy.now_ms',return_value=NOW) as clock:
                core,accounts,a,b,configured=self.fixture(paid=change=='api_disabled')
                original=copy.deepcopy(core.resources['target'])
                initial_rows=copy.deepcopy(core.value(profiles.SETTINGS))
                call=core.call;mutated=False
                def mutate_after_claim(method,params):
                    nonlocal mutated
                    result=call(method,params)
                    if method=='operational.apply' and not mutated:
                        mutated=True;facts=core.resources['target']['data']['agent']
                        if change=='exhausted':facts['usage_limits'][0]['used_percent']=100
                        elif change=='incomplete':facts['incomplete']=['usage_limits']
                        elif change=='expired':clock.return_value=NOW+policy.MAX_AGE_MS+1
                        elif change=='blocked':facts['availability']={'state':'blocked','reason':'quota'}
                        elif change=='unavailable':core.resources['target']['available']=False
                        elif change=='notify':core.apply([{'op':'set','key':policy.POLICY,'value':dict(configured,blocked='notify')}],'changed during claim')
                        elif change=='api_disabled':core.apply([{'op':'set','key':policy.POLICY,'value':dict(configured,api_key_overflow=False)}],'changed during claim')
                        elif change=='registry':
                            rows=copy.deepcopy(initial_rows);rows['accounts'][1]['profile']='/fake/changed'
                            core.apply([{'op':'set','key':profiles.SETTINGS,'value':rows}],'changed during claim')
                    return result
                with patch.object(core,'call',side_effect=mutate_after_claim):accounts.handle_blocked('old')
                self.assertTrue(mutated);self.assertIsNotNone(core.op_entry)
                self.assertEqual(core.launches,[])
                self.assertIn(policy.episode(core.resources['old']),core.op_entry['value']['attempts'])
                # Recovery is not permission to replay the consumed intention.
                clock.return_value=NOW;core.resources['target']=original
                core.apply([{'op':'set','key':policy.POLICY,'value':configured},
                            {'op':'set','key':profiles.SETTINGS,'value':initial_rows}],'fake recovery')
                Accounts(core).handle_blocked('old');self.assertEqual(core.launches,[])

    def test_manual_offer_rechecks_target_api_policy_and_registry_after_claim(self):
        from vqro_ext import VqroError
        for change in ('blocked','api_disabled','registry'):
            with self.subTest(change=change),patch('accounts_policy.now_ms',return_value=NOW):
                core,accounts,a,b,configured=self.fixture(paid=change=='api_disabled',mode='offer_restart')
                reply=accounts.offer({'resource':'old'})
                options=[row for row in core.live[reply['client']['popup']['view']]['items'] if row.get('command')=='accounts.restart']
                call=core.call;mutated=False
                def mutate_after_claim(method,params):
                    nonlocal mutated
                    result=call(method,params)
                    if method=='operational.apply' and not mutated:
                        mutated=True
                        if change=='blocked':core.resources['target']['data']['agent']['availability']={'state':'blocked','reason':'quota'}
                        elif change=='api_disabled':core.apply([{'op':'set','key':policy.POLICY,'value':dict(configured,api_key_overflow=False)}],'late policy')
                        else:
                            rows=core.value(profiles.SETTINGS);rows['accounts'][1]['profile']='/fake/changed'
                            core.apply([{'op':'set','key':profiles.SETTINGS,'value':rows}],'late registry')
                    return result
                with patch.object(core,'call',side_effect=mutate_after_claim),self.assertRaises(VqroError):accounts.restart(options[0]['args'])
                self.assertTrue(mutated);self.assertEqual(core.launches,[])
                self.assertIn(policy.episode(core.resources['old']),core.op_entry['value']['attempts'])

    def test_explicit_labelled_unknown_offer_remains_new_empty_work(self):
        with patch('accounts_policy.now_ms',return_value=NOW):
            core,accounts,a,b,_=self.fixture(mode='offer_restart')
            del core.resources['target']['data']['agent']['usage_limits']
            reply=accounts.offer({'resource':'old'})
            options=[row for row in core.live[reply['client']['popup']['view']]['items'] if row.get('command')=='accounts.restart']
            self.assertTrue(options);self.assertIn('unknown',options[0]['description'].lower())
            accounts.restart(options[0]['args']);self.assertEqual(len(core.launches),1)
            self.assertEqual(set(core.launches[0][1]),{'account','_account_revision','fresh'})

if __name__=='__main__':unittest.main()
