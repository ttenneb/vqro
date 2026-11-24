"""Rust producer goldens -> real common policy/claims; fake native Codex boundary.
Public quota/account/incomplete fields stay exact. Never native/auth/model calls.
"""
import copy
import json
from pathlib import Path
import sys
import unittest
from unittest.mock import patch
sys.path[:0]=[str(Path(__file__).resolve().parents[1]/'extensions'),str(Path(__file__).resolve().parent)]
from accounts import Accounts
from accounts_cutover import Cutovers
import accounts_policy as policy
from account_credentials import SETTINGS
from test_account_cutover_dispatch import Core as NativeCore
from test_account_capacity import resource
from vqro_ext import VqroError


class DTOCore(NativeCore):
    """Keep actual common authorization; simulate only native prepare/switch/readback."""
    def invoke(self,name,args=None):
        if name=='accounts.cutover.codex':
            record=Cutovers(self).validate(args['_claim'],self.resources['r1'],'native-turn-1')
            keys=('resource','account','_account_revision','automatic')
            if any(record[key]!=args[key] for key in keys) or args['episode']!=record['native_episode']:
                raise VqroError('protected native cutover arguments changed','conflict')
        result=super().invoke(name,args)
        if name=='accounts.prepare.codex':
            result['affected'][0]['account']=args['account']
        elif name=='accounts.cutover.codex':
            result['account']=args['account'];result['affected'][0]['account']=args['account']
            self.resources['r1']['data']['agent']['account']={'id':args['account']}
        return result


class CapacityProducerPipeline(unittest.TestCase):
    def fixture(self,key):
        data=json.loads((Path(__file__).parent/'fixtures'/'usage_limits_incomplete.json').read_text(encoding="utf-8"))
        normalized=copy.deepcopy(data[key]);account=normalized['account']
        core=DTOCore();target=dict(core.b,id=account['id'],label=account['label'],kind=account['kind'],
            login_ref={'sessionId':'saved-work','accountId':data['dto']['accountId']})
        core.b=target
        core.entries[SETTINGS]={'version':7,'value':{'accounts':[core.a,target]}}
        del core.resources['capacity-b']
        core.resources['target']=resource('target',target)
        core.resources['target']['data']['agent'].update(normalized)
        # Native producer fields are neither repaired nor supplemented by the consumer.
        for field,value in normalized.items():self.assertEqual(core.resources['target']['data']['agent'][field],value)
        return core,Accounts(core),target

    def assert_same_conversation_cutover(self,core,accounts,target):
        old=copy.deepcopy(core.resources['r1'])
        accounts.handle_blocked('r1')
        self.assertEqual(core.effect_count,1);self.assertEqual(len(core.sends),1)
        self.assertEqual(core.resources['r1']['host'],old['host'])
        self.assertEqual(core.resources['r1']['data']['agent']['session'],old['data']['agent']['session'])
        self.assertEqual(core.resources['r1']['data']['agent']['account']['id'],target['id'])
        self.assertEqual(core.sends[0]['args']['guard'],{'session':'session-1'})
        self.assertEqual(core.sends[0]['args']['origin']['kind'],'tool')
        self.assertEqual(core.sends[0]['args']['content'],[{'type':'text','text':policy.CONTINUATION_TEXT}])
        self.assertFalse(any(name in ('codex.new','pi.new','tier1.claude','resource.spawn') for name,_ in core.calls))

    def test_actual_malformed_dto_is_unknown_not_partial_positive_but_auto_is_allowed(self):
        with patch('accounts_policy.now_ms',return_value=0):
            core,accounts,target=self.fixture('expected')
            self.assertIn('usage_limits',core.resources['target']['data']['agent']['incomplete'])
            self.assertIsNone(policy.capacities(list(core.resources.values()),[target])[target['id']])
            self.assert_same_conversation_cutover(core,accounts,target)
            # Counterfactual on the exact producer row, not B's newly resumed and
            # separately unknown native capacity, still proves the cut is essential.
            core.resources['target']['data']['agent']['incomplete'].remove('usage_limits')
            self.assertEqual(policy.capacities([core.resources['target']],[target])[target['id']],90)

    def test_actual_sparse_transition_does_not_turn_dropped_restriction_into_capacity(self):
        with patch('accounts_policy.now_ms',return_value=0):
            core,accounts,target=self.fixture('transition_expected')
            self.assertIn('usage_limits',core.resources['target']['data']['agent']['incomplete'])
            self.assertIsNone(policy.capacities(list(core.resources.values()),[target])[target['id']])
            self.assert_same_conversation_cutover(core,accounts,target)
            core.resources['target']['data']['agent']['incomplete'].remove('usage_limits')
            self.assertEqual(policy.capacities([core.resources['target']],[target])[target['id']],89)

    def test_actual_complete_dto_stays_positive_and_cutover_preserves_native_conversation(self):
        with patch('accounts_policy.now_ms',return_value=0):
            core,accounts,target=self.fixture('complete_expected')
            self.assertNotIn('usage_limits',core.resources['target']['data']['agent']['incomplete'])
            self.assertEqual(policy.capacities(list(core.resources.values()),[target])[target['id']],90)
            self.assert_same_conversation_cutover(core,accounts,target)


if __name__=='__main__':unittest.main()
