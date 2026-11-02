"""Standard-library Win32 SID and byte-mode named-pipe client for Tasks.

No shell, reconnect after submission, or authority supplied through environment.
Native calls are isolated here; import is safe on Unix for contract tests.
"""
import ctypes as c
import time

DWORD = c.c_uint32
HANDLE = c.c_void_p
BOOL = c.c_int32
INVALID_HANDLE = c.c_void_p(-1).value
ERROR_IO_PENDING = 997
ERROR_PIPE_BUSY = 231
WAIT_TIMEOUT = 258
MAX_REPLY = 1024 * 1024


class Overlapped(c.Structure):
    _fields_ = [("Internal", c.c_size_t), ("InternalHigh", c.c_size_t),
                ("Offset", DWORD), ("OffsetHigh", DWORD), ("hEvent", HANDLE)]


class TokenUser(c.Structure):
    _fields_ = [("Sid", HANDLE), ("Attributes", DWORD)]


def _function(dll, name, result, *args):
    function = getattr(dll, name)
    function.restype = result
    function.argtypes = args
    return function


class Kernel:
    def __init__(self):
        self.dll = c.WinDLL("kernel32", use_last_error=True)
        self.create = _function(self.dll, "CreateFileW", HANDLE, c.c_wchar_p, DWORD, DWORD, HANDLE, DWORD, DWORD, HANDLE)
        self.wait_pipe = _function(self.dll, "WaitNamedPipeW", BOOL, c.c_wchar_p, DWORD)
        self.close = _function(self.dll, "CloseHandle", BOOL, HANDLE)
        self.event = _function(self.dll, "CreateEventW", HANDLE, HANDLE, BOOL, BOOL, c.c_wchar_p)
        self.wait = _function(self.dll, "WaitForSingleObject", DWORD, HANDLE, DWORD)
        self.read = _function(self.dll, "ReadFile", BOOL, HANDLE, HANDLE, DWORD, c.POINTER(DWORD), c.POINTER(Overlapped))
        self.write = _function(self.dll, "WriteFile", BOOL, HANDLE, HANDLE, DWORD, c.POINTER(DWORD), c.POINTER(Overlapped))
        self.result = _function(self.dll, "GetOverlappedResult", BOOL, HANDLE, c.POINTER(Overlapped), c.POINTER(DWORD), BOOL)
        self.cancel = _function(self.dll, "CancelIoEx", BOOL, HANDLE, c.POINTER(Overlapped))

    @staticmethod
    def error():
        return c.WinError(c.get_last_error())


def current_sid():
    kernel = Kernel()
    security = c.WinDLL("advapi32", use_last_error=True)
    current = _function(kernel.dll, "GetCurrentProcess", HANDLE)
    open_token = _function(security, "OpenProcessToken", BOOL, HANDLE, DWORD, c.POINTER(HANDLE))
    information = _function(security, "GetTokenInformation", BOOL, HANDLE, DWORD, HANDLE, DWORD, c.POINTER(DWORD))
    convert = _function(security, "ConvertSidToStringSidW", BOOL, HANDLE, c.POINTER(c.c_wchar_p))
    free = _function(kernel.dll, "LocalFree", HANDLE, HANDLE)
    token = HANDLE()
    if not open_token(current(), 8, c.byref(token)):  # TOKEN_QUERY
        raise kernel.error()
    try:
        needed = DWORD()
        information(token, 1, None, 0, c.byref(needed))  # TokenUser
        if not c.sizeof(TokenUser) <= needed.value <= 65536:
            raise OSError("invalid TokenUser size")
        buffer = c.create_string_buffer(needed.value)
        if not information(token, 1, buffer, len(buffer), c.byref(needed)):
            raise kernel.error()
        user = c.cast(buffer, c.POINTER(TokenUser)).contents
        text = c.c_wchar_p()
        if not convert(user.Sid, c.byref(text)):
            raise kernel.error()
        try:
            from local_platform import valid_sid
            sid = text.value
            if not valid_sid(sid):
                raise OSError("invalid TokenUser SID")
            return sid
        finally:
            free(c.cast(text, HANDLE))
    finally:
        kernel.close(token)


def _remaining(deadline):
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise TimeoutError("named-pipe deadline elapsed; submitted writes may have committed")
    return max(1, min(0xfffffffe, int(remaining * 1000)))


def _transfer(kernel, handle, buffer, size, writing, deadline):
    event = kernel.event(None, True, False, None)
    if not event:
        raise kernel.error()
    operation = Overlapped(hEvent=event)
    transferred = DWORD()
    pending = False
    try:
        function = kernel.write if writing else kernel.read
        if not function(handle, buffer, size, c.byref(transferred), c.byref(operation)):
            if c.get_last_error() != ERROR_IO_PENDING:
                raise kernel.error()
            pending = True
            wait = kernel.wait(event, _remaining(deadline))
            if wait != 0:
                if wait == WAIT_TIMEOUT:
                    raise TimeoutError("named-pipe deadline elapsed; action outcome may be unknown")
                raise kernel.error()
            if not kernel.result(handle, c.byref(operation), c.byref(transferred), False):
                raise kernel.error()
            pending = False
        if transferred.value > size:
            raise OSError("invalid named-pipe transfer count")
        return transferred.value
    finally:
        if pending:
            # Cancellation races completion. Always drain it BEFORE dropping the
            # buffer/OVERLAPPED/event, including ERROR_NOT_FOUND from CancelIoEx.
            kernel.cancel(handle, c.byref(operation))
            kernel.result(handle, c.byref(operation), c.byref(transferred), True)
        kernel.close(event)


def pipe_path(name):
    # Match ipc::connect_local_stream's GenericNamespaced mapping exactly. The
    # supplied socket context is a marker-path namespace, not an NPFS pathname.
    # Always prepend the local host, even if the namespace resembles a path.
    if not isinstance(name, str) or not name or "\x00" in name or len(name) > 32000:
        raise ValueError("invalid Windows local socket namespace")
    return "\\\\.\\pipe\\" + name


def request(path, frame, timeout=6, kernel=None):
    path = pipe_path(path)
    if not frame.endswith(b"\n") or len(frame) > 262144:
        raise ValueError("API request frame exceeds bound")
    kernel = Kernel() if kernel is None else kernel
    deadline = time.monotonic() + timeout
    handle = INVALID_HANDLE
    # Retry only pipe admission BEFORE any request bytes have been submitted.
    while handle == INVALID_HANDLE:
        _remaining(deadline)
        handle = kernel.create(path, 0xc0000000, 0, None, 3, 0x40000000, None)
        if handle != INVALID_HANDLE:
            break
        if c.get_last_error() != ERROR_PIPE_BUSY:
            raise kernel.error()
        if not kernel.wait_pipe(path, _remaining(deadline)):
            raise kernel.error()
    try:
        sent = 0
        while sent < len(frame):
            _remaining(deadline)
            buffer = c.create_string_buffer(frame[sent:])
            count = _transfer(kernel, handle, buffer, len(frame) - sent, True, deadline)
            if not count:
                raise OSError("named pipe closed during request; action outcome unknown")
            sent += count
        data = bytearray()
        while b"\n" not in data:
            _remaining(deadline)
            buffer = c.create_string_buffer(min(65536, MAX_REPLY + 1 - len(data)))
            count = _transfer(kernel, handle, buffer, len(buffer), False, deadline)
            if not count:
                raise OSError("API closed without reply; action outcome unknown")
            data.extend(buffer.raw[:count])
            if len(data) > MAX_REPLY:
                raise OSError("API reply exceeds CLI bound; action outcome unknown")
        return bytes(data.partition(b"\n")[0])
    finally:
        kernel.close(handle)
