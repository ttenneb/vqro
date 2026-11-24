"""Credential selection never changes runtime configuration; fake metadata only."""
import json
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

sys.path[:0] = [str(Path(__file__).resolve().parents[1] / 'extensions'), str(Path(__file__).resolve().parent)]
from agent_fixture import FakeCore
from accounts import Accounts
import account_credentials as credentials
from vqro_ext import VqroError


class CredentialRegistryTests(unittest.TestCase):
    def row(self, **extra):
        return dict(id='a', label='Account A', runtime='pi', provider='openai-codex',
                    kind='subscription', native_provider='vqro-account-a', **extra)

    def test_claude_callback_is_registered_in_the_existing_accounts_process(self):
        core = FakeCore('accounts')
        Accounts(core)
        self.assertIn('accounts.cutover.claude', core.handlers)
        self.assertIn('accounts.claim.read', core.handlers)

    def test_pi_host_launcher_owns_shipped_extension_path(self):
        from pi import Pi
        core = FakeCore('pi')
        core.apply([{'op': 'set', 'key': credentials.SETTINGS, 'value': {'accounts': [self.row()]}}], 'fake registry')
        app = Pi(core)
        with patch('pi.launch', return_value={'outcome': 'observed'}) as launched:
            app.new({'account': 'a'})
        argv = launched.call_args.args[1]['argv']
        self.assertNotIn('--account-extension', argv)
        self.assertNotIn('-e', argv[argv.index('--') + 1:])
        self.assertTrue(any(value.endswith('pi_bridge/launch.py') for value in argv[:argv.index('--')]))

    def test_credential_record_is_not_a_configuration_folder(self):
        account = credentials.validate(self.row())
        self.assertEqual(account['native_provider'], 'vqro-account-a')
        with self.assertRaises(VqroError):
            credentials.validate(dict(self.row(), profile='/fake/not-a-setting'))

    def test_launch_passes_only_auth_reference_and_keeps_shared_runtime_settings(self):
        env = {'PI_CODING_AGENT_DIR': '/fake/shared', 'HOME': '/fake/home'}
        account = self.row()
        with patch.object(Path, 'open', side_effect=AssertionError('never read credentials')):
            selected = credentials.environment(account, env)
        self.assertEqual(selected['PI_CODING_AGENT_DIR'], '/fake/shared')
        self.assertEqual(selected['HOME'], '/fake/home')
        self.assertNotIn('VQRO_PI_AUTH_FILE', selected)
        self.assertEqual(json.loads(selected['VQRO_PI_ACCOUNT'])['native_provider'], account['native_provider'])
        self.assertNotIn('native_provider', json.loads(selected['VQRO_ACCOUNT']))

    def test_legacy_metadata_migration_never_opens_native_folders(self):
        core = FakeCore('accounts')
        old = dict(id='a', label='Account A', runtime='pi', provider='openai-codex', kind='subscription', profile='/fake/old')
        core.apply([{'op': 'set', 'key': credentials.SETTINGS, 'expect': 0,
                     'value': {'accounts': [old]}}], 'fake legacy metadata')
        with patch.object(Path, 'open', side_effect=AssertionError('never open native files')):
            Accounts(core)
            rows, _ = credentials.snapshot(core)
        self.assertNotIn('profile', rows[0])
        self.assertTrue(rows[0]['native_provider'].startswith('vqro-account-'))
        self.assertNotIn('auth_file', rows[0])

    def test_migration_is_one_validated_transaction_and_leaves_bad_registry_untouched(self):
        core = FakeCore('accounts')
        legacy = {'accounts': [dict(id='a', label='A', runtime='pi', kind='subscription', profile='/fake/old'),
                               dict(id='b', label='B', runtime='pi', kind='subscription', profile='relative')]}
        core.apply([{'op': 'set', 'key': credentials.SETTINGS, 'value': legacy}], 'fake legacy')
        revision = core.version(credentials.SETTINGS)
        with self.assertRaises(VqroError):
            credentials.migrate(core)
        self.assertEqual(core.value(credentials.SETTINGS), legacy)
        self.assertEqual(core.version(credentials.SETTINGS), revision)

    def test_windows_legacy_configuration_path_is_removed_without_credential_copy(self):
        core = FakeCore('accounts')
        old = dict(id='a', label='A', runtime='pi', provider='openai-codex', kind='subscription', profile=r'C:\fake\old')
        core.apply([{'op': 'set', 'key': credentials.SETTINGS, 'value': {'accounts': [old]}}], 'fake legacy')
        credentials.migrate(core)
        row = credentials.snapshot(core)[0][0]
        self.assertNotIn('auth_file', row)
        self.assertNotIn('profile', row)
        self.assertTrue(row['native_provider'].startswith('vqro-account-'))

    def test_migration_discards_other_runtime_folder_selection_without_touching_folders(self):
        core = FakeCore('accounts')
        old = dict(id='a', label='A', runtime='codex', kind='subscription', profile='/fake/old')
        core.apply([{'op': 'set', 'key': credentials.SETTINGS, 'value': {'accounts': [old]}}], 'fake legacy')
        with patch.object(Path, 'open', side_effect=AssertionError('never open native files')):
            credentials.migrate(core)
        migrated = credentials.snapshot(core)[0][0]
        self.assertEqual(migrated, {key: value for key, value in old.items() if key != 'profile'})
        self.assertEqual(credentials.environment(migrated, {'CODEX_HOME': '/fake/shared'})['CODEX_HOME'], '/fake/shared')

    def test_runtime_native_references_are_opaque_and_never_credential_values(self):
        codex = dict(id='c', label='C', runtime='codex', kind='subscription',
                     login_ref={'sessionId': 'opaque-session', 'accountId': 'opaque-workspace'})
        claude = dict(id='k', label='K', runtime='claude', kind='subscription',
                      secret_ref='a' * 32, host='r-execution-host')
        self.assertEqual(credentials.validate(codex), codex)
        self.assertEqual(credentials.validate(claude), claude)
        for bad in (dict(codex, login_ref={'sessionId': 's', 'accountId': 'w', 'accessToken': 'never-store'}),
                    dict(claude, secret_ref='raw-token-is-not-an-opaque-handle'),
                    dict(self.row(), auth_file='/fake/no-longer-a-selector')):
            with self.subTest(row=bad), self.assertRaises(VqroError): credentials.validate(bad)

    def test_non_pi_account_never_selects_an_alternate_config_home(self):
        for runtime, selector in (('codex', 'CODEX_HOME'), ('claude', 'CLAUDE_CONFIG_DIR')):
            account = dict(id='a', label='Account A', runtime=runtime, kind='subscription')
            selected = credentials.environment(account, {selector: '/fake/shared'})
            self.assertEqual(selected[selector], '/fake/shared')


if __name__ == '__main__':
    unittest.main()
