"""Diagnostics redact credentials without changing evidence or protocol replies."""
import copy
import json
from pathlib import Path
import sys
import unittest
from unittest.mock import AsyncMock
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'extensions'))
import vqro_provider as sdk


class Diagnostics(unittest.TestCase):
    def test_error_diagnostics_hide_known_credentials_and_echoes(self):
        token = 'private-host-012345'
        data = {'host_context': token, 'future': {'echo': f'bad {token}'}}
        error = sdk.ProviderError('backend', f'bad {token}', remote=True, data=data)
        self.assertNotIn(token, str(error))
        self.assertNotIn(token, repr(error))
        self.assertEqual(error.data, data)

    def test_nested_diagnostic_copy_and_unicode_echoes_do_not_mutate_input(self):
        token = 'private-quoted-"\\漢字'
        value = {'host_context': token, 'token': 'short', 'nested': [
            {'resume_token': 'short-long', 'echo': 'short-long'}, {'extension_token': 'extension-private'},
            {'bridge_token': 'bridge-private', 'echo': token}, {token: 'key echo'}]}
        original = copy.deepcopy(value)
        safe = sdk.diagnostic_data(value)
        text = json.dumps(safe, ensure_ascii=True)
        for credential in (token, 'short', 'short-long', 'extension-private', 'bridge-private'):
            self.assertNotIn(json.dumps(credential)[1:-1], text)
        self.assertEqual(value, original)
        self.assertEqual(safe['nested'][0]['echo'], '[redacted]')
        self.assertEqual(safe['host_context'], '[redacted]')


class WireSemantics(unittest.IsolatedAsyncioTestCase):
    async def test_private_reply_is_not_rewritten_by_diagnostic_formatting(self):
        provider = sdk.Provider('test', 'one', {})
        provider._write = AsyncMock()
        message = 'explicit private-host-012345'
        error = sdk.ProviderError('backend', message, data={'host_context': 'private-host-012345'})
        await provider._reply(7, error=error)
        row = provider._write.call_args.args[0]
        self.assertEqual(row['error']['message'], message)
        self.assertEqual(row['error']['data'], {'kind': 'backend'})
        self.assertEqual(error.data['host_context'], 'private-host-012345')
