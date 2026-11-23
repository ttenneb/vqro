"""Local core byte streams (stdlib, CPython 3.11+).

Windows endpoint names must be in the local pipe namespace (D22).

Unix keeps the socket path. Windows uses overlapped pipe I/O so a reader thread
cannot stall writes. Async callers use the Windows proactor pipe transport.
See docs/research/windows-python-transport.md for CPython API evidence.
"""
import asyncio
import io
import math
import os
import socket
import threading
import time


_WINDOWS = os.name == "nt"
# winbase.h: SECURITY_IDENTIFICATION = SecurityIdentification (1) << 16;
# SECURITY_SQOS_PRESENT = 0x100000. _winapi does not export these constants.
_SECURITY_IDENTIFICATION = 0x00010000
_SECURITY_SQOS_PRESENT = 0x00100000
_FILE_TYPE_PIPE = 3
_PIPE_SERVER_END = 1
_LOCAL_PIPE_PREFIX = "\\\\.\\pipe\\"
_ENDPOINT_RULE = "Windows core endpoint must be a local named pipe: \\\\.\\pipe\\<name> (non-empty name, no further backslash)"


def _validate_windows_endpoint(endpoint):
    if isinstance(endpoint, str):
        prefix, name = endpoint[:len(_LOCAL_PIPE_PREFIX)], endpoint[len(_LOCAL_PIPE_PREFIX):]
        if (prefix.isascii() and prefix.lower() == _LOCAL_PIPE_PREFIX and name
                and "\\" not in name and "\x00" not in name):
            return
    raise OSError(f"unavailable: refusing endpoint {endpoint}: {_ENDPOINT_RULE}")


def _verify_pipe_server(handle, endpoint):
    """Authenticate the connected server before any protocol/credential write."""
    try:
        query = _NativePipeQueries()
        if query.file_type(handle) != _FILE_TYPE_PIPE:
            raise OSError("not a pipe")
        if query.pipe_flags(handle) & _PIPE_SERVER_END:
            raise OSError("not the client end of a named pipe")
        if not query.owner_is_current_user(handle):
            raise OSError("pipe owner is not the current user")
    except Exception as exc:
        raise OSError(f"core endpoint {endpoint!r} unavailable: server verification failed: {exc}") from exc


def _open_verified_pipe(endpoint, api):
    _validate_windows_endpoint(endpoint)
    handle = api.CreateFile(
        endpoint, api.GENERIC_READ | api.GENERIC_WRITE,
        0, api.NULL, api.OPEN_EXISTING,
        api.FILE_FLAG_OVERLAPPED | _SECURITY_SQOS_PRESENT | _SECURITY_IDENTIFICATION,
        api.NULL)
    try:
        _verify_pipe_server(handle, endpoint)
    except BaseException:
        api.CloseHandle(handle)
        raise
    return handle


class _NativePipeQueries:
    """Small ctypes boundary; all queries fail closed and own their handles."""

    def __init__(self, kernel32=None, advapi32=None):
        import ctypes as ct
        self.ct = ct
        self.kernel = kernel32 if kernel32 is not None else ct.WinDLL("kernel32", use_last_error=True)
        self.security = advapi32 if advapi32 is not None else ct.WinDLL("advapi32", use_last_error=True)
        # Fixed-width Win32 types, including pointer-width HANDLE/PSID. Do not
        # let ctypes' default int return type truncate handles on 64-bit Windows.
        self.DWORD, handle, boolean, pointer = ct.c_uint32, ct.c_void_p, ct.c_int, ct.c_void_p
        dword_ptr = ct.POINTER(self.DWORD)
        for dll, name, args, result in (
            (self.kernel, "GetFileType", [handle], self.DWORD),
            (self.kernel, "GetNamedPipeInfo", [handle, dword_ptr, dword_ptr, dword_ptr, dword_ptr], boolean),
            (self.kernel, "GetCurrentProcess", [], handle),
            (self.kernel, "CloseHandle", [handle], boolean),
            (self.kernel, "LocalFree", [pointer], pointer),
            (self.security, "GetSecurityInfo", [handle, ct.c_int, self.DWORD,
                                               ct.POINTER(pointer), ct.POINTER(pointer),
                                               ct.POINTER(pointer), ct.POINTER(pointer), ct.POINTER(pointer)], self.DWORD),
            (self.security, "OpenProcessToken", [handle, self.DWORD, ct.POINTER(handle)], boolean),
            (self.security, "GetTokenInformation", [handle, ct.c_int, pointer, self.DWORD, dword_ptr], boolean),
            (self.security, "EqualSid", [pointer, pointer], boolean),
        ):
            fn = getattr(dll, name)
            fn.argtypes, fn.restype = args, result

    def _fail(self, operation):
        raise OSError(f"{operation} failed (Windows error {self.ct.get_last_error()})")

    def file_type(self, handle):
        return self.kernel.GetFileType(handle)

    def pipe_flags(self, handle):
        flags = self.DWORD()
        if not self.kernel.GetNamedPipeInfo(handle, self.ct.byref(flags), None, None, None):
            self._fail("GetNamedPipeInfo")
        return flags.value

    def _token_user(self, process):
        ct = self.ct
        token = ct.c_void_p()
        # TOKEN_QUERY = 0x0008, TokenUser = 1 (winnt.h).
        if not self.security.OpenProcessToken(process, 0x0008, ct.byref(token)):
            self._fail("OpenProcessToken")
        try:
            size = self.DWORD()
            if self.security.GetTokenInformation(token, 1, None, 0, ct.byref(size)) or ct.get_last_error() != 122:
                self._fail("GetTokenInformation(size)")  # ERROR_INSUFFICIENT_BUFFER
            if size.value < ct.sizeof(ct.c_void_p):
                raise OSError("invalid TokenUser size")
            buffer = ct.create_string_buffer(size.value)
            if not self.security.GetTokenInformation(token, 1, buffer, size, ct.byref(size)):
                self._fail("GetTokenInformation(TokenUser)")
            return buffer  # keep the SID storage alive through EqualSid
        finally:
            self.kernel.CloseHandle(token)

    def owner_is_current_user(self, handle):
        ct = self.ct
        owner, descriptor = ct.c_void_p(), ct.c_void_p()
        try:
            # SE_KERNEL_OBJECT = 6; OWNER_SECURITY_INFORMATION = 1.
            # The owner pointer is inside the returned descriptor, which must
            # remain allocated until EqualSid has completed (then LocalFree).
            error = self.security.GetSecurityInfo(handle, 6, 1, ct.byref(owner), None, None, None, ct.byref(descriptor))
            if error:
                raise OSError(f"GetSecurityInfo failed (Windows error {error})")
            if not owner.value or not descriptor.value:
                raise OSError("GetSecurityInfo returned no pipe owner")
            current = self._token_user(self.kernel.GetCurrentProcess())
            # TOKEN_USER starts with SID_AND_ATTRIBUTES, whose first member is PSID.
            sid = ct.POINTER(ct.c_void_p)
            return bool(self.security.EqualSid(owner, ct.cast(current, sid)[0]))
        finally:
            if descriptor.value:
                self.kernel.LocalFree(descriptor)


def connect_endpoint(endpoint, *, timeout=None):
    """Return the socket subset used by bundled synchronous core clients."""
    if _WINDOWS:
        import _winapi
        return _PipeSocket(endpoint, timeout, _winapi)
    sock = socket.socket(socket.AF_UNIX)
    try:
        if timeout is not None:
            sock.settimeout(timeout)
        sock.connect(endpoint)
        return sock
    except BaseException:
        sock.close()
        raise


async def open_endpoint(endpoint, *, limit):
    """Return asyncio streams; Windows requires its default proactor loop."""
    if not _WINDOWS:
        return await asyncio.open_unix_connection(endpoint, limit=limit)
    _validate_windows_endpoint(endpoint)
    import _winapi
    import inspect
    loop = asyncio.get_running_loop()
    make_transport = getattr(loop, "_make_duplex_pipe_transport", None)
    try:
        from asyncio.windows_utils import PipeHandle
        if not callable(PipeHandle):
            raise TypeError("missing PipeHandle wrapper")
        inspect.signature(make_transport).bind(None, None, extra={})
    except (ImportError, TypeError, ValueError):
        raise OSError(f"core endpoint {endpoint!r} unavailable: unsupported Python version for Windows async pipe (CPython proactor factory required)") from None
    # CPython's create_pipe_connection/ConnectPipe omits SQOS. Open with explicit
    # SQOS and authenticate ourselves, then use its same duplex-transport factory.
    delay = 0.001
    while True:
        try:
            handle = _open_verified_pipe(endpoint, _winapi)
            break
        except OSError as exc:
            if getattr(exc, "winerror", None) != _winapi.ERROR_PIPE_BUSY:
                raise
            delay = min(delay * 2, 0.1)
            await asyncio.sleep(delay)
    pipe = None
    try:
        pipe = PipeHandle(handle)
        reader = asyncio.StreamReader(limit=limit, loop=loop)
        protocol = asyncio.StreamReaderProtocol(reader, loop=loop)
        transport = make_transport(pipe, protocol, extra={"addr": endpoint})
        return reader, asyncio.StreamWriter(transport, protocol, reader, loop)
    except BaseException:
        if pipe is None:
            _winapi.CloseHandle(handle)
        else:
            pipe.close()
        raise


class _PipeSocket:
    """Full-duplex byte-mode named pipe, not a general socket implementation.

    One reader and one writer may run concurrently. shutdown(SHUT_RDWR) and
    close() cancel both directions, drain cancellation, then release the handle.
    No message framing, endpoint parsing, reconnect or effect replay lives here.
    """

    def __init__(self, endpoint, timeout, api):
        self._api = api
        self._state = threading.Lock()
        self._pending = set()
        self._handle = None
        self.settimeout(timeout)
        end = None if timeout is None else time.monotonic() + timeout
        while True:
            try:
                self._handle = _open_verified_pipe(endpoint, api)
                break
            except OSError as exc:
                if getattr(exc, "winerror", None) != api.ERROR_PIPE_BUSY:
                    raise
                remaining = None if end is None else end - time.monotonic()
                if remaining is not None and remaining <= 0:
                    raise TimeoutError("core pipe connect timed out") from exc
                try:
                    api.WaitNamedPipe(endpoint, 1000 if remaining is None else max(1, min(1000, math.ceil(remaining * 1000))))
                except OSError as wait_error:
                    if wait_error.winerror not in (api.ERROR_SEM_TIMEOUT, api.ERROR_PIPE_BUSY):
                        raise

    def settimeout(self, timeout):
        if timeout is not None and (not math.isfinite(timeout) or timeout <= 0):
            raise ValueError("pipe timeout must be positive or None (blocking)")
        self._timeout = timeout

    def _io(self, data, *, reading, deadline=None):
        api = self._api
        ov = None
        try:
            # Registration is atomic with starting I/O: shutdown cannot miss it.
            with self._state:
                if self._handle is None:
                    raise OSError("core pipe is closed")
                fn = api.ReadFile if reading else api.WriteFile
                ov, error = fn(self._handle, data, overlapped=True)
                self._pending.add(ov)
            try:
                if error == api.ERROR_IO_PENDING:
                    seconds = self._timeout if deadline is None else max(0, deadline - time.monotonic())
                    ms = api.INFINITE if seconds is None else min(api.INFINITE - 1, math.ceil(seconds * 1000))
                    result = api.WaitForMultipleObjects([ov.event], False, ms)
                    if result == api.WAIT_TIMEOUT:
                        raise TimeoutError("core pipe I/O timed out; outcome indeterminate")
                    if result != api.WAIT_OBJECT_0:
                        raise OSError("core pipe wait failed")
            except BaseException:
                ov.cancel()
                raise
            finally:
                # Always settle overlapped I/O before its buffers can be freed.
                count, error = ov.GetOverlappedResult(True)
            if error:
                raise OSError(error, "core pipe I/O failed")
            return bytes(ov.getbuffer()) if reading else count
        except OSError as exc:
            code = getattr(exc, "winerror", None) or exc.errno
            if reading and code == api.ERROR_BROKEN_PIPE:
                return b""
            raise
        finally:
            if ov is not None:
                with self._state:
                    self._pending.discard(ov)

    def recv(self, size):
        if size == 0:
            return b""
        return self._io(size, reading=True)

    def sendall(self, data):
        data = memoryview(data)
        end = None if self._timeout is None else time.monotonic() + self._timeout
        while data:
            written = self._io(data, reading=False, deadline=end)
            if not written:
                raise BrokenPipeError("core pipe write made no progress")
            data = data[written:]

    def makefile(self, mode="rwb", buffering=-1, *, encoding=None):
        raw = _PipeFile(self)
        if "b" in mode:
            if buffering == 0:
                return raw
            return io.BufferedRWPair(raw, raw)
        return io.TextIOWrapper(io.BufferedRWPair(raw, raw), encoding=encoding or "utf-8", newline="\n")

    def shutdown(self, how):
        if how != socket.SHUT_RDWR:
            raise ValueError("named pipe supports only full shutdown")
        self.close()

    def close(self):
        with self._state:
            handle, self._handle = self._handle, None
            if handle is None:
                return
            try:
                for ov in self._pending:
                    ov.cancel()
                for ov in self._pending:
                    try:
                        ov.GetOverlappedResult(True)
                    except OSError:
                        pass  # cancellation/broken pipe is expected on close
            finally:
                self._api.CloseHandle(handle)


class _PipeFile(io.RawIOBase):
    """Binary file facade with chunked line reads, never newline translation."""

    def __init__(self, pipe):
        super().__init__()
        self._pipe = pipe
        self._buffer = bytearray()

    def readable(self):
        return True

    def writable(self):
        return True

    def readinto(self, buf):
        self._checkClosed()
        if not buf:
            return 0
        if not self._buffer:
            self._buffer.extend(self._pipe.recv(len(buf)))
        count = min(len(buf), len(self._buffer))
        buf[:count] = self._buffer[:count]
        del self._buffer[:count]
        return count

    def readline(self, size=-1):
        self._checkClosed()
        parts = bytearray()
        while size < 0 or len(parts) < size:
            if not self._buffer:
                chunk = self._pipe.recv(65536)
                if not chunk:
                    break
                self._buffer.extend(chunk)
            cap = len(self._buffer) if size < 0 else min(len(self._buffer), size - len(parts))
            newline = self._buffer.find(b"\n", 0, cap)
            count = newline + 1 if newline >= 0 else cap
            parts.extend(self._buffer[:count])
            del self._buffer[:count]
            if newline >= 0:
                break
        return bytes(parts)

    def write(self, data):
        self._checkClosed()
        self._pipe.sendall(data)
        return len(data)

    def close(self):
        try:
            self._pipe.close()
        finally:
            super().close()
