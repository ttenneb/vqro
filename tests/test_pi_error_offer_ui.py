"""Human click paths for native Pi errors; fake public primitives, no runtime/auth."""
import copy
import unittest
import test_account_capacity as fixture
from agents import Agents
from vqro_ext import VqroError


class PiErrorOfferUI(unittest.TestCase):
    def setUp(self):
        self.case=fixture.CapacityPolicy();self.case.setUp();self.addCleanup(self.case.doCleanups)
        self.core=self.case.core;self.app=self.case.accounts
        self.case.settings(blocked='offer_restart');self.case.blocked()
        self.row=self.core.resources['old'];self.row['status']={'app':'pi'}
        self.row['data']['agent'].pop('availability',None)
        self.row['data']['agent']['completion']['error']['message']='Synthetic error not recognised as a usage limit.'

    def test_accounts_row_and_click_offer_are_human_judged_without_effect(self):
        self.app.show({})
        actions=[item for item in self.core.live['accounts']['items'] if item.get('command')=='accounts.offer']
        self.assertEqual(len(actions),1);self.assertEqual(actions[0]['args']['resource'],'old')
        self.assertIn('human-judged',actions[0]['text'])
        reply=self.app.offer(actions[0]['args']);items=self.core.live[reply['client']['popup']['view']]['items']
        self.assertTrue(any('not recognised as a usage limit' in item.get('text','') for item in items))
        choices=[item for item in items if item.get('command')=='accounts.restart']
        self.assertEqual([item['text'] for item in choices],['Continue on B'])
        self.assertIn('native_episode',choices[0]['args'])
        self.assertEqual(self.core.stops,[]);self.assertEqual(self.core.resumes,[]);self.assertEqual(self.core.sends,[])

    def test_manual_click_resumes_once_then_waits_without_a_continuation_turn(self):
        reply=self.app.offer({'context':{'resource':'old'}})
        choice=next(item for item in self.core.live[reply['client']['popup']['view']]['items'] if item.get('command')=='accounts.restart')
        self.app.restart(choice['args'])
        self.assertEqual(len(self.core.stops),1);self.assertEqual(len(self.core.resumes),1)
        self.assertEqual(self.core.sends,[])
        with self.assertRaises(VqroError):self.app.restart(choice['args'])
        self.assertEqual(len(self.core.stops),1);self.assertEqual(len(self.core.resumes),1)

    def test_unrecognised_error_never_authorizes_auto_or_claims_a_limit(self):
        self.case.settings(blocked='auto_restart');self.app.handle_blocked('old')
        self.assertEqual(self.core.stops,[]);self.assertEqual(self.core.resumes,[]);self.assertEqual(self.core.sends,[])
        self.app.show({})
        self.assertTrue(any(item.get('command')=='accounts.offer' for item in self.core.live['accounts']['items']))

    def test_owner_and_pending_input_are_checked_again_at_click(self):
        reply=self.app.offer({'resource':'old'})
        choice=next(item for item in self.core.live[reply['client']['popup']['view']]['items'] if item.get('command')=='accounts.restart')
        self.core.caller_record={'principal':'not-owner','initiator':'not-owner'}
        with self.assertRaises(VqroError):self.app.restart(choice['args'])
        self.core.caller_record={'principal':'owner','initiator':'owner'}
        self.row['data']['agent']['queue']=[{'delivery':'new-pending'}]
        with self.assertRaises(VqroError):self.app.restart(choice['args'])
        self.assertEqual(self.core.stops,[]);self.assertEqual(self.core.resumes,[]);self.assertEqual(self.core.sends,[])

    def test_truncated_error_is_offerable_but_never_labelled_native_limit(self):
        native=self.row['data']['agent'];native['completion']['native']['error_message_complete']=False
        native['completion']['error']['message']='You have hit your ChatGPT usage limit.'
        self.app.show({})
        action=next(item for item in self.core.live['accounts']['items'] if item.get('command')=='accounts.offer')
        reply=self.app.offer(action['args'])
        self.assertTrue(any('not recognised as a usage limit' in item.get('text','')
                            for item in self.core.live[reply['client']['popup']['view']]['items']))
        self.assertEqual(self.core.stops,[]);self.assertEqual(self.core.sends,[])

    def test_policy_command_is_a_real_settings_opener_without_a_write(self):
        before=len(self.core.transactions)
        reply=self.app.configure_policy({})
        self.assertEqual(reply['client']['popup']['view'],'accounts')
        self.assertEqual(len(self.core.transactions),before)

    def test_pane_menu_uses_public_context_resource_and_live_generic_error_flag(self):
        menu=self.core.value('menu/accounts.pi-error')
        self.assertIsNotNone(menu);self.assertEqual(menu['command'],'accounts.offer')
        self.assertEqual(menu['when'],{'resource_kind':'agent','menu.agent_error':True,'menu.agent_app':'pi'})
        agents=object.__new__(Agents)
        agents.ext=self.core
        flags=agents.pane_menu_flags(self.row)
        self.assertTrue(flags['agent_error']);self.assertEqual(flags['agent_app'],'pi')
        recovered=copy.deepcopy(self.row);recovered['data']['agent']['completion']={'reason':'end_turn'}
        self.assertFalse(agents.pane_menu_flags(recovered)['agent_error'])
        recovered['data']['agent']['completion']={'reason':'error'};recovered['data']['agent']['state']='running'
        self.assertFalse(agents.pane_menu_flags(recovered)['agent_error'])
        reply=self.app.offer({'context':{'resource':'old'}})
        self.assertEqual(reply['client']['popup']['view'],'accounts-restarts')

    def test_native_error_changes_after_choice_refuse_before_quiesce(self):
        reply=self.app.offer({'resource':'old'})
        choice=next(item for item in self.core.live[reply['client']['popup']['view']]['items'] if item.get('command')=='accounts.restart')
        self.row['data']['agent']['completion']['native']['timestamp']+=1
        with self.assertRaises(VqroError):self.app.restart(choice['args'])
        self.assertEqual(self.core.stops,[]);self.assertEqual(self.core.resumes,[])

    def test_success_running_and_queued_errors_do_not_offer_panel_action(self):
        original=copy.deepcopy(self.row)
        for patch in ({'state':'running'},{'queue':[{'delivery':'pending'}]}, {'completion':{'reason':'end_turn'}}):
            with self.subTest(patch=patch):
                self.row.clear();self.row.update(copy.deepcopy(original));self.row['data']['agent'].update(patch)
                self.app.show({})
                self.assertFalse(any(item.get('command')=='accounts.offer' for item in self.core.live['accounts']['items']))
