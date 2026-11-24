"""Actual Accounts worker/UI routes over synthetic callbacks; no native effects."""
import unittest
from unittest.mock import patch, Mock
from accounts import Accounts
from accounts_policy import POLICY
from test_account_cutover_dispatch import Core, NOW
from vqro_ext import VqroError


class AccountsWorkerIntegration(unittest.TestCase):
    def setUp(self):
        self.core=Core()
        self.app=Accounts(self.core)
        self.clock=patch('accounts_policy.now_ms',return_value=NOW)
        self.clock.start();self.addCleanup(self.clock.stop)

    def test_actual_worker_uses_cutover_then_one_new_turn_never_fresh_launch(self):
        self.core.resources['r1']['data']['agent']['old_input']='NEVER REPLAY THIS'
        self.app.handle_blocked('r1')
        self.assertEqual(self.core.effect_count,1)
        self.assertEqual(len(self.core.sends),1)
        self.assertEqual(self.core.sends[0]['op'],'agent.send')
        self.assertNotIn('NEVER REPLAY THIS',str(self.core.sends))
        self.assertFalse(hasattr(self.app,'launch_fresh'))

    def test_one_setup_inactive_unknown_b_is_an_auto_target(self):
        del self.core.resources['capacity-b']
        self.app.handle_blocked('r1')
        self.assertEqual(self.core.effect_count,1)
        self.assertEqual(len(self.core.sends),1)
        self.assertTrue(self.app.cutovers.known_blocked(self.core.a))

    def test_false_pi_predicate_does_not_make_ordinary_agents_actionable(self):
        import copy
        row=copy.deepcopy(self.core.resources['r1']);row['id']='ordinary'
        native=row['data']['agent'];native['availability']={'state':'available'}
        native.pop('usage_limits',None);native.pop('completion',None)
        self.core.resources['ordinary']=row
        self.assertFalse(self.app.actionable(row))
        self.app.changed('resource',{'id':'ordinary'})
        self.assertEqual(self.app.pending,set())
        native['completion']={'reason':'end_turn'}
        self.app.changed('resource',{'id':'ordinary'})
        self.assertEqual(self.app.pending,set())

    def test_actual_notify_worker_has_zero_effects(self):
        self.core.entries[POLICY]['value']['blocked']='notify'
        self.app.handle_blocked('r1')
        self.assertEqual(self.core.effect_count,0)
        self.assertEqual(self.core.sends,[])
        self.assertIsNone(self.core.op_entry)

    def test_untrusted_manual_click_cannot_gain_background_override(self):
        self.core.caller_record={'principal':'agent','initiator':'owner'}
        with self.assertRaises(VqroError):
            self.app.restart({'resource':'r1','background_loss_disclosed':True})
        self.assertEqual(self.core.effect_count,0)
        self.assertIsNone(self.core.op_entry)

    def test_claude_idle_terminal_episode_uses_warning_then_protected_offer(self):
        from test_accounts_claude import Fake
        from account_credentials import SETTINGS
        from accounts_policy import episode
        native=Fake();row=native.source
        row['data']['agent']['availability']['state']='available'
        self.core.resources={row['id']:row}
        self.core.providers=native.providers
        a=dict(native.target,id='a',secret_ref='a'*32)
        self.core.entries[SETTINGS]={'version':7,'value':{'accounts':[a,native.target]}}
        execute=Mock(return_value={'outcome':'observed'})
        self.app.dispatch.execute=execute
        args={'resource':row['id'],'episode':episode(row),'account':'b','_account_revision':7,
              'background_loss_disclosed':True}
        self.app.restart(args)
        execute.assert_not_called()
        warning=self.core.live['accounts-restart-warning']
        self.assertIn('Background work may be lost',str(warning))
        button=warning['items'][1]
        self.assertEqual(button['command'],'accounts.restart.confirm')
        self.app.confirm_restart(button['args'])
        self.assertTrue(execute.call_args.kwargs['background_loss_disclosed'])
        self.assertFalse(execute.call_args.kwargs['automatic'])
        self.assertEqual(execute.call_args.kwargs['expected_native_episode'],'native-result-uuid')
        execute.reset_mock()
        self.core.entries['doc/principals/owner']['version']=2
        with self.assertRaises(VqroError):self.app.confirm_restart(button['args'])
        execute.assert_not_called()

    def test_api_key_cutover_is_explicitly_not_yet_not_silently_launched(self):
        self.app.offer({'resource':'r1'})
        self.assertIn('API-key account: not yet',str(self.core.live))
        self.assertEqual(self.core.effect_count,0)


if __name__=='__main__':unittest.main()
