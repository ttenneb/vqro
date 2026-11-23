"""Fake boundaries only: never invoke a real keychain."""
import base64
import ctypes
import subprocess
import sys
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'extensions'))
import accounts_keychain as keys


class KeychainTests(unittest.TestCase):
    def test_secret_is_not_printable(self):
        value = keys.Secret('fake-api-key')
        self.assertNotIn('fake-api-key', repr(value))
        self.assertNotIn('fake-api-key', str(value))

    def test_linux_store_uses_stdin_not_argv(self):
        run = Mock(return_value=subprocess.CompletedProcess([], 0, b'', b''))
        store = keys.SecretTool('/fake/secret-tool', run=run)
        handle = store.store(keys.Secret('fake-api-key'))
        args, options = run.call_args
        self.assertNotIn('fake-api-key', repr(args))
        self.assertEqual(options['input'], b'fake-api-key')
        self.assertNotIn('shell', options)
        self.assertIn(handle, args[0])

    def test_linux_lookup_preserves_exact_value(self):
        run = Mock(return_value=subprocess.CompletedProcess([], 0, b'fake-key ', b''))
        store = keys.SecretTool('/fake/secret-tool', run=run)
        self.assertEqual(store.retrieve('a' * 32).reveal(), 'fake-key ')

    def test_linux_ambiguous_failure_not_missing_or_locked(self):
        run = Mock(return_value=subprocess.CompletedProcess([], 1, b'', b'secret fake-api-key'))
        with self.assertRaises(keys.KeychainError) as caught:
            keys.SecretTool('/fake/secret-tool', run=run).retrieve('a' * 32)
        self.assertEqual(caught.exception.status, 'backend_error')
        self.assertNotIn('fake-api-key', str(caught.exception))

    def test_timeout_mutation_is_indeterminate(self):
        run = Mock(side_effect=subprocess.TimeoutExpired(['fake'], 1, stderr=b'fake-api-key'))
        with self.assertRaises(keys.KeychainError) as caught:
            keys.SecretTool('/fake/secret-tool', run=run).store(keys.Secret('fake-api-key'))
        self.assertEqual(caught.exception.status, 'indeterminate')
        self.assertEqual(caught.exception.handle, run.call_args.args[0][-1])
        self.assertEqual(run.call_count, 1)

    def test_clear_does_not_claim_confirmed_deletion(self):
        run = Mock(return_value=subprocess.CompletedProcess([], 0, b'', b''))
        with self.assertRaises(keys.KeychainError) as caught:
            keys.SecretTool('/fake/secret-tool', run=run).delete('a' * 32)
        self.assertEqual(caught.exception.status, 'indeterminate')

    def test_validation_precedes_boundary(self):
        run = Mock()
        store = keys.SecretTool('/fake/secret-tool', run=run)
        for value in ('', 'a\n', 'a\r', 'a\0', 'a' * 2049):
            with self.assertRaises(keys.KeychainError):
                store.store(keys.Secret(value))
        for handle in ('', '../anything', 'a' * 31, 'A' * 32):
            with self.assertRaises(keys.KeychainError):
                store.retrieve(handle)
        run.assert_not_called()

    def test_no_fallback_when_platform_missing(self):
        with self.assertRaises(keys.KeychainError) as caught:
            keys.backend(platform='linux', which=lambda name: None)
        self.assertEqual(caught.exception.status, 'unavailable')


class MacTests(unittest.TestCase):
    def test_timeout_preserves_handle_for_cleanup(self):
        run = Mock(side_effect=subprocess.TimeoutExpired(['fake'], 1))
        with self.assertRaises(keys.KeychainError) as caught:
            keys.MacSecurity('/fake/security', run=run).store(keys.Secret('fake-key'), handle='b' * 32)
        self.assertEqual((caught.exception.status, caught.exception.handle), ('indeterminate', 'b' * 32))
        self.assertIn(('b' * 32).encode(), run.call_args.kwargs['input'])
        run.assert_called_once()

    def test_fixed_alphabet_single_stdin_command_below_parser_limit(self):
        run = Mock(return_value=subprocess.CompletedProcess([], 0, b'', b''))
        value = 'quotes\" backslash\\ tab\t and unicode \u00e9; $(ignored)'
        store = keys.MacSecurity('/fake/security', run=run)
        handle = store.store(keys.Secret(value))
        args, options = run.call_args
        self.assertEqual(args[0], ['/fake/security', '-i', '-q'])
        data = options['input']
        self.assertEqual(data.count(b'\n'), 1)
        self.assertLess(len(data), 4095)
        self.assertNotIn(value.encode(), data)
        tokens = data.split()
        self.assertEqual(tokens[:5], [b'add-generic-password', b'-s', keys.SERVICE.encode(), b'-a', handle.encode()])
        self.assertEqual(base64.b64decode(tokens[-1][len(store.PREFIX):]), value.encode())

    def test_max_utf8_secret_fits_fixed_line(self):
        run = Mock(return_value=subprocess.CompletedProcess([], 0, b'', b''))
        keys.MacSecurity('/fake/security', run=run).store(keys.Secret('x' * keys.MAX_SECRET_BYTES))
        self.assertLess(len(run.call_args.kwargs['input']), 4095)

    def test_lookup_envelope_preserves_unusual_key(self):
        value = '\tquotes\"\\\u00e9 '
        raw = keys.MacSecurity.PREFIX + base64.b64encode(value.encode()) + b'\n'
        run = Mock(return_value=subprocess.CompletedProcess([], 0, raw, b''))
        self.assertEqual(keys.MacSecurity('/fake/security', run=run).retrieve('a' * 32).reveal(), value)

    def test_bad_or_raw_envelope_is_not_used_as_key(self):
        for raw in (b'not-an-envelope\n', keys.MacSecurity.PREFIX + b'!\n', b''):
            run = Mock(return_value=subprocess.CompletedProcess([], 0, raw, b''))
            with self.assertRaises(keys.KeychainError):
                keys.MacSecurity('/fake/security', run=run).retrieve('a' * 32)

    def test_delete_only_exact_namespaced_handle(self):
        run = Mock(return_value=subprocess.CompletedProcess([], 0, b'', b''))
        self.assertEqual(keys.MacSecurity('/fake/security', run=run).delete('a' * 32), 'deleted')
        self.assertEqual(run.call_args.args[0], ['/fake/security', 'delete-generic-password', '-s', keys.SERVICE, '-a', 'a' * 32])


class WindowsTests(unittest.TestCase):
    def setUp(self):
        self.dll = Mock()
        self.store = keys.WindowsCredentials(dll=self.dll, last_error=lambda: self.error)
        self.error = 1168

    def test_write_abi_and_blob_zeroed_after_call(self):
        saved = []
        def write(pointer, flags):
            row = ctypes.cast(pointer, ctypes.POINTER(keys.Credential)).contents
            saved.append((row.Type, row.TargetName, row.Persist, row.CredentialBlobSize,
                          ctypes.string_at(row.CredentialBlob, row.CredentialBlobSize), row.CredentialBlob))
            return 1
        self.dll.CredWriteW.side_effect = write
        with patch.object(keys.ctypes, 'memset', wraps=ctypes.memset) as cleared:
            handle = self.store.store(keys.Secret('fake-api-key'))
        kind, target, persist, length, blob, pointer = saved[0]
        self.assertEqual((kind, target, persist, blob), (1, keys.SERVICE + ':' + handle, 2, b'fake-api-key'))
        self.assertEqual(bytes(cleared.call_args.args[0]), b'\0' * length)
        self.assertEqual(self.dll.CredWriteW.restype, ctypes.c_int32)
        self.assertEqual(ctypes.sizeof(keys.DWORD), 4)

    def _read_blob(self, raw, size=None):
        buffer = (ctypes.c_ubyte * len(raw)).from_buffer_copy(raw)
        row = keys.Credential(Type=1, CredentialBlobSize=len(raw) if size is None else size,
                              CredentialBlob=buffer)
        def read(target, kind, flags, output):
            ctypes.cast(output, ctypes.POINTER(ctypes.POINTER(keys.Credential)))[0] = ctypes.pointer(row)
            return 1
        self.dll.CredReadW.side_effect = read
        return buffer, row

    def test_read_copies_then_frees(self):
        keepalive = self._read_blob(b'fake-api-key')
        self.assertEqual(self.store.retrieve('a' * 32).reveal(), 'fake-api-key')
        self.dll.CredFree.assert_called_once()
        self.assertTrue(keepalive)
        self.dll.CredReadW.assert_called_once()

    def test_invalid_blob_frees_even_when_decode_fails(self):
        for raw, size in ((b'\xff', None), (b'x', 2049), (b'', None)):
            with self.subTest(raw=raw, size=size):
                self.dll.CredFree.reset_mock()
                keepalive = self._read_blob(raw, size)
                with self.assertRaises(keys.KeychainError):
                    self.store.retrieve('a' * 32)
                self.dll.CredFree.assert_called_once()
                self.assertTrue(keepalive)

    def test_native_statuses_and_missing_delete(self):
        self.dll.CredReadW.return_value = 0
        self.dll.CredDeleteW.return_value = 0
        for code, status in ((1168, 'not_found'), (1312, 'unavailable'), (5, 'denied'), (999, 'backend_error')):
            self.error = code
            with self.assertRaises(keys.KeychainError) as caught:
                self.store.retrieve('a' * 32)
            self.assertEqual(caught.exception.status, status)
        self.error = 1168
        self.assertEqual(self.store.delete('a' * 32), 'not_found')
        self.dll.CredFree.assert_not_called()

    def test_two_accounts_have_separate_targets(self):
        self.dll.CredWriteW.return_value = 1
        first = self.store.store(keys.Secret('fake-a'))
        second = self.store.store(keys.Secret('fake-b'))
        self.assertNotEqual(first, second)
        self.assertNotEqual(self.store._target(first), self.store._target(second))


if __name__ == '__main__':
    unittest.main()
