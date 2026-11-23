"""Linux simulations of CPython's Windows overlapped API; no live endpoint."""
import asyncio
import collections
import contextlib
import ctypes
import pathlib
import socket
import sys
import threading
import types
import unittest
from unittest import mock

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "extensions"))
import vqro_transport as transport


def winerror(code):
    exc = OSError(code, "simulated Windows error")
    exc.winerror = code
    return exc


class Overlapped:
    def __init__(self, api, data=b"", count=0, error=0, pending=False):
        self.api, self.data, self.count, self.error = api, data, count, error
        self.event = threading.Event()
        self.cancelled = False
        self.results = 0
        if not pending:
            self.event.set()
        api.operations.append(self)

    def finish(self, data=b"", error=0):
        self.data, self.count, self.error = data, len(data), error
        self.event.set()

    def cancel(self):
        if not self.event.is_set():
            self.cancelled = True
            self.finish(error=self.api.ERROR_OPERATION_ABORTED)

    def GetOverlappedResult(self, wait):
        if wait and not self.event.wait(2):
            raise AssertionError("overlapped operation never settled")
        self.results += 1
        if self.error not in (0, self.api.ERROR_OPERATION_ABORTED):
            raise winerror(self.error)
        return self.count, self.error

    def getbuffer(self):
        return self.data


class WinAPI(types.ModuleType):
    GENERIC_READ, GENERIC_WRITE = 1, 2
    NULL, OPEN_EXISTING, FILE_FLAG_OVERLAPPED = 0, 3, 0x40000000
    ERROR_PIPE_BUSY, ERROR_SEM_TIMEOUT, ERROR_IO_PENDING = 231, 121, 997
    ERROR_OPERATION_ABORTED, ERROR_BROKEN_PIPE = 995, 109
    INFINITE, WAIT_TIMEOUT, WAIT_OBJECT_0 = 0xffffffff, 258, 0

    def __init__(self):
        super().__init__("_winapi")
        self.operations, self.creates, self.waits, self.writes = [], [], [], []
        self.incoming, self.readers = collections.deque(), collections.deque()
        self.busy, self.closed, self.pending_writes = 0, 0, False
        self.read_started = threading.Event()
        self.write_started = threading.Event()
        self.on_write = None
        self.partial = None
        self.wait_error = None
        self.read_error = None
        self.write_error = None
        self.lock = threading.Lock()

    def CreateFile(self, *args):
        self.creates.append(args)
        if self.busy:
            self.busy -= 1
            raise winerror(self.ERROR_PIPE_BUSY)
        return 42

    def WaitNamedPipe(self, endpoint, timeout):
        self.waits.append((endpoint, timeout))
        if self.wait_error is not None:
            raise winerror(self.wait_error)

    def ReadFile(self, handle, size, *, overlapped):
        assert handle == 42 and overlapped
        if self.read_error is not None:
            raise winerror(self.read_error)
        with self.lock:
            ov = Overlapped(self, pending=True)
            if self.incoming:
                data = self.incoming.popleft()
                ov.finish(data[:size])
                if len(data) > size:
                    self.incoming.appendleft(data[size:])
            else:
                self.readers.append((ov, size))
            self.read_started.set()
            return ov, 0 if ov.event.is_set() else self.ERROR_IO_PENDING

    def feed(self, data):
        with self.lock:
            if self.readers:
                ov, size = self.readers.popleft()
                ov.finish(data[:size])
                if len(data) > size:
                    self.incoming.append(data[size:])
            else:
                self.incoming.append(data)

    def WriteFile(self, handle, data, *, overlapped):
        assert handle == 42 and overlapped
        if self.write_error is not None:
            raise winerror(self.write_error)
        count = len(data) if self.partial is None else min(self.partial, len(data))
        data = bytes(data[:count])
        self.writes.append(data)
        ov = Overlapped(self, count=count, pending=self.pending_writes)
        self.write_started.set()
        if self.on_write:
            self.on_write(data)
        return ov, self.ERROR_IO_PENDING if self.pending_writes else 0

    def WaitForMultipleObjects(self, events, wait_all, timeout):
        assert not wait_all and len(events) == 1
        seconds = 2 if timeout == 0xffffffff else timeout / 1000
        return 0 if events[0].wait(seconds) else 258

    def CloseHandle(self, handle):
        assert handle == 42
        assert all(op.event.is_set() for op in self.operations)
        self.closed += 1


def trusted_queries():
    query = mock.Mock()
    query.file_type.return_value = 3
    query.pipe_flags.return_value = 0
    query.owner_is_current_user.return_value = True
    return query


class PipeTransportTests(unittest.TestCase):
    endpoint = r"\\.\pipe\opaque-not-a-file"

    def setUp(self):
        self.api = WinAPI()
        self.modules = mock.patch.dict(sys.modules, {"_winapi": self.api})
        self.platform = mock.patch.object(transport, "_WINDOWS", True)
        self.modules.start()
        self.platform.start()
        self.query = trusted_queries()
        self.verifier = mock.patch.object(transport, "_NativePipeQueries", return_value=self.query)
        self.verifier.start()
        self.addCleanup(self.modules.stop)
        self.addCleanup(self.platform.stop)
        self.addCleanup(self.verifier.stop)

    def connect(self, **kwargs):
        pipe = transport.connect_endpoint(self.endpoint, **kwargs)
        self.addCleanup(pipe.close)
        return pipe

    def worker(self, fn):
        result = []
        def run():
            try:
                result.append(fn())
            except BaseException as exc:
                result.append(exc)
        thread = threading.Thread(target=run, daemon=True)
        thread.start()
        return thread, result

    def test_busy_uses_wait_and_overlapped_verified_open(self):
        self.api.busy = 2
        self.connect()
        self.assertEqual(self.api.waits, [(self.endpoint, 1000)] * 2)
        self.assertEqual(self.api.creates[-1], (self.endpoint, 3, 0, 0, 3, 0x40110000, 0))
        self.assertEqual(self.query.mock_calls, [mock.call.file_type(42), mock.call.pipe_flags(42),
                                               mock.call.owner_is_current_user(42)])

    def test_untrusted_endpoint_is_refused_before_any_io(self):
        cases = (("file_type", 0), ("file_type", 1), ("file_type", 2),
                 ("pipe_flags", 1), ("owner_is_current_user", False))
        for method, value in cases:
            with self.subTest(method=method, value=value):
                query = trusted_queries()
                getattr(query, method).return_value = value
                with mock.patch.object(transport, "_NativePipeQueries", return_value=query):
                    with self.assertRaisesRegex(OSError, "unavailable: server verification failed") as error:
                        self.connect()
                self.assertIn(repr(self.endpoint), str(error.exception))
                self.assertEqual(self.api.writes, [])
                self.assertEqual(self.api.operations, [])
        self.assertEqual(self.api.closed, len(cases))

    def test_query_failure_is_refused_and_handle_released(self):
        for method in ("file_type", "pipe_flags", "owner_is_current_user"):
            with self.subTest(method=method):
                query = trusted_queries()
                getattr(query, method).side_effect = OSError("query failed")
                with mock.patch.object(transport, "_NativePipeQueries", return_value=query):
                    with self.assertRaisesRegex(OSError, "query failed"):
                        self.connect()
                self.assertEqual(self.api.writes, [])
        self.assertEqual(self.api.closed, 3)

    def test_endpoint_rules_refuse_before_open(self):
        endpoints = (r"\\host\pipe\x", r"\\?\pipe\x", r"C:\x", "", "\\\\.\\pipe\\", r"\\.\pipe\x\y", r"\\.\pipe\x" + chr(0) + "suffix")
        for endpoint in endpoints:
            with self.subTest(endpoint=endpoint):
                with self.assertRaises(OSError) as error:
                    transport.connect_endpoint(endpoint)
                self.assertEqual(str(error.exception), f"unavailable: refusing endpoint {endpoint}: " +
                                 r"Windows core endpoint must be a local named pipe: \\.\pipe\<name> (non-empty name, no further backslash)")
        self.assertEqual(self.api.creates, [])
        self.assertEqual(self.api.writes, [])
        self.query.file_type.assert_not_called()

    def test_local_prefix_is_case_insensitive_and_open_spelling_is_preserved(self):
        endpoint = r"\\.\PiPe\Mixed-Case"
        pipe = transport.connect_endpoint(endpoint)
        self.addCleanup(pipe.close)
        self.assertEqual(self.api.creates[-1][0], endpoint)

    def test_connect_wait_timeout_is_bounded(self):
        self.api.busy = 5
        self.api.wait_error = self.api.ERROR_SEM_TIMEOUT
        with mock.patch.object(transport.time, "monotonic", side_effect=[0, 0, 4]):
            with self.assertRaises(TimeoutError):
                self.connect(timeout=3)
        self.assertEqual(self.api.waits, [(self.endpoint, 1000)])
        self.assertEqual(self.api.closed, 0)

    def test_non_busy_connect_error_is_not_retried(self):
        with mock.patch.object(self.api, "CreateFile", side_effect=winerror(5)) as create:
            with self.assertRaises(OSError):
                self.connect()
        create.assert_called_once()
        self.assertEqual(self.api.waits, [])

    def test_blocked_read_does_not_serialize_write(self):
        pipe = self.connect()
        file = pipe.makefile("rwb", buffering=0)
        thread, result = self.worker(file.readline)
        self.assertTrue(self.api.read_started.wait(1))
        pipe.sendall(b"request\n")
        self.assertEqual(self.api.writes, [b"request\n"])
        self.api.feed(b"reply\n")
        thread.join(1)
        self.assertFalse(thread.is_alive())
        self.assertEqual(result, [b"reply\n"])

    def test_pending_write_completes_independently_of_reader(self):
        pipe = self.connect()
        self.api.pending_writes = True
        thread, result = self.worker(lambda: pipe.sendall(b"request\n"))
        self.assertTrue(self.api.write_started.wait(1))
        self.api.feed(b"reply\n")
        self.assertEqual(pipe.recv(64), b"reply\n")
        self.api.operations[0].event.set()
        thread.join(1)
        self.assertEqual(result, [None])
        self.assertFalse(thread.is_alive())

    def test_shutdown_cancels_read_and_write_then_closes_once(self):
        pipe = self.connect()
        reader, reads = self.worker(lambda: pipe.recv(64))
        self.assertTrue(self.api.read_started.wait(1))
        self.api.pending_writes = True
        writer, writes = self.worker(lambda: pipe.sendall(b"request\n"))
        self.assertTrue(self.api.write_started.wait(1))
        pipe.shutdown(socket.SHUT_RDWR)
        pipe.close()
        for thread in (reader, writer):
            thread.join(1)
            self.assertFalse(thread.is_alive())
        self.assertIsInstance(reads[0], OSError)
        self.assertIsInstance(writes[0], OSError)
        self.assertTrue(all(ov.cancelled and ov.results for ov in self.api.operations))
        self.assertEqual(self.api.closed, 1)
        with self.assertRaises(OSError):
            pipe.sendall(b"must not replay")

    def test_broken_pipe_core_exit_is_eof_and_write_error(self):
        pipe = self.connect()
        file = pipe.makefile("rwb", buffering=0)
        self.api.read_error = self.api.ERROR_BROKEN_PIPE
        self.assertEqual(file.readline(), b"")
        self.api.write_error = self.api.ERROR_BROKEN_PIPE
        with self.assertRaises(OSError):
            pipe.sendall(b"not replayed")
        self.assertEqual(self.api.writes, [])

    def test_core_exit_while_read_pending_returns_eof(self):
        pipe = self.connect()
        reader, result = self.worker(lambda: pipe.recv(64))
        self.assertTrue(self.api.read_started.wait(1))
        self.api.operations[0].finish(error=self.api.ERROR_BROKEN_PIPE)
        reader.join(1)
        self.assertFalse(reader.is_alive())
        self.assertEqual(result, [b""])

    def test_shutdown_cannot_miss_an_operation_being_started(self):
        pipe = self.connect()
        started, release = threading.Event(), threading.Event()
        original = self.api.ReadFile
        def start(*args, **kwargs):
            started.set()
            if not release.wait(1):
                raise AssertionError("read start was not released")
            return original(*args, **kwargs)
        with mock.patch.object(self.api, "ReadFile", start):
            reader, reads = self.worker(lambda: pipe.recv(64))
            self.assertTrue(started.wait(1))
            closer, closes = self.worker(lambda: pipe.shutdown(socket.SHUT_RDWR))
            release.set()
            for thread in (reader, closer):
                thread.join(1)
                self.assertFalse(thread.is_alive())
        self.assertIsInstance(reads[0], OSError)
        self.assertEqual(closes, [None])
        self.assertTrue(self.api.operations[0].cancelled)
        self.assertEqual(self.api.closed, 1)

    def test_interrupted_wait_cancels_and_settles(self):
        pipe = self.connect()
        with mock.patch.object(self.api, "WaitForMultipleObjects", side_effect=KeyboardInterrupt):
            with self.assertRaises(KeyboardInterrupt):
                pipe.recv(64)
        self.assertTrue(self.api.operations[0].cancelled)
        self.assertGreater(self.api.operations[0].results, 0)

    def test_zero_progress_write_fails_instead_of_looping(self):
        pipe = self.connect()
        self.api.partial = 0
        with self.assertRaises(BrokenPipeError):
            pipe.sendall(b"not replayed")
        self.assertEqual(len(self.api.writes), 1)

    def test_io_timeout_cancels_and_settles_without_replay(self):
        pipe = self.connect(timeout=0.001)
        self.api.pending_writes = True
        with self.assertRaises(TimeoutError):
            pipe.sendall(b"indeterminate")
        ov = self.api.operations[0]
        self.assertTrue(ov.cancelled)
        self.assertGreater(ov.results, 0)
        self.assertEqual(self.api.writes, [b"indeterminate"])
        with self.assertRaises(TimeoutError):
            pipe.recv(10)
        self.assertTrue(self.api.operations[-1].cancelled)

    def test_line_limits_fragmentation_and_binary_newlines(self):
        pipe = self.connect()
        file = pipe.makefile("rwb", buffering=0)
        self.api.feed(b"ab")
        self.api.feed(b"c\r\ndef\nghi")
        self.assertEqual(file.readline(2), b"ab")
        self.assertEqual(file.readline(), b"c\r\n")
        self.assertEqual(file.readline(), b"def\n")
        buf = bytearray(3)
        self.assertEqual(file.readinto(buf), 3)
        self.assertEqual(buf, b"ghi")
        self.assertEqual(file.readline(0), b"")
        file.close()
        with self.assertRaises(ValueError):
            file.readline()

    def test_partial_writes_and_text_makefile(self):
        pipe = self.connect()
        self.api.partial = 2
        file = pipe.makefile("rw", encoding="utf-8")
        self.api.feed('hello \u263a\n'.encode())
        self.assertEqual(file.readline(), 'hello \u263a\n')
        file.write('hi \u263a\n')
        file.flush()
        self.assertEqual(b"".join(self.api.writes), 'hi \u263a\n'.encode())
        file.close()


class NativePipeQueryTests(unittest.TestCase):
    """Exercise ctypes call shapes and ownership using powerless DLL mocks."""

    def setUp(self):
        self.kernel, self.security = mock.Mock(), mock.Mock()
        self.query = transport._NativePipeQueries(self.kernel, self.security)
        self.error = mock.patch.object(ctypes, "get_last_error", return_value=122, create=True)
        self.error.start()
        self.addCleanup(self.error.stop)
        self.kernel.GetFileType.return_value = 3
        self.kernel.GetCurrentProcess.return_value = -1
        self.current_sid = self.owner_sid = "S-1-5-21-our-user"
        self.security.EqualSid.side_effect = lambda owner, current: self.owner_sid == self.current_sid
        def output(pointer, value, kind=ctypes.c_uint32):
            ctypes.cast(pointer, ctypes.POINTER(kind))[0] = value
        def pipe_info(handle, flags, *_):
            output(flags, 0)
            return True
        def security_info(handle, kind, info, owner, group, dacl, sacl, descriptor):
            self.assertEqual((handle, kind, info, group, dacl, sacl), (42, 6, 1, None, None, None))
            output(owner, 111, ctypes.c_void_p)
            output(descriptor, 0x100000002, ctypes.c_void_p)
            return 0
        def token(process, access, pointer):
            self.assertEqual((process, access), (-1, 8))
            output(pointer, 202, ctypes.c_void_p)
            return True
        def info(token, info_class, buffer, size, needed):
            self.assertEqual((token.value, info_class), (202, 1))
            output(needed, 24)
            if buffer is None:
                return False
            self.assertEqual(size.value, 24)
            output(buffer, 222, ctypes.c_void_p)
            return True
        self.kernel.GetNamedPipeInfo.side_effect = pipe_info
        self.security.GetSecurityInfo.side_effect = security_info
        self.security.OpenProcessToken.side_effect = token
        self.security.GetTokenInformation.side_effect = info

    def assert_cleanup(self, token=True, descriptor=True):
        handles = [arg.value for (arg,), _ in self.kernel.CloseHandle.call_args_list]
        self.assertEqual(handles, [202] if token else [])
        descriptors = [arg.value for (arg,), _ in self.kernel.LocalFree.call_args_list]
        self.assertEqual(descriptors, [0x100000002] if descriptor else [])
        self.kernel.GetNamedPipeServerProcessId.assert_not_called()
        self.kernel.OpenProcess.assert_not_called()

    def test_connected_owner_matches_exact_token_user_and_buffers_are_released(self):
        self.assertEqual(self.query.file_type(42), 3)
        self.assertEqual(self.query.pipe_flags(42), 0)
        self.assertTrue(self.query.owner_is_current_user(42))
        owner, current = self.security.EqualSid.call_args.args
        self.assertEqual((owner.value, current), (111, 222))
        self.assert_cleanup()
        self.assertIs(self.kernel.LocalFree.restype, ctypes.c_void_p)
        self.assertIs(self.kernel.GetCurrentProcess.restype, ctypes.c_void_p)

    def test_foreign_user_and_administrators_group_owners_are_refused(self):
        for owner in ("S-1-5-21-other-user", "S-1-5-32-544"):
            with self.subTest(owner=owner):
                self.owner_sid = owner
                self.assertFalse(self.query.owner_is_current_user(42))
                self.assertEqual(self.kernel.LocalFree.call_count, self.security.EqualSid.call_count)

    def test_pipe_query_failure_raises(self):
        with mock.patch.object(self.kernel, "GetNamedPipeInfo", return_value=False):
            with self.assertRaisesRegex(OSError, "GetNamedPipeInfo"):
                self.query.pipe_flags(42)

    def test_owner_query_error_uses_returned_status_and_does_not_query_token(self):
        self.security.GetSecurityInfo.side_effect = None
        self.security.GetSecurityInfo.return_value = 5
        with self.assertRaisesRegex(OSError, "GetSecurityInfo failed.*5"):
            self.query.owner_is_current_user(42)
        self.security.OpenProcessToken.assert_not_called()
        self.assert_cleanup(token=False, descriptor=False)

    def test_missing_owner_refuses_and_releases_descriptor(self):
        original = self.security.GetSecurityInfo.side_effect
        def missing(*args):
            original(*args)
            ctypes.cast(args[3], ctypes.POINTER(ctypes.c_void_p))[0] = None
            return 0
        self.security.GetSecurityInfo.side_effect = missing
        with self.assertRaisesRegex(OSError, "no pipe owner"):
            self.query.owner_is_current_user(42)
        self.assert_cleanup(token=False)

    def test_token_open_failure_releases_descriptor(self):
        self.security.OpenProcessToken.side_effect = None
        self.security.OpenProcessToken.return_value = False
        with self.assertRaisesRegex(OSError, "OpenProcessToken"):
            self.query.owner_is_current_user(42)
        self.assert_cleanup(token=False)

    def test_token_info_failure_releases_token_and_descriptor(self):
        self.security.GetTokenInformation.side_effect = None
        self.security.GetTokenInformation.return_value = False
        with mock.patch.object(ctypes, "get_last_error", return_value=5):
            with self.assertRaisesRegex(OSError, "GetTokenInformation"):
                self.query.owner_is_current_user(42)
        self.assert_cleanup()


class UnixTransportTests(unittest.TestCase):
    def test_socket_connect_and_timeout_behavior_unchanged(self):
        with mock.patch.object(transport, "_WINDOWS", False), mock.patch.object(transport.socket, "socket") as factory:
            sock = factory.return_value
            self.assertIs(transport.connect_endpoint("opaque"), sock)
            factory.assert_called_once_with(socket.AF_UNIX)
            self.assertEqual(sock.mock_calls, [mock.call.connect("opaque")])
            sock.reset_mock()
            transport.connect_endpoint("opaque", timeout=3)
            self.assertEqual(sock.mock_calls, [mock.call.settimeout(3), mock.call.connect("opaque")])

    def test_failed_connect_releases_socket(self):
        with mock.patch.object(transport, "_WINDOWS", False), mock.patch.object(transport.socket, "socket") as factory:
            factory.return_value.connect.side_effect = OSError("no core")
            with self.assertRaises(OSError):
                transport.connect_endpoint("opaque")
            factory.return_value.close.assert_called_once()


class AsyncTransportTests(unittest.IsolatedAsyncioTestCase):
    endpoint = r"\\.\PiPe\opaque"

    async def connect(self):
        return await transport.open_endpoint(self.endpoint, limit=123)

    def setUp(self):
        self.patches = contextlib.ExitStack()
        self.addCleanup(self.patches.close)
        self.api = WinAPI()
        self.query = trusted_queries()
        self.pipe_handles = []
        api, handles = self.api, self.pipe_handles
        class PipeHandle:
            def __init__(self, handle):
                self.handle = handle
                handles.append(self)
            def close(self):
                if self.handle is not None:
                    api.CloseHandle(self.handle)
                    self.handle = None
        self.utils = types.ModuleType("asyncio.windows_utils")
        self.utils.PipeHandle = PipeHandle
        self.patches.enter_context(mock.patch.dict(sys.modules, {"_winapi": self.api, "asyncio.windows_utils": self.utils}))
        self.patches.enter_context(mock.patch.object(transport, "_NativePipeQueries", return_value=self.query))
        self.patches.enter_context(mock.patch.object(transport, "_WINDOWS", True))

    async def test_unix_calls_existing_asyncio_api(self):
        with mock.patch.object(transport, "_WINDOWS", False), mock.patch.object(transport.asyncio, "open_unix_connection", new_callable=mock.AsyncMock) as connect:
            connect.return_value = ("reader", "writer")
            self.assertEqual(await transport.open_endpoint("opaque", limit=123), ("reader", "writer"))
            connect.assert_awaited_once_with("opaque", limit=123)

    async def test_windows_uses_proactor_pipe_stream_protocol(self):
        loop = asyncio.get_running_loop()
        pipe_transport = mock.Mock()
        pipe_transport.is_closing.return_value = False
        protocols = []
        def create(pipe, protocol, *, extra):
            self.assertEqual(extra, {"addr": self.endpoint})
            self.assertEqual(pipe.handle, 42)
            self.query.owner_is_current_user.assert_called_once_with(42)
            protocols.append(protocol)
            protocol.connection_made(pipe_transport)
            return pipe_transport
        with mock.patch.object(loop, "_make_duplex_pipe_transport", create, create=True):
            reader, writer = await self.connect()
            protocols[0].data_received(b"reply\n")
            self.assertEqual(await reader.readline(), b"reply\n")
            writer.write(b"request\n")
            await writer.drain()
            pipe_transport.write.assert_called_once_with(b"request\n")
            writer.close()
            pipe_transport.is_closing.return_value = True
            protocols[0].connection_lost(None)
            await writer.wait_closed()
            self.assertEqual(await reader.read(), b"")
        self.assertEqual(self.api.creates[-1][5], 0x40110000)
        self.assertEqual(self.api.creates[-1][0], self.endpoint)
        self.pipe_handles[0].close()  # fake transport's owned handle

    async def test_endpoint_rules_refuse_before_open_or_proactor_handoff(self):
        endpoints = (r"\\host\pipe\x", r"\\?\pipe\x", r"C:\x", "", "\\\\.\\pipe\\", r"\\.\pipe\x\y", r"\\.\pipe\x" + chr(0) + "suffix")
        with mock.patch.object(asyncio.get_running_loop(), "_make_duplex_pipe_transport", create=True) as make:
            for endpoint in endpoints:
                with self.subTest(endpoint=endpoint), self.assertRaisesRegex(OSError, "Windows core endpoint must be a local named pipe"):
                    await transport.open_endpoint(endpoint, limit=123)
            make.assert_not_called()
        self.assertEqual(self.api.creates, [])
        self.assertEqual(self.api.writes, [])
        self.assertEqual(self.pipe_handles, [])

    async def test_untrusted_pipe_never_reaches_proactor_or_writes(self):
        loop = asyncio.get_running_loop()
        for method, value in (("owner_is_current_user", False), ("file_type", 1), ("pipe_flags", 1)):
            with self.subTest(method=method):
                query = trusted_queries()
                getattr(query, method).return_value = value
                with mock.patch.object(transport, "_NativePipeQueries", return_value=query), mock.patch.object(loop, "_make_duplex_pipe_transport", create=True) as make:
                    with self.assertRaisesRegex(OSError, "server verification failed"):
                        await self.connect()
                    make.assert_not_called()
        self.assertEqual(self.api.closed, 3)
        self.assertEqual(self.api.writes, [])
        self.assertEqual(self.pipe_handles, [])

    async def test_query_error_never_reaches_proactor(self):
        self.query.owner_is_current_user.side_effect = OSError("query failed")
        with mock.patch.object(asyncio.get_running_loop(), "_make_duplex_pipe_transport", create=True) as make:
            with self.assertRaisesRegex(OSError, "query failed"):
                await self.connect()
            make.assert_not_called()
        self.assertEqual(self.api.closed, 1)
        self.assertEqual(self.api.writes, [])

    async def test_busy_connect_can_be_cancelled_without_open_handle(self):
        self.api.busy = 10
        with mock.patch.object(asyncio.get_running_loop(), "_make_duplex_pipe_transport", create=True) as make, mock.patch.object(transport.asyncio, "sleep", side_effect=asyncio.CancelledError) as sleep:
            with self.assertRaises(asyncio.CancelledError):
                await self.connect()
            sleep.assert_awaited_once_with(0.002)
            make.assert_not_called()
        self.assertEqual(len(self.api.creates), 1)
        self.assertEqual(self.api.closed, 0)
        self.query.file_type.assert_not_called()

    async def test_failed_proactor_handoff_releases_verified_handle(self):
        with mock.patch.object(asyncio.get_running_loop(), "_make_duplex_pipe_transport", side_effect=OSError("handoff failed"), create=True):
            with self.assertRaisesRegex(OSError, "handoff failed"):
                await self.connect()
        self.assertEqual(self.api.closed, 1)
        self.assertEqual(self.api.writes, [])

    async def test_non_proactor_loop_refuses_before_open(self):
        with mock.patch.object(asyncio.get_running_loop(), "_make_duplex_pipe_transport", None, create=True):
            with self.assertRaisesRegex(OSError, "unsupported Python version for Windows async pipe"):
                await self.connect()
        self.assertEqual(self.api.creates, [])

    async def test_incompatible_proactor_signature_refuses_before_open(self):
        def incompatible(pipe):
            raise AssertionError("incompatible factory must never be called")
        with mock.patch.object(asyncio.get_running_loop(), "_make_duplex_pipe_transport", incompatible, create=True):
            with self.assertRaisesRegex(OSError, "unsupported Python version for Windows async pipe"):
                await self.connect()
        self.assertEqual(self.api.creates, [])

    async def test_missing_pipe_handle_wrapper_refuses_before_open(self):
        with mock.patch.object(self.utils, "PipeHandle", new=None):
            # A wrapper must be callable before we transfer a native handle.
            with mock.patch.object(asyncio.get_running_loop(), "_make_duplex_pipe_transport", create=True):
                with self.assertRaisesRegex(OSError, "unsupported Python version for Windows async pipe"):
                    await self.connect()
        self.assertEqual(self.api.creates, [])


if __name__ == "__main__":
    unittest.main()
