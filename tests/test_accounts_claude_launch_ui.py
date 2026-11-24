"""Accounts launch actions only; fake core, no launcher/helper/native effects."""
import unittest
import test_accounts as fixture
from account_credentials import SETTINGS


class ClaudeNamedLaunchUI(unittest.TestCase):
    def setUp(self):
        self.case=fixture.AccountTests();self.case.setUp();self.addCleanup(self.case.doCleanups)
        self.core=self.case.core;self.app=self.case.accounts

    def items(self):
        self.app.show({})
        return self.core.live['accounts']['items']

    def test_named_subscription_routes_to_credential_bound_native_frontend(self):
        self.case.add('claude',host='herdr')
        items=self.items()
        actions=[item for item in items if item.get('text')=='New agent on Work']
        self.assertEqual(len(actions),1)
        self.assertEqual(actions[0]['command'],'claude-code.new')
        self.assertEqual(actions[0]['args']['account'],'work')
        self.assertIs(type(actions[0]['args']['_account_revision']),int)
        self.assertFalse(any(method=='resource.spawn' for method,_ in self.core.calls))

    def test_shared_normal_route_is_unchanged_and_never_names_a_credential(self):
        items=self.items()
        actions=[item for item in items if item.get('text')=='Shared runtime login' and item.get('command')=='tier1.claude']
        self.assertEqual(len(actions),1);self.assertEqual(actions[0]['args'],{'account':'normal'})

    def test_api_key_metadata_never_gets_a_launch_action(self):
        self.case.add('claude',kind='api_key',host='herdr')
        items=self.items()
        self.assertFalse(any(item.get('text')=='New agent on Work' for item in items))
        self.assertTrue(any('API-key accounts: launching is not available yet' in item.get('text','') for item in items))

    def test_incomplete_subscription_stays_unselectable(self):
        self.core.apply([{'op':'set','key':SETTINGS,'value':{'accounts':[
            {'id':'work','label':'Work','runtime':'claude','provider':'anthropic','kind':'subscription','host':'herdr'}]}}], 'incomplete metadata')
        items=self.items()
        self.assertFalse(any(item.get('text')=='New agent on Work' for item in items))
        self.assertTrue(any('Invalid / not selectable:' in item.get('text','') for item in items))
