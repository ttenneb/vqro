"""Host helper tests use injected stores, input and exec; never real credentials."""
import contextlib
import io
import json
import sys
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'extensions'))
import accounts_keychain as keys
import accounts_keychain_helper as helper


class HelperTests(unittest.TestCase):
    def test_launch_only_puts_key_in_child_environment(self):
        store = Mock()
        store.retrieve.return_value = keys.Secret('fake-api-key')
        captured = []
        execute = Mock(side_effect=lambda program, argv, env: captured.append((program, argv, dict(env))))
        original = {'PATH': '/fake', 'OPENAI_API_KEY': 'ambient',
                    'ANTHROPIC_AUTH_TOKEN': 'ambient', 'CLAUDE_CODE_OAUTH_TOKEN': 'ambient',
                    'ANTHROPIC_API_KEY': 'ambient', 'VQRO_ACCOUNT': '{"kind":"api_key"}'}
        helper.launch('a' * 32, 'openai', ['runtime', '--flag'], store=store,
                      environment=original, execute=execute)
        program, argv, env = captured[0]
        self.assertEqual((program, argv), ('runtime', ['runtime', '--flag']))
        self.assertNotIn('fake-api-key', repr((program, argv)))
        self.assertEqual(env['OPENAI_API_KEY'], 'fake-api-key')
        self.assertNotIn('ANTHROPIC_AUTH_TOKEN', env)
        self.assertNotIn('ANTHROPIC_API_KEY', env)
        self.assertNotIn('CLAUDE_CODE_OAUTH_TOKEN', env)
        self.assertEqual(original['OPENAI_API_KEY'], 'ambient')
        self.assertEqual(env['VQRO_ACCOUNT'], original['VQRO_ACCOUNT'])
        store.retrieve.assert_called_once_with('a' * 32)

    def test_claude_subscription_token_is_only_in_selected_child_environment(self):
        store = Mock()
        store.retrieve.return_value = keys.Secret('fake-claude-setup-token')
        seen = []
        original = {'PATH': '/fake', 'ANTHROPIC_API_KEY': 'ambient-key',
                    'ANTHROPIC_AUTH_TOKEN': 'ambient-bearer', 'CLAUDE_CODE_OAUTH_TOKEN': 'old-token'}
        output, error = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(error):
            helper.launch('b' * 32, 'anthropic', ['claude', '--resume', 'native-session'],
                          credential_kind='claude_subscription', store=store, environment=original,
                          execute=lambda program, argv, env: seen.append((program, argv, dict(env))))
        program, argv, env = seen[0]
        self.assertEqual(env['CLAUDE_CODE_OAUTH_TOKEN'], 'fake-claude-setup-token')
        self.assertNotIn('ANTHROPIC_API_KEY', env)
        self.assertNotIn('ANTHROPIC_AUTH_TOKEN', env)
        self.assertEqual(original['CLAUDE_CODE_OAUTH_TOKEN'], 'old-token')
        self.assertNotIn('fake-claude-setup-token', repr((program, argv, output.getvalue(), error.getvalue())))
        store.retrieve.assert_called_once_with('b' * 32)

    def test_subscription_mode_rejects_wrong_provider_before_secret_access(self):
        store = Mock()
        for kind, provider in [('claude_subscription', 'openai'), ('unknown', 'anthropic')]:
            with self.assertRaises(keys.KeychainError):
                helper.launch('b' * 32, provider, ['claude'], credential_kind=kind,
                              store=store, execute=Mock())
        store.retrieve.assert_not_called()

    def test_subscription_entry_uses_hidden_token_prompt_and_returns_only_handle(self):
        store = Mock()
        store.store.return_value = 'b' * 32
        read = Mock(return_value='fake-claude-setup-token')
        result = helper.enter(store=store, read=read, terminal=True, credential_kind='claude_subscription')
        self.assertEqual(result, 'b' * 32)
        self.assertIn('Claude setup token', read.call_args.args[0])
        self.assertNotIn('fake-claude-setup-token', str(store.store.call_args))

    def test_subscription_exec_failure_does_not_leak_or_retry(self):
        store = Mock()
        store.retrieve.return_value = keys.Secret('fake-claude-setup-token')
        execute = Mock(side_effect=OSError('fake-claude-setup-token'))
        with self.assertRaises(keys.KeychainError) as caught:
            helper.launch('b' * 32, 'anthropic', ['claude'], credential_kind='claude_subscription',
                          store=store, execute=execute)
        self.assertNotIn('fake-claude-setup-token', str(caught.exception))
        execute.assert_called_once()

    def test_claude_preflight_reports_only_ready_and_does_not_execute(self):
        store = Mock()
        store.retrieve.return_value = keys.Secret('fake-claude-setup-token')
        output, error = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(error):
            result = helper.main(['check', '--handle', 'b' * 32, '--provider', 'anthropic',
                                  '--kind', 'claude_subscription'], store_factory=lambda: store)
        self.assertEqual(result, 0)
        self.assertEqual(json.loads(output.getvalue()), {'status': 'ready'})
        self.assertNotIn('fake-claude-setup-token', output.getvalue() + error.getvalue())
        store.retrieve.assert_called_once_with('b' * 32)

    def test_claude_preflight_refusal_is_safe_and_not_retried(self):
        store = Mock()
        store.retrieve.side_effect = RuntimeError('fake-claude-setup-token')
        output, error = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(error):
            result = helper.main(['check', '--handle', 'b' * 32, '--provider', 'anthropic',
                                  '--kind', 'claude_subscription'], store_factory=lambda: store)
        self.assertEqual(result, 1)
        self.assertEqual(output.getvalue(), '')
        self.assertEqual(json.loads(error.getvalue()), {'status': 'backend_error'})
        store.retrieve.assert_called_once()

    def test_unknown_provider_and_invalid_argv_fail_before_retrieval(self):
        store = Mock()
        for provider, argv in [('other', ['runtime']), ('openai', []), ('openai', ['a\0'])]:
            with self.assertRaises(keys.KeychainError):
                helper.launch('a' * 32, provider, argv, store=store, execute=Mock())
        store.retrieve.assert_not_called()

    def test_failed_retrieval_never_launches_or_falls_back(self):
        store = Mock()
        store.retrieve.side_effect = keys.KeychainError('locked')
        execute = Mock()
        with self.assertRaises(keys.KeychainError) as caught:
            helper.launch('a' * 32, 'anthropic', ['runtime'], store=store, execute=execute)
        self.assertEqual(caught.exception.status, 'locked')
        execute.assert_not_called()

    def test_exec_failure_is_safe_and_not_retried(self):
        store = Mock()
        store.retrieve.return_value = keys.Secret('fake-api-key')
        execute = Mock(side_effect=OSError('fake-api-key'))
        with self.assertRaises(keys.KeychainError) as caught:
            helper.launch('a' * 32, 'openai', ['runtime'], store=store, execute=execute)
        self.assertNotIn('fake-api-key', str(caught.exception))
        execute.assert_called_once()

    def test_non_terminal_entry_rejected_without_prompt(self):
        read = Mock()
        with self.assertRaises(keys.KeychainError) as caught:
            helper.enter(store=Mock(), read=read, terminal=False)
        self.assertEqual(caught.exception.status, 'denied')
        read.assert_not_called()

    def test_entry_returns_only_handle(self):
        store = Mock()
        store.store.return_value = 'a' * 32
        self.assertEqual(helper.enter(store=store, read=lambda _: 'fake-api-key', terminal=True), 'a' * 32)
        self.assertIsInstance(store.store.call_args.args[0], keys.Secret)

    def test_pending_handle_is_announced_before_indeterminate_store(self):
        events = []
        store = Mock()
        def write(secret, *, handle):
            self.assertEqual(events, [handle])
            raise keys.KeychainError('indeterminate')
        store.store.side_effect = write
        with self.assertRaises(keys.KeychainError) as caught:
            helper.enter(store=store, read=lambda _: 'fake-key', terminal=True, announce=events.append)
        self.assertEqual(caught.exception.handle, events[0])
        keys.valid_handle(events[0])
        store.store.assert_called_once()

    def test_interrupted_store_preserves_announced_handle(self):
        store, events = Mock(), []
        store.store.side_effect = KeyboardInterrupt()
        with self.assertRaises(keys.KeychainError) as caught:
            helper.enter(store=store, read=lambda _: 'fake-key', terminal=True, announce=events.append)
        self.assertEqual((caught.exception.status, caught.exception.handle), ('indeterminate', events[0]))
        store.store.assert_called_once()

    def test_cli_unknown_store_outputs_pending_and_final_cleanup_handle(self):
        store = Mock()
        store.store.side_effect = keys.KeychainError('indeterminate')
        output, error = io.StringIO(), io.StringIO()
        original_enter = helper.enter
        def fake_entry(**kwargs):
            return original_enter(**kwargs, read=lambda _: 'fake-key', terminal=True)
        with patch.object(helper, 'enter', side_effect=fake_entry), contextlib.redirect_stdout(output), contextlib.redirect_stderr(error):
            result = helper.main(['store'], store_factory=lambda: store)
        rows = [json.loads(line) for line in error.getvalue().splitlines()]
        self.assertEqual(result, 1)
        self.assertEqual(output.getvalue(), '')
        self.assertEqual([row['status'] for row in rows], ['pending_store', 'indeterminate'])
        self.assertEqual(rows[0]['handle'], rows[1]['handle'])
        self.assertEqual(rows[0]['handle'], store.store.call_args.kwargs['handle'])
        self.assertNotIn('fake-key', error.getvalue())
        store.store.assert_called_once()

    def test_cli_failure_has_only_safe_status(self):
        store = Mock()
        store.retrieve.side_effect = keys.KeychainError('locked')
        output, error = io.StringIO(), io.StringIO()
        with contextlib.redirect_stdout(output), contextlib.redirect_stderr(error):
            result = helper.main(['launch', '--handle', 'a' * 32, '--provider', 'openai', '--', 'runtime'],
                                 store_factory=lambda: store)
        self.assertEqual(result, 1)
        self.assertEqual(output.getvalue(), '')
        self.assertIn('locked', error.getvalue())


if __name__ == '__main__':
    unittest.main()
