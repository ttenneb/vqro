"""Headless launcher policy with fake public primitives only; no backend/CLI/auth/profile IO."""
import importlib.util
import json
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
        self.providers = {'backend': {'role':'backend','state':'connected','capabilities':{'terminal.spawn':{'env':True}}}}

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
        expected_helper=[str(Path(sys.executable).resolve()),str((EXT/'accounts_keychain_helper.py').resolve())]
        self.assertEqual(params['argv'], ['vqro-bridge-claude', '--endpoint-principal', two['principal'],
                         '--restart-helper',json.dumps(expected_helper,ensure_ascii=False,separators=(',',':')),'--','claude'])
        self.assertNotIn('--bare', params['argv'])
        self.assertNotIn('--resume', params['argv'])
        self.assertEqual(params['cwd'], '/synthetic/work')
        self.assertEqual(launch.call_args.kwargs['expected_agents'], 1)
        self.assertEqual([method for method, _ in obj.ext.vqro.calls], ['workspace.get', 'workspace.get'])

    def test_private_restart_helper_preserves_native_options_and_ignores_public_helper_args(self):
        prefix=['/synthetic/custom bridge']
        native=['claude','--model','synthetic-model']
        obj=launcher([{'name':'native','argv':native,'bridge_argv':prefix}])
        with patch.object(MODULE,'choose_account',return_value=(None,None)), \
             patch.object(MODULE,'launch_cwd',return_value=None), \
             patch.object(MODULE,'launch',return_value={'agent':'r1','host':'r2'}) as launch:
            obj.new({'restart_helper':['untrusted-executable','untrusted-helper'],
                     'helper':['other-untrusted-executable']})
        argv=launch.call_args.args[1]['argv'];at=argv.index('--restart-helper')
        self.assertEqual(argv[:1],prefix)
        helper=json.loads(argv[at+1])
        self.assertEqual(helper,[str(Path(sys.executable).resolve()),str((EXT/'accounts_keychain_helper.py').resolve())])
        self.assertTrue(all(Path(value).is_absolute() for value in helper))
        self.assertEqual(argv[at+2:],['--',*native])
        self.assertEqual(argv.count('--restart-helper'),1)
        self.assertNotIn('untrusted-executable',str(argv))
        self.assertNotIn('CLAUDE_CODE_OAUTH_TOKEN',str(argv))
        self.assertEqual(prefix,['/synthetic/custom bridge']);self.assertEqual(native,['claude','--model','synthetic-model'])

    def test_missing_private_helper_does_not_refuse_initial_launch_or_guess_another_helper(self):
        missing=EXT/'not-installed'/'claude_code.py'
        obj=launcher([{'name':'native','argv':['claude']}])
        with patch.object(MODULE,'__file__',str(missing)), \
             patch.object(MODULE,'choose_account',return_value=(None,None)), \
             patch.object(MODULE,'launch_cwd',return_value=None), \
             patch.object(MODULE,'launch',return_value={'agent':'r1','host':'r2'}) as launch:
            obj.new({})
        launch.assert_called_once()
        argv=launch.call_args.args[1]['argv'];at=argv.index('--restart-helper')
        helper=json.loads(argv[at+1])
        self.assertEqual(helper[1],str(missing.with_name('accounts_keychain_helper.py').resolve()))
        self.assertFalse(Path(helper[1]).exists())
        self.assertEqual(argv[at+2:],['--','claude'])

    def test_named_subscription_pins_backend_and_passes_only_private_reference(self):
        account={'id':'a','label':'A','runtime':'claude','provider':'anthropic','kind':'subscription',
                 'secret_ref':'a'*32,'host':'backend'}
        obj=launcher([{'name':'native','argv':['claude','--model','synthetic-model']}])
        with patch.object(MODULE,'choose_account',return_value=(account,None)), \
             patch.object(MODULE,'launch_cwd',return_value=None), \
             patch.object(MODULE,'launch',return_value={'agent':'r1','host':'new-terminal'}) as launch:
            obj.new({'account':'a','provider':'untrusted-other-backend','host':'old-terminal'})
        launch.assert_called_once()
        params=launch.call_args.args[1];argv=params['argv']
        self.assertEqual(params['provider'],'backend')
        at=argv.index('--launch-credential')
        self.assertEqual(json.loads(argv[at+1]),{'account':'a','ref':'a'*32,'host':'backend'})
        self.assertLess(at,argv.index('--'))
        self.assertEqual(argv[argv.index('--')+1:],['claude','--model','synthetic-model'])
        self.assertEqual(json.loads(params['env']['VQRO_ACCOUNT'])['id'],'a')
        self.assertNotIn('CLAUDE_CODE_OAUTH_TOKEN',params['env'])
        self.assertNotIn('secret_ref',params['env']['VQRO_ACCOUNT'])

    def test_disconnected_named_backend_never_falls_back_to_another_backend(self):
        account={'id':'a','label':'A','runtime':'claude','provider':'anthropic','kind':'subscription',
                 'secret_ref':'a'*32,'host':'backend'}
        obj=launcher([{'name':'native','argv':['claude']}])
        obj.ext.providers['backend']['state']='disconnected'
        obj.ext.providers['other']={'role':'backend','state':'connected'}
        with patch.object(MODULE,'choose_account',return_value=(account,None)), \
             patch.object(MODULE,'launch_cwd',return_value=None),patch.object(MODULE,'launch') as launch:
            with self.assertRaises(MODULE.VqroError):obj.new({'account':'a'})
        launch.assert_not_called()

    def test_per_agent_host_registration_refuses_with_re_register_guidance(self):
        account={'id':'a','label':'A','runtime':'claude','provider':'anthropic','kind':'subscription',
                 'secret_ref':'a'*32,'host':'old-terminal'}
        obj=launcher([{'name':'native','argv':['claude']}])
        with patch.object(MODULE,'choose_account',return_value=(account,None)), \
             patch.object(MODULE,'launch_cwd',return_value=None),patch.object(MODULE,'launch') as launch:
            with self.assertRaisesRegex(MODULE.VqroError,'re-register: host must be the backend name'):
                obj.new({'account':'a'})
        launch.assert_not_called()

    def test_api_key_and_unbound_subscription_refuse_before_any_backend_launch(self):
        for kind in ('subscription','api_key'):
            with self.subTest(kind=kind):
                obj=launcher([{'name':'native','argv':['claude']}])
                with patch.object(MODULE,'choose_account',return_value=({'kind':kind},None)), \
                     patch.object(MODULE,'launch_cwd',return_value=None),patch.object(MODULE,'launch') as launch:
                    with self.assertRaises(MODULE.VqroError) as caught:obj.new({'account':'named'})
                launch.assert_not_called();self.assertEqual(caught.exception.kind,'unsupported')
                self.assertIn('launching is not available yet' if kind=='api_key' else
                              're-register: host must be the backend name',str(caught.exception))

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
