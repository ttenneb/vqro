"""Common continuation configuration only; fake core and no model/native calls."""
from pathlib import Path
import sys
import unittest
sys.path[:0] = [str(Path(__file__).resolve().parents[1] / 'extensions'), str(Path(__file__).resolve().parent)]
from agent_fixture import FakeCore
from accounts import Accounts
import accounts_policy as policy
from vqro_ext import VqroError

DEFAULT = 'Continue where you left off. (vqro switched this agent to account B after account A reached its limit.)'


class ContinuationSettings(unittest.TestCase):
    def test_default_is_one_new_vqro_labelled_message_not_previous_input(self):
        self.assertEqual(policy.DEFAULTS['continuation_text'], DEFAULT)

    def test_text_is_bounded_literal_not_a_template_or_shell(self):
        custom = 'Continue. Literal $(do-not-run), `not-shell`, {not-a-template}.'
        self.assertEqual(policy.validate_settings({'continuation_text': custom})['continuation_text'], custom)
        for bad in (None, False, '', '  ', 'x\0y', 'x' * 4097, 'é' * 2049):
            with self.subTest(value=repr(bad)[:40]), self.assertRaises(VqroError):
                policy.validate_settings({'continuation_text': bad})

    def test_ui_saves_text_transactionally_and_exposes_same_setting(self):
        core = FakeCore('accounts')
        accounts = Accounts(core)
        accounts.configure_policy({'field': 'continuation_text', 'value': 'Continue the current task.', 'expect': 0})
        value = core.value(policy.POLICY)
        self.assertEqual(value['continuation_text'], 'Continue the current task.')
        self.assertTrue(any(row.get('command') == 'accounts.policy' and
                            row.get('args', {}).get('field') == 'continuation_text'
                            for row in core.live['accounts']['items']))
        self.assertFalse(any(method in ('resource.spawn', 'resource.invoke') for method, _ in core.calls))


if __name__ == '__main__':
    unittest.main()
