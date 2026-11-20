"""Generic SDK RPC-error evidence; no real socket, core, agent or model."""
import io
from pathlib import Path
import sys
import threading
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'extensions'))
import vqro_ext as sdk


class ErrorDataTests(unittest.TestCase):
    def error(self, payload):
        client = object.__new__(sdk.Vqro)
        client.lock = threading.Lock()
        client.next = 1
        client.waiting = {}
        def send(message):
            slot = client.waiting[message['id']]
            slot[1] = {'jsonrpc': '2.0', 'id': message['id'], 'error': payload}
            slot[0].set()
        client._send = send
        with self.assertRaises(sdk.VqroError) as failure:
            client._call('fixture.read')
        self.assertEqual(client.waiting, {})
        return failure.exception

    def test_error_data_and_original_object_are_retained_without_copy(self):
        data = {'kind': 'stale', 'outcome': 'rejected', 'effect': 'e:fixture', 'detail': {'native': [1, 2]}}
        wire = {'code': -32000, 'message': 'native guard rejected', 'data': data}
        error = self.error(wire)
        self.assertEqual(error.kind, 'stale')
        self.assertEqual(str(error), 'native guard rejected')
        self.assertIs(error.data, data)
        self.assertIs(error.wire_error, wire)
        self.assertIs(error.data['detail'], data['detail'])

    def test_absent_data_stays_absent(self):
        wire = {'code': -32000, 'message': 'no extra evidence'}
        error = self.error(wire)
        self.assertIsNone(error.kind)
        self.assertIsNone(error.data)
        self.assertIs(error.wire_error, wire)
        self.assertNotIn('data', error.wire_error)

    def test_non_object_json_data_remains_opaque_not_a_kind_claim(self):
        for data in (None, False, 17, 'not an object', ['native', 'detail']):
            with self.subTest(data=data):
                wire = {'code': -32000, 'message': 'opaque data', 'data': data}
                error = self.error(wire)
                self.assertIsNone(error.kind)
                self.assertIs(error.data, data)
                self.assertIs(error.wire_error, wire)

    def test_large_decoded_data_is_referenced_not_duplicated(self):
        data = {'kind': 'backend', 'opaque': 'x' * (1024 * 1024)}
        wire = {'code': -32000, 'message': 'large error', 'data': data}
        error = self.error(wire)
        self.assertIs(error.data, data)
        self.assertIs(error.data['opaque'], data['opaque'])
        self.assertIs(error.wire_error, wire)

    def test_oversized_wire_record_is_rejected_before_json_decode(self):
        client = object.__new__(sdk.Vqro)
        client.file = io.BytesIO(b'{"error":{"data":"' + b'x' * 2048 + b'"}}\n')
        # This exercises the existing framing gate without allowing the SDK's
        # managed-process exit to terminate the unittest runner.
        with patch.object(sdk, 'MAX_RECORD_BYTES', 1024), patch.object(sdk.os, '_exit') as exit_process, patch.object(sdk.json, 'loads') as decode:
            client._read()
        exit_process.assert_called_once_with(0)
        decode.assert_not_called()

    def test_malformed_error_envelope_is_a_protocol_error_with_original_value(self):
        for wire in ('garbage', [], None, 1):
            with self.subTest(wire=wire):
                error = self.error(wire)
                self.assertEqual(error.kind, 'protocol')
                self.assertIs(error.wire_error, wire)
                self.assertIsNone(error.data)

    def test_old_constructor_kind_and_message_are_unchanged(self):
        error = sdk.VqroError('original', 'conflict')
        self.assertEqual(error.kind, 'conflict')
        self.assertEqual(str(error), 'original')
        self.assertIsNone(error.data)
        self.assertIsNone(error.wire_error)


class OutboundErrorDataTests(unittest.TestCase):
    def dispatch_error(self, error):
        client = object.__new__(sdk.Vqro)
        sent = []
        client._send = sent.append
        def handler(*args):
            raise error
        client.handler = handler
        self.assertFalse(client._dispatch({'id': 1, 'method': 'command.invoke', 'params': {}}))
        self.assertEqual(len(sent), 1)
        return sent[0]['error']

    def test_structured_metadata_is_forwarded_with_authoritative_kind(self):
        data = {'kind': 'not-authoritative', 'uncached': True, 'window': {'epoch': 'new'}, 'future': [1]}
        reply = self.dispatch_error(sdk.VqroError('actual refusal', 'quota', data=data))
        self.assertEqual(reply['message'], 'actual refusal')
        self.assertEqual(reply['data'], dict(data, kind='quota'))
        self.assertEqual(data['kind'], 'not-authoritative', 'forwarding must not mutate the exception')
        self.assertIs(reply['data']['future'], data['future'])

    def test_unserialisable_or_nonfinite_object_data_falls_back_before_send(self):
        cyclic = {}; cyclic['self'] = cyclic
        for value in ({'bad': {1}}, {'bad': b'bytes'}, {'bad': Path('opaque')},
                      {'bad': float('nan')}, {'bad': '\ud800'}, cyclic):
            with self.subTest(value=repr(value)):
                reply = self.dispatch_error(sdk.VqroError('refused', 'quota', data=value))
                self.assertEqual(reply['data'], {'kind': 'quota'})

    def test_oversized_metadata_falls_back_inside_the_reply_cap(self):
        with patch.object(sdk, 'MAX_RECORD_BYTES', 256):
            reply = self.dispatch_error(sdk.VqroError('refused', 'quota', data={'huge': 'x' * 1024}))
        self.assertEqual(reply['data'], {'kind': 'quota'})

    def test_unrepresentable_kind_or_message_has_a_small_generic_fallback(self):
        with patch.object(sdk, 'MAX_RECORD_BYTES', 256):
            reply = self.dispatch_error(sdk.VqroError('m' * 1024, 'quota', data={'bad': {1}}))
        self.assertEqual(reply['data']['kind'], 'extension')
        self.assertTrue(reply['data']['incomplete'])

    def test_transport_failure_is_not_retried_as_a_fallback(self):
        client = object.__new__(sdk.Vqro)
        calls = []
        def send(message):
            calls.append(message)
            raise OSError('owned test transport failed')
        def handler(*args):
            raise sdk.VqroError('refused', 'quota', data={'uncached': True})
        client._send, client.handler = send, handler
        with self.assertRaises(OSError):
            client._dispatch({'id': 1, 'method': 'command.invoke', 'params': {}})
        self.assertEqual(len(calls), 1)

    def test_missing_or_nonobject_metadata_keeps_legacy_error_shape(self):
        for data in (None, False, 7, 'opaque', ['opaque']):
            with self.subTest(data=data):
                reply = self.dispatch_error(sdk.VqroError('refused', 'conflict', data=data))
                self.assertEqual(reply['data'], {'kind': 'conflict'})
        self.assertEqual(self.dispatch_error(RuntimeError('plain'))['data'], {'kind': 'extension'})


if __name__ == '__main__':
    unittest.main()
