"""Accounts consumer integration of credential readiness/alias identity."""
import unittest
from test_account_credential_routes import row
from test_account_capacity import resource
from test_account_cutover_claims import Core
from account_credentials import SETTINGS
from accounts import Accounts
import accounts_policy as policy


class AccountRouteIntegration(unittest.TestCase):
    def core(self, rows):
        core = Core()
        core.apply([{'op':'set','key':SETTINGS,'value':{'accounts':rows}}], 'fake metadata')
        return core

    def test_invalid_registration_is_visible_without_launch_or_signin_actions(self):
        invalid = {key:value for key,value in row().items() if key != 'host'}
        core = self.core([invalid]); Accounts(core).show({})
        items = core.live['accounts']['items']
        self.assertTrue(any(item.get('style') == 'warn' and 'not selectable' in item.get('text','') for item in items))
        self.assertFalse(any(item.get('command') in ('tier1.claude','accounts.sign_in') and
                             item.get('args',{}).get('account') == 'a' for item in items))

    def test_source_and_blocked_credential_aliases_are_not_alternatives(self):
        a, alias = row(), row('alias')
        b, b_alias = row('b',secret_ref='b'*32), row('b-alias',secret_ref='b'*32)
        core = self.core([a,alias,b,b_alias]); core.resources['blocked-b']=resource('blocked-b',b,100,blocked=True)
        app = Accounts(core)
        self.assertEqual(app.alternatives(resource('source-a',a,100,blocked=True),policy.validate_settings({}))[0],[])


if __name__ == '__main__': unittest.main()
