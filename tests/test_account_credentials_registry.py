"""Credential reference schema only; no native credentials/files/runtime calls."""
from pathlib import Path
import sys
import unittest
sys.path[:0] = [str(Path(__file__).resolve().parents[1] / 'extensions')]
from account_credentials import native_registration, validate
from vqro_ext import VqroError


class CredentialRegistry(unittest.TestCase):
    def row(self, runtime='pi', **extra):
        return dict(id='b', label='B', runtime=runtime, kind='subscription', **extra)

    def test_pi_alias_is_stable_and_does_not_select_an_auth_file_or_config_folder(self):
        row = native_registration(validate(self.row(provider='anthropic')))
        self.assertEqual(row, native_registration(validate(row)))
        self.assertRegex(row['native_provider'], r'^vqro-account-[0-9a-f]{32}$')
        self.assertEqual(set(row), {'id', 'label', 'runtime', 'kind', 'provider', 'native_provider'})

    def test_codex_opaque_native_login_reference(self):
        row = self.row('codex', login_ref={'sessionId': 'native-login-2', 'accountId': 'native-workspace-2'})
        self.assertEqual(validate(row), row)
        for invalid in ({'threadId': 'thread-1', 'accountId': 'a'}, {'sessionId': '', 'accountId': 'a'},
                        {'sessionId': 's', 'accountId': 'a', 'token': 'not-allowed'}):
            with self.subTest(invalid=invalid), self.assertRaises(VqroError):
                validate(self.row('codex', login_ref=invalid))

    def test_claude_subscription_opaque_host_keychain_reference(self):
        row = self.row('claude', secret_ref='a' * 32, host='authenticated-terminal-1')
        self.assertEqual(validate(row), row)

    def test_api_keys_keep_host_keychain_reference(self):
        row = dict(self.row('codex'), kind='api_key', secret_ref='a' * 32, host='authenticated-terminal-1')
        self.assertEqual(validate(row), row)

    def test_no_profile_auth_file_token_or_cross_runtime_native_reference(self):
        for extra in ({'profile': '/fake/profile'}, {'auth_file': '/fake/auth.json'}, {'token': 'not-allowed'},
                      {'login_ref': {'sessionId': 's', 'accountId': 'a'}}, {'native_provider': 'alias-no-base'}):
            with self.subTest(extra=extra), self.assertRaises(VqroError):
                validate(self.row(**extra))
        with self.assertRaises(VqroError):
            validate(self.row('claude', provider='anthropic', native_provider='alias'))

    def test_codex_reference_invalid_unicode_is_a_validation_error(self):
        with self.assertRaises(VqroError):
            validate(self.row('codex', login_ref={'sessionId': '\ud800', 'accountId': 'a'}))


if __name__ == '__main__':
    unittest.main()
