"""Public SDK opt-in parity: no ambient identity adoption and no endpoint attribution claim."""
import asyncio
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'extensions'))
from vqro_ext import host_context_hello
from vqro_provider import Provider


class HostHelpers(unittest.TestCase):
    def test_explicit_helper_preserves_fields_and_configured_identity(self):
        with patch.dict(os.environ, {'VQRO_HOST_CONTEXT': 'opaque', 'VQRO_PRINCIPAL': 'undeclared'}, clear=True):
            self.assertEqual(host_context_hello({'profiles': ['terminal']}), {'profiles': ['terminal'], 'host_context': 'opaque'})
            self.assertEqual(host_context_hello({'host_context': 'explicit'}), {'host_context': 'explicit'})
            identity = {'identity': {'kind': 'owner'}}
            self.assertEqual(host_context_hello(identity), identity)
            os.environ['VQRO_PRINCIPAL'] = 'endpoint-a'
            self.assertNotIn('host_context', host_context_hello())
        with patch.dict(os.environ, {}, clear=True):
            self.assertEqual(host_context_hello(), {})

    def test_tool_environment_never_inherits_owner_or_claims_endpoint_env(self):
        provider = Provider('runtime', 'one', {}, role='runtime', host_context=True)
        self.assertIsNone(provider.tool_context_env('explicit-endpoint'))
        provider.registration = {'host_context': 'opaque'}
        self.assertEqual(provider.tool_context_env('explicit-endpoint'), {
            'VQRO_SOCKET': 'explicit-endpoint', 'VQRO_SOCKET_SOURCE': '',
            'VQRO_PRINCIPAL': 'undeclared', 'VQRO_HOST_CONTEXT': 'opaque'})
        self.assertNotIn('agent', provider.capabilities)
        with self.assertRaises(ValueError):
            Provider('runtime', 'one', {}, host_context='truthy')


class HostHandshake(unittest.IsolatedAsyncioTestCase):
    @unittest.skipIf(sys.platform == "win32", "registration fixture uses asyncio.start_unix_server, not a Windows pipe server")
    async def test_provider_opt_in_is_explicit_on_its_registration_only(self):
        requests = []
        async def core(reader, writer):
            try:
                for _ in range(2):
                    request = json.loads(await reader.readline())
                    requests.append(request)
                    result = {} if request['method'] == 'hello' else {
                        'generation': 1, 'host_context': 'opaque',
                        'limits': {'stream_window_bytes': 262144, 'max_record_bytes': 1048576}}
                    writer.write((json.dumps({'jsonrpc': '2.0', 'id': request['id'], 'result': result}) + '\n').encode())
                    await writer.drain()
                await reader.read()
            finally:
                writer.close()
                await writer.wait_closed()
        with tempfile.TemporaryDirectory(prefix='py-hctx') as root:
            path = str(Path(root) / 's')
            server = await asyncio.start_unix_server(core, path)
            provider = Provider('runtime', 'one', {}, role='runtime', token='bootstrap', host_context=True)
            try:
                await provider.connect(path)
                self.assertEqual(provider.tool_context_env(path)['VQRO_HOST_CONTEXT'], 'opaque')
            finally:
                await provider.close()
                server.close()
                await server.wait_closed()
        self.assertNotIn('host_context', requests[0]['params'], 'provider hello is not a tool child')
        self.assertIs(requests[1]['params']['host_context'], True)
