"""Host-local API-key storage. No public operation returns secret material.

Adapters are deliberately small: errors never include subprocess output/native data.
Native macOS/Windows interoperability is unverified; tests inject every OS boundary.
"""
import base64
import ctypes
import re
import shutil
import subprocess
import sys
import uuid

MAX_SECRET_BYTES = 2048
SERVICE = 'vqro.api-key.v1'


class KeychainError(Exception):
    """Safe, structured status; never wrap raw native exceptions or diagnostics."""
    def __init__(self, status, *, handle=None):
        self.status = status
        self.handle = handle
        super().__init__('API key storage: ' + status)


class Secret:
    __slots__ = ('_value',)

    def __init__(self, value):
        if not isinstance(value, str) or not value or any(c in value for c in '\0\r\n'):
            raise KeychainError('invalid_secret')
        try:
            raw = value.encode('utf-8')
        except UnicodeError:
            raise KeychainError('invalid_secret') from None
        if len(raw) > MAX_SECRET_BYTES:
            raise KeychainError('invalid_secret')
        self._value = value

    def __repr__(self):
        return '<Secret redacted>'

    __str__ = __repr__

    def reveal(self):
        """Private launch/native boundary only. Do not log or serialize."""
        return self._value


def valid_handle(handle):
    if not isinstance(handle, str) or re.fullmatch('[0-9a-f]{32}', handle) is None:
        raise KeychainError('invalid_handle')
    return handle


def _decode(raw):
    try:
        return Secret(raw.decode('utf-8'))
    except (UnicodeError, KeychainError):
        raise KeychainError('backend_error') from None


class _CommandStore:
    def __init__(self, executable, *, run=subprocess.run):
        self.executable = executable
        self.run = run

    def _call(self, args, *, data=None, mutation=False, pending_handle=None):
        try:
            result = self.run([self.executable, *args], input=data,
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=30,
                              check=False)
        except subprocess.TimeoutExpired:
            raise KeychainError('indeterminate' if mutation else 'unavailable', handle=pending_handle) from None
        except FileNotFoundError:
            raise KeychainError('unavailable') from None
        except PermissionError:
            raise KeychainError('denied') from None
        except OSError:
            raise KeychainError('indeterminate' if mutation else 'unavailable', handle=pending_handle) from None
        if result.returncode:
            # CLI exit codes/diagnostic strings do not prove locked vs absent.
            raise KeychainError('backend_error')
        return result.stdout


class SecretTool(_CommandStore):
    """libsecret CLI. A successful clear cannot prove locked matches were removed."""
    @staticmethod
    def _attrs(handle):
        return ['application', SERVICE, 'account', valid_handle(handle)]

    def store(self, secret, *, handle=None):
        if not isinstance(secret, Secret):
            raise KeychainError('invalid_secret')
        handle = valid_handle(handle) if handle is not None else uuid.uuid4().hex
        self._call(['store', '--label=vqro API key', *self._attrs(handle)],
                   data=secret.reveal().encode('utf-8'), mutation=True, pending_handle=handle)
        return handle

    def retrieve(self, handle):
        return _decode(self._call(['lookup', *self._attrs(handle)]))

    def delete(self, handle):
        self._call(['clear', *self._attrs(handle)], mutation=True)
        # secret-tool clear only deletes UNLOCKED matches; retain registry reference.
        raise KeychainError('indeterminate')


class MacSecurity(_CommandStore):
    """Fixed-alphabet stdin command, never arbitrary quoting or secret argv.

    See docs/research/accounts-keychain.md for the parser/echo proof and limits.
    Store base64 envelope so find -w cannot hex-transform UTF-8/nonprintable data.
    """
    PREFIX = b'vqro-api-key-v1:'

    def store(self, secret, *, handle=None):
        if not isinstance(secret, Secret):
            raise KeychainError('invalid_secret')
        handle = valid_handle(handle) if handle is not None else uuid.uuid4().hex
        envelope = self.PREFIX + base64.b64encode(secret.reveal().encode('utf-8'))
        command = (b'add-generic-password -s ' + SERVICE.encode('ascii') + b' -a '
                   + handle.encode('ascii') + b' -w ' + envelope + b'\n')
        # Apple readline's fixed buffer is 4096. One full line, then EOF.
        if len(command) >= 4095:
            raise KeychainError('invalid_secret')
        self._call(['-i', '-q'], data=command, mutation=True, pending_handle=handle)
        return handle

    def retrieve(self, handle):
        raw = self._call(['find-generic-password', '-s', SERVICE, '-a', valid_handle(handle), '-w'])
        if not raw.endswith(b'\n') or not raw[:-1].startswith(self.PREFIX):
            raise KeychainError('backend_error')
        try:
            data = base64.b64decode(raw[len(self.PREFIX):-1], validate=True)
        except ValueError:
            raise KeychainError('backend_error') from None
        return _decode(data)

    def delete(self, handle):
        self._call(['delete-generic-password', '-s', SERVICE, '-a', valid_handle(handle)], mutation=True)
        return 'deleted'


# Fixed-width Win32 DWORD/BOOL even when fake ABI tests run on LP64 Unix.
DWORD = ctypes.c_uint32
BYTE_PTR = ctypes.POINTER(ctypes.c_ubyte)


class FileTime(ctypes.Structure):
    _fields_ = [('low', DWORD), ('high', DWORD)]


class Credential(ctypes.Structure):
    _fields_ = [('Flags', DWORD), ('Type', DWORD), ('TargetName', ctypes.c_wchar_p),
                ('Comment', ctypes.c_wchar_p), ('LastWritten', FileTime),
                ('CredentialBlobSize', DWORD), ('CredentialBlob', BYTE_PTR),
                ('Persist', DWORD), ('AttributeCount', DWORD), ('Attributes', ctypes.c_void_p),
                ('TargetAlias', ctypes.c_wchar_p), ('UserName', ctypes.c_wchar_p)]


class WindowsCredentials:
    def __init__(self, *, dll=None, last_error=None):
        if dll is None:
            try:
                dll = ctypes.WinDLL('Advapi32.dll', use_last_error=True)
            except (AttributeError, OSError):
                raise KeychainError('unavailable') from None
        self.dll = dll
        self.last_error = last_error or ctypes.get_last_error
        pointer = ctypes.POINTER(Credential)
        signatures = {
            'CredWriteW': ([pointer, DWORD], ctypes.c_int32),
            'CredReadW': ([ctypes.c_wchar_p, DWORD, DWORD, ctypes.POINTER(pointer)], ctypes.c_int32),
            'CredDeleteW': ([ctypes.c_wchar_p, DWORD, DWORD], ctypes.c_int32),
            'CredFree': ([ctypes.c_void_p], None),
        }
        for name, (args, result) in signatures.items():
            function = getattr(dll, name)
            function.argtypes, function.restype = args, result

    @staticmethod
    def _target(handle):
        return SERVICE + ':' + valid_handle(handle)

    def _check(self, success):
        if not success:
            status = {1168: 'not_found', 1312: 'unavailable', 5: 'denied'}.get(
                self.last_error(), 'backend_error')
            raise KeychainError(status)

    def store(self, secret, *, handle=None):
        if not isinstance(secret, Secret):
            raise KeychainError('invalid_secret')
        handle = valid_handle(handle) if handle is not None else uuid.uuid4().hex
        raw = secret.reveal().encode('utf-8')
        buffer = (ctypes.c_ubyte * len(raw)).from_buffer_copy(raw)
        credential = Credential(Type=1, TargetName=self._target(handle),
                                CredentialBlobSize=len(raw), CredentialBlob=buffer, Persist=2)
        try:
            self._check(self.dll.CredWriteW(ctypes.byref(credential), 0))
        finally:
            ctypes.memset(buffer, 0, len(raw))
        return handle

    def retrieve(self, handle):
        result = ctypes.POINTER(Credential)()
        self._check(self.dll.CredReadW(self._target(handle), 1, 0, ctypes.byref(result)))
        try:
            if not result:
                raise KeychainError('backend_error')
            row = result.contents
            if not 0 < row.CredentialBlobSize <= MAX_SECRET_BYTES or not row.CredentialBlob:
                raise KeychainError('backend_error')
            return _decode(ctypes.string_at(row.CredentialBlob, row.CredentialBlobSize))
        finally:
            if result:
                self.dll.CredFree(result)

    def delete(self, handle):
        try:
            self._check(self.dll.CredDeleteW(self._target(handle), 1, 0))
        except KeychainError as error:
            if error.status == 'not_found':
                return 'not_found'
            raise
        return 'deleted'


def backend(*, platform=None, which=shutil.which):
    platform = platform or sys.platform
    if platform.startswith('linux'):
        executable = which('secret-tool')
        if executable:
            return SecretTool(executable)
        raise KeychainError('unavailable')
    if platform == 'darwin':
        return MacSecurity('/usr/bin/security')
    if platform == 'win32':
        return WindowsCredentials()
    raise KeychainError('unsupported')
