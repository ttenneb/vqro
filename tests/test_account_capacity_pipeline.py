"""Same DTO/public-fact golden asserted by Rust producer, consumed by Python policy.
Only neutral resource envelope fields are added; quota/account/incomplete data stay exact.
No native/auth/model/profile calls.
"""
import copy
import json
from pathlib import Path
import sys
import unittest
from unittest.mock import patch
sys.path[:0]=[str(Path(__file__).resolve().parents[1]/'extensions'),str(Path(__file__).resolve().parent)]
from accounts import Accounts
import accounts_policy as policy
from test_account_capacity import Core, resource

class CapacityProducerPipeline(unittest.TestCase):
    def fixture(self,key):
        data=json.loads((Path(__file__).parent/'fixtures'/'usage_limits_incomplete.json').read_text())
        normalized=copy.deepcopy(data[key]);account=normalized['account']
        core=Core();accounts=Accounts(core)
        source={'id':'source-account','label':'Source','runtime':'pi','kind':'subscription','profile':'/fake/source'}
        target={'id':account['id'],'label':account['label'],'runtime':'codex','kind':account['kind'],'profile':'/fake/work'}
        accounts.add(source);accounts.add(target)
        core.apply([{'op':'set','key':policy.POLICY,'value':dict(policy.DEFAULTS,blocked='auto_restart')}],'fake policy')
        core.resources['source']=resource('source',source,blocked=True)
        core.resources['target']=resource('target',target)
        core.resources['target']['data']['agent'].update(normalized)
        # Same exact public producer fields, no manually added completeness or account scope.
        for field,value in normalized.items():self.assertEqual(core.resources['target']['data']['agent'][field],value)
        return core,accounts,target

    def test_actual_malformed_dto_projection_is_unknown_and_never_auto_launches(self):
        with patch('accounts_policy.now_ms',return_value=0):
            core,accounts,target=self.fixture('expected')
            self.assertIn('usage_limits',core.resources['target']['data']['agent']['incomplete'])
            self.assertIsNone(policy.capacities(list(core.resources.values()),[target])[target['id']])
            accounts.handle_blocked('source');self.assertEqual(core.launches,[])
            # Counterfactual: removing ONLY the producer's cut marker exposes the unsafe positive subset.
            core.resources['target']['data']['agent']['incomplete'].remove('usage_limits')
            self.assertEqual(policy.capacities(list(core.resources.values()),[target])[target['id']],90)

    def test_actual_complete_dto_projection_remains_positive_fresh_empty_work(self):
        with patch('accounts_policy.now_ms',return_value=0):
            core,accounts,target=self.fixture('complete_expected')
            self.assertNotIn('usage_limits',core.resources['target']['data']['agent']['incomplete'])
            self.assertEqual(policy.capacities(list(core.resources.values()),[target])[target['id']],90)
            old=copy.deepcopy(core.resources['source'])
            accounts.handle_blocked('source');self.assertEqual(len(core.launches),1)
            self.assertEqual(core.launches[0][0],'codex.new')
            self.assertEqual(core.launches[0][1]['account'],target['id'])
            self.assertEqual(set(core.launches[0][1]),{'account','_account_revision','fresh'})
            self.assertEqual(core.resources['source'],old)

if __name__=='__main__':unittest.main()
