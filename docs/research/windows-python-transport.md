# Python local endpoint transport (2026-11-22)

Slice C implementation: `extensions/vqro_transport.py`, stdlib-only CPython 3.11+.
Unix endpoints remain opaque and return the original socket. Windows endpoints
are validated as local pipe names (D22); synchronous callers use an overlapped
byte-mode pipe and asyncio callers use the proactor pipe transport.
No request framing, reconnect, lifetime policy, or replay is added by transport.

## Evidence

Microsoft's [Synchronous and Overlapped Pipe I/O](https://learn.microsoft.com/en-us/windows/win32/ipc/synchronous-and-overlapped-input-and-output)
requires an overlapped handle plus a separate `OVERLAPPED` and event per concurrent
operation, explicitly enabling simultaneous read/write on one pipe. We use one
CPython overlapped object per operation and wait on that object's event.

CPython 3.11.9 source (pinned rather than the moving main branch):

- [`multiprocessing/connection.py:268–308`](https://github.com/python/cpython/blob/v3.11.9/Lib/multiprocessing/connection.py#L268-L308):
  `PipeConnection` requires `FILE_FLAG_OVERLAPPED`, writes with
  `WriteFile(..., overlapped=True)`, waits on the overlapped event, cancels on
  interruption/close, and settles with `GetOverlappedResult(True)`.
- [`connection.py:310–344`](https://github.com/python/cpython/blob/v3.11.9/Lib/multiprocessing/connection.py#L310-L344):
  the equivalent read sequence and broken-pipe-to-EOF translation.
- [`connection.py:712–737`](https://github.com/python/cpython/blob/v3.11.9/Lib/multiprocessing/connection.py#L712-L737):
  `WaitNamedPipe`, busy/timeout handling, and `CreateFile` flags/access. We do
  **not** adopt multiprocessing's message mode or length-prefixed/pickled framing;
  vqro is a byte stream with LF-delimited JSON.
- [`asyncio/windows_events.py:336–343`](https://github.com/python/cpython/blob/v3.11.9/Lib/asyncio/windows_events.py#L336-L343):
  `ProactorEventLoop.create_pipe_connection` returns a duplex pipe transport and
  protocol. [`:687–704`](https://github.com/python/cpython/blob/v3.11.9/Lib/asyncio/windows_events.py#L687-L704)
  handles busy pipe instances asynchronously before constructing `PipeHandle`.
- [`Modules/_winapi.c:244–283`](https://github.com/python/cpython/blob/v3.11.9/Modules/_winapi.c#L244-L283)
  completes/resizes read buffers on `GetOverlappedResult`, and
  [`:304–324`](https://github.com/python/cpython/blob/v3.11.9/Modules/_winapi.c#L304-L324)
  cancels pending overlapped operations with `CancelIoEx` (tolerates completion
  racing cancellation). [`:2355–2385`](https://github.com/python/cpython/blob/v3.11.9/Modules/_winapi.c#L2355-L2385)
  lists the constants exported in 3.11; notably not `ERROR_PIPE_NOT_CONNECTED`.

The synchronous implementation starts I/O and registers its overlapped object under
a short state lock, then waits **outside** that lock. Shutdown cancels outstanding
reads and writes, waits for cancellation to finish before releasing buffers/handle,
and prevents new I/O. A blocked reader therefore never holds the writer's lock.
The existing managed-stdin lifetime watcher in `vqro_ext.py` remains unchanged.

## Authentication and impersonation ceiling (Round 3 supersedes the PID proof)

Both Python connection paths now open with `FILE_FLAG_OVERLAPPED |
SECURITY_SQOS_PRESENT | SECURITY_IDENTIFICATION` (exact flags `0x40110000`).
The latter two values are from
[`winbase.h:177–184`](https://github.com/mingw-w64/mingw-w64/blob/v12.0.0/mingw-w64-headers/include/winbase.h#L177-L184):
`SecurityIdentification << 16` (enum value 1) and `0x100000`. This permits server
identification, not client impersonation. See Microsoft's
[`CreateFileW` security quality-of-service flags](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew).
SQOS is not server authentication and the legitimate core's DACL does not protect
clients from a different user's pre-created endpoint.

Before returning a synchronous connection, constructing an async transport, or
sending any hello/token/record, the client checks the **connected handle**:

1. [`GetFileType`](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getfiletype)
   must report `FILE_TYPE_PIPE` (3).
2. [`GetNamedPipeInfo`](https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-getnamedpipeinfo)
   must succeed and must not report `PIPE_SERVER_END` (1): we require the client end.
3. [`GetSecurityInfo`](https://learn.microsoft.com/en-us/windows/win32/api/aclapi/nf-aclapi-getsecurityinfo)
   reads the **connected pipe object's owner**, using `SE_KERNEL_OBJECT` (6) and
   `OWNER_SECURITY_INFORMATION` (1). It returns a status code, not a BOOL. The
   owner pointer remains inside the returned security descriptor until comparison
   completes; [`LocalFree`](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-localfree)
   frees that descriptor in every exit path. A failed query or missing owner refuses.
   The handle needs `READ_CONTROL`; the existing `GENERIC_READ` pipe access maps
   to `FILE_GENERIC_READ`, which includes that right
   ([Named Pipe Security and Access Rights](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights)).
4. [`OpenProcessToken`](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-openprocesstoken)
   requests only `TOKEN_QUERY` (8) for
   [`GetCurrentProcess`](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-getcurrentprocess).
   [`GetTokenInformation(TokenUser)`](https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-gettokeninformation)
   returns our **user** SID, never an owner group. Its buffer stays alive while
   [`EqualSid`](https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-equalsid)
   compares it with the pipe owner. The opened token is closed; the current-process
   pseudo-handle is not closed. No server PID query or `OpenProcess` remains.

The previous PID-to-process lookup was replaced, not certified: its association
was not handle-bound. Owner comparison avoids that association entirely. The
server must explicitly set `O:<TokenUser SID>` in addition to the owner-only DACL,
not rely on an elevated token's default Administrators-group owner. A different
user or a shared group owner (including `S-1-5-32-544`) fails exact SID equality.

Same-user processes are trusted; this is not executable identity authentication or
protection against a malicious process already running as the same user. Holders
of restore/take-ownership privileges are outside the approved boundary; in
particular Microsoft documents that `SeRestorePrivilege` can assign arbitrary
valid user/group owner SIDs, while `SeTakeOwnershipPrivilege` alone is limited to
legitimately assignable owner values
([Privilege Constants](https://learn.microsoft.com/en-us/windows/win32/secauthz/privilege-constants)).
Any mismatch/query failure closes the connection, names the endpoint in an
`unavailable`-style error, and writes nothing. There is no endpoint fallback.

### Local-only Windows endpoint grammar

Before any open, both Python paths require `\\\\.\\pipe\\<name>`: the prefix is
ASCII-case-insensitive, the name is non-empty and contains no further backslash,
and embedded NUL is refused so native open cannot truncate it. Accepted spelling
is passed unchanged. Remote UNC hosts, `\\\\?\\`/device forms, ordinary file paths,
empty names, and nested pipe paths refuse before any native open, authentication
query, writer, or proactor handoff. Unix is unchanged.

The exact shared Rust/Python refusal text (agreed with win-transport) is:

```text
unavailable: refusing endpoint {endpoint}: Windows core endpoint must be a local named pipe: \\\\.\\pipe\\<name> (non-empty name, no further backslash)
```

### Async handle handoff: deliberate CPython-specific boundary

`loop.create_pipe_connection` cannot accept an already-opened handle. Its
`_overlapped.ConnectPipe` helper omits SQOS in all three checked CPython sources:
[3.11.9 `overlapped.c:1642–1646`](https://github.com/python/cpython/blob/v3.11.9/Modules/overlapped.c#L1642-L1646),
[3.12.0 `:1609–1613`](https://github.com/python/cpython/blob/v3.12.0/Modules/overlapped.c#L1609-L1613),
[3.13.0 `:1631–1635`](https://github.com/python/cpython/blob/v3.13.0/Modules/overlapped.c#L1631-L1635).
Therefore we open and verify the handle ourselves, retry `ERROR_PIPE_BUSY` with
cancellable asyncio sleeps, wrap the verified handle in `asyncio.windows_utils.PipeHandle`,
and use the same private duplex-transport factory as CPython's public connector:

| CPython | Public connector's factory call | Factory signature |
|---|---|---|
| 3.11.9 | [`windows_events.py:336–343`](https://github.com/python/cpython/blob/v3.11.9/Lib/asyncio/windows_events.py#L336-L343) | [`proactor_events.py:669–672`](https://github.com/python/cpython/blob/v3.11.9/Lib/asyncio/proactor_events.py#L669-L672) |
| 3.12.0 | [`windows_events.py:336–343`](https://github.com/python/cpython/blob/v3.12.0/Lib/asyncio/windows_events.py#L336-L343) | [`proactor_events.py:670–673`](https://github.com/python/cpython/blob/v3.12.0/Lib/asyncio/proactor_events.py#L670-L673) |
| 3.13.0 | [`windows_events.py:338–345`](https://github.com/python/cpython/blob/v3.13.0/Lib/asyncio/windows_events.py#L338-L345) | [`proactor_events.py:667–670`](https://github.com/python/cpython/blob/v3.13.0/Lib/asyncio/proactor_events.py#L667-L670) |

All define `_make_duplex_pipe_transport(sock, protocol, waiter=None, extra=None)`
on the bound proactor loop. `PipeHandle` takes ownership and closes its handle
([3.11.9](https://github.com/python/cpython/blob/v3.11.9/Lib/asyncio/windows_utils.py#L81-L110),
[3.12.0](https://github.com/python/cpython/blob/v3.12.0/Lib/asyncio/windows_utils.py#L81-L110),
[3.13.0](https://github.com/python/cpython/blob/v3.13.0/Lib/asyncio/windows_utils.py#L81-L110)).
Before opening anything, we check that the wrapper is available and the factory's
signature accepts the exact call used. A missing/incompatible factory or wrapper
refuses with `unsupported Python version for Windows async pipe`; it never falls
back to unverified or non-SQOS connection. Failed handoff closes the verified handle.
This internal API remains a CPython-specific maintenance/compatibility risk.

## Call-site audit

`rg -n 'AF_UNIX|open_unix_connection|socket\.socket' extensions` found three Python
core endpoint openers: `vqro_ext.py`, `vqro_provider.py`, and `tier1_run.py`. All now
use the shared transport. `vqro_provider_stream.py`, `clients/text.py`, and
`pi_bridge/program.py` use those SDKs. `clients/vqro_client_model.py` is a pure
renderer-independent model with no endpoint I/O. `pi_bridge/client.py` only
speaks to its child runtime, not the core endpoint.
`tmux_server.py`'s socket is the backend's own socket. OpenTUI's Bun transport is
not a Python caller and is outside this slice.

The returned synchronous object's supported subset is `sendall`, `recv`,
`makefile` (binary `rwb` unbuffered or text `rw`), `settimeout` (positive or None),
`shutdown(SHUT_RDWR)`, and `close`. Windows shutdown is full close, not a fabricated
half-close. Unix is still a native socket, with its native semantics.

## Validation / remaining Windows gate

`tests/test_vqro_transport.py` supplies a fake `_winapi` on Linux: pending read and
write in parallel, pipe busy, bounded waits, partial writes, binary/text records,
line limits, broken core pipe, cancel-and-drain shutdown, and Unix call preservation.
Async tests check proactor stream-protocol wiring and unchanged Unix delegation.
Controls assert exact SQOS flags, reject foreign-user/group owners and
non-pipe/server-end handles before any I/O or proactor handoff, and fail closed on
query errors. DLL mocks exercise ctypes signatures, connected-handle owner
queries, SID extraction/comparison, token cleanup and descriptor `LocalFree`.
Both paths reject disallowed endpoint names before opening. Mutation checks bypass
the owner proof and the endpoint guard independently and demonstrate that their
negative controls fail without those rules. Async controls cover incompatible/missing factories, cancellation
while busy, and handle cleanup on failed handoff.
These tests validate control flow, not Windows kernel behavior.

Run `py -3 tests/windows_pipe_smoke.py --endpoint <private fake core endpoint>`
on native Windows. The script contains private-core setup instructions and checks
both synchronous overlapped and asyncio proactor hello/snapshot/close. Actual
Windows busy-instance contention, core-exit cancellation timing, and owner-only
ACL enforcement remain unverified on Linux. `_winapi` is CPython-specific; other
Python implementations are not claimed supported on Windows.

D22's 2026-11-22 lead clarification records synchronous overlapped I/O and asyncio
proactor use. The synchronous SDK already owns a reader thread and does not need
an extra event-loop thread. Both paths keep the same wire contract.
