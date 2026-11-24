"""Accounts consumer integration of credential readiness/alias identity."""
import unittest
from unittest.mock import patch
from vqro_ext import VqroError
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

    def test_unbound_named_api_key_launch_refuses_instead_of_shared_login_fallback(self):
        from pi import Pi
        from codex import Codex
        from tier1 import Tier1
        for runtime,provider in (('pi','anthropic'),('codex','openai'),('claude','anthropic')):
            with self.subTest(runtime=runtime):
                account=dict(row(),runtime=runtime,kind='api_key',provider=provider,host='keychain-only-host')
                core=self.core([account])
                with patch('pi.launch') as pi_launch,patch('codex.launch') as codex_launch:
                    app=Pi(core) if runtime=='pi' else Codex(core) if runtime=='codex' else Tier1(core)
                    with self.assertRaises(VqroError) as caught:
                        if runtime=='claude':app.launch(runtime,{'account':'a'})
                        else:app.new({'account':'a'})
                self.assertEqual(caught.exception.kind,'unsupported')
                self.assertIn('no verified execution-host/native credential binding',str(caught.exception))
                pi_launch.assert_not_called();codex_launch.assert_not_called()
                self.assertFalse(any(method=='resource.spawn' for method,_ in core.calls))

    def test_source_and_blocked_credential_aliases_are_not_alternatives(self):
        a, alias = row(), row('alias')
        b, b_alias = row('b',secret_ref='b'*32), row('b-alias',secret_ref='b'*32)
        core = self.core([a,alias,b,b_alias]); core.resources['blocked-b']=resource('blocked-b',b,100,blocked=True)
        app = Accounts(core)
        self.assertEqual(app.alternatives(resource('source-a',a,100,blocked=True),policy.validate_settings({}))[0],[])


if __name__ == '__main__': unittest.main()
