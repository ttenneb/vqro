"""Headless launcher policy with fake public primitives only; no backend/CLI/auth/profile IO."""
import importlib.util
import os
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

EXT = Path(os.environ.get('VQRO_TEST_EXTENSIONS_DIR', Path(__file__).resolve().parents[1] / 'extensions'))
sys.path.insert(0, str(EXT))
SPEC = importlib.util.spec_from_file_location('claude_code_launcher', EXT / 'claude_code.py')
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class FakeCore:
    def __init__(self, rows):
        self.rows = rows
        self.calls = []

    def call(self, method, args):
        self.calls.append((method, args))
        if method == 'workspace.get':
            return {'value': {'runtimes': self.rows}}
        raise AssertionError('unexpected public call: ' + method)


class FakeExtension:
    def __init__(self, rows):
        self.vqro = FakeCore(rows)
        self.notices = []
        self.panels = []

    def notify(self, text, style):
        self.notices.append((text, style))

    def publish(self, name, value):
        self.panels.append((name, value))


def launcher(rows):
    result = MODULE.ClaudeCode.__new__(MODULE.ClaudeCode)
    result.ext = FakeExtension(rows)
    return result


class ClaudeCodeLauncherTests(unittest.TestCase):
    def test_uses_existing_backend_launch_and_unique_explicit_endpoint_principal_without_prompt(self):
        obj = launcher([{'name': 'native', 'argv': ['claude'], 'bridge_env': {'VQRO_PRINCIPAL': 'owner', 'VQRO_ACCOUNT': 'forged'}}])
        with patch.object(MODULE, 'choose_account', return_value=(None, None)), \
             patch.object(MODULE, 'launch_cwd', return_value='/synthetic/work'), \
             patch.object(MODULE, 'launch', side_effect=lambda *a, **k: {'agent': 'r1', 'host': 'r2'}) as launch:
            one = obj.new({})
            two = obj.new({})
        self.assertNotEqual(one['principal'], two['principal'])
        params = launch.call_args.args[1]
        self.assertTrue(params['bridge'])
        self.assertEqual(params['env']['VQRO_PRINCIPAL'], two['principal'])
        self.assertNotIn('VQRO_ACCOUNT', params['env'])
        self.assertEqual(params['argv'], ['vqro-bridge-claude', '--endpoint-principal', two['principal'], '--', 'claude'])
        self.assertNotIn('--bare', params['argv'])
        self.assertNotIn('--resume', params['argv'])
        self.assertEqual(params['cwd'], '/synthetic/work')
        self.assertEqual(launch.call_args.kwargs['expected_agents'], 1)
        self.assertEqual([method for method, _ in obj.ext.vqro.calls], ['workspace.get', 'workspace.get'])

    def test_account_popup_returns_before_any_backend_effect(self):
        obj = launcher([{'name': 'native', 'argv': ['claude']}])
        popup = {'client': {'popup': {'view': 'synthetic-account-choice'}}}
        with patch.object(MODULE, 'choose_account', return_value=(None, popup)), \
             patch.object(MODULE, 'launch_cwd', return_value=None), \
             patch.object(MODULE, 'launch') as launch:
            self.assertEqual(obj.new({}), popup)
        launch.assert_not_called()

    def test_runtime_menu_preserves_context_without_launching_or_showing_truncated_argv(self):
        obj = launcher([{'name': 'one', 'argv': ['claude']}, {'name': 'two', 'argv': ['claude']}])
        with patch.object(MODULE, 'launch_cwd', return_value='/synthetic/work'), patch.object(MODULE, 'launch') as launch:
            result = obj.new({'fresh': True})
        launch.assert_not_called()
        self.assertEqual(result['client']['popup']['view'], 'claude-code-runtimes')
        for item in obj.ext.panels[0][1]['items']:
            self.assertEqual(item['args']['cwd'], '/synthetic/work')
            self.assertTrue(item['args']['fresh'])
            self.assertNotIn('description', item)

    def test_unknown_runtime_is_a_refusal_not_a_native_fallback(self):
        obj = launcher([{'name': 'native', 'argv': ['claude']}])
        with patch.object(MODULE, 'launch_cwd', return_value=None), patch.object(MODULE, 'launch') as launch:
            with self.assertRaises(MODULE.VqroError):
                obj.new({'runtime': 'missing'})
        launch.assert_not_called()

    def test_uncertain_launch_is_not_retried(self):
        obj = launcher([{'name': 'native', 'argv': ['claude']}])
        with patch.object(MODULE, 'choose_account', return_value=(None, None)), \
             patch.object(MODULE, 'launch_cwd', return_value=None), \
             patch.object(MODULE, 'launch', side_effect=MODULE.VqroError('synthetic delivery unknown', 'indeterminate')) as launch:
            with self.assertRaises(MODULE.VqroError):
                obj.guarded(obj.new)({})
        self.assertEqual(launch.call_count, 1)
        self.assertIn('no automatic retry', obj.ext.notices[0][0])


if __name__ == '__main__':
    unittest.main()
