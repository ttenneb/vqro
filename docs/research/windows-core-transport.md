# Windows core transport (2026-11-22)

Implementation: `src/transport.rs`; this note records API evidence, not a claim of native Windows validation.

## Mechanism and evidence

- One byte-stream module owns Unix sockets and Windows named pipes. Unix keeps the stale-socket live probe,
  stale unlink and mode 0600. Clients/protocol code do not inspect endpoint names.
- Windows uses Tokio `ServerOptions::first_pipe_instance(true)` for the first instance only, and
  `reject_remote_clients(true)` for every instance. A connected instance is handed off only after its replacement
  has been created. `Listener::accept` retains its current instance across cancellation.
  [Tokio named-pipe documentation](https://docs.rs/tokio/1.54.1/tokio/net/windows/named_pipe/index.html)
  demonstrates the create-next-before-handoff pattern. A reachability probe that closes before acceptance does
  not end the listener: disconnected instances are retired and replaced.
  [ConnectNamedPipe](https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-connectnamedpipe)
  documents `ERROR_NO_DATA` when the previous client has closed before the server disconnects.
- [CreateNamedPipeW](https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-createnamedpipew)
  documents byte mode, `FILE_FLAG_FIRST_PIPE_INSTANCE` (another first instance fails with access denied),
  `PIPE_REJECT_REMOTE_CLIENTS`, overlapped I/O and pipe destruction after the final handle closes.
- Every instance gets an explicit user owner and protected DACL,
  `O:<starting user's SID>D:P(A;;GA;;;<starting user's SID>)`; never rely on an elevated token's group default owner.
  [Named Pipe Security and Access Rights](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights)
  documents why the default descriptor is unsuitable (it admits Everyone/anonymous reads), and that the DACL
  controls both client connections and subsequent server instances. Full access includes creating the next instance.
  [Security Descriptor String Format](https://learn.microsoft.com/en-us/windows/win32/secauthz/security-descriptor-string-format)
  documents `D:` and `P`; [ACE Strings](https://learn.microsoft.com/en-us/windows/win32/secauthz/ace-strings)
  documents the access-allowed ACE and generic-all access mask.
- The SID comes from the current process token's `TokenUser`, not its default owner (which can be a group).
  [GetTokenInformation](https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-gettokeninformation)
  documents `TOKEN_QUERY`, the required-size probe and returned buffer. The process-token handle is closed after
  querying. SID conversion and security-descriptor conversion allocations are freed with `LocalFree`.
  [ConvertStringSecurityDescriptorToSecurityDescriptorW](https://learn.microsoft.com/en-us/windows/win32/api/sddl/nf-sddl-convertstringsecuritydescriptortosecuritydescriptorw)
  documents revision 1, the self-relative result and its allocation lifetime.
- Async clients use Tokio's overlapped `ClientOptions::open`; sync clients use read/write `std::fs::File`.
  Only `ERROR_PIPE_BUSY` is retried, every 10 ms with a 500 ms deadline; no application request has been written,
  so this is endpoint acquisition, never replay of an effect.
  [Named Pipe Client](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-client) documents the busy error,
  read/write duplex access, and the default synchronous byte-mode handle. Sync handles serialize I/O and are only
  suitable for strict request/reply, not interactive full-duplex callbacks. The Claude hook's enclosing module
  remains Unix-only by the Windows lead's ruling; porting its private state/watchdog is a later slice.
- Named pipes have no Unix request-input half-close. Tokio 1.54.1 `named_pipe.rs` implements both server and client
  `AsyncWrite::poll_shutdown` as `poll_flush` (lines 926 and 1717); it does not signal EOF. Windows clients must read
  replies before closing their full-duplex connection. The endpoint spec now states this limitation explicitly.
- Windows background startup uses `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`, not Unix process groups.
  [Process Creation Flags](https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags)
  documents these native flags. No terminal lifetime guarantee is added.
- The Windows default pipe suffix is fixed FNV-1a over the UTF-16 bytes of the canonical state directory. The
  directory is created on first default-endpoint resolution so its canonical identity is available before opening
  the store. Explicit `VQRO_SOCKET` overrides do not create a directory or impose a Unix path length limit.

## Connected-object owner authentication and local endpoints (round 3)

`transport_auth.rs` is a fail-closed pure decision function over a query trait; `transport_auth_windows.rs` provides
its native queries. Both asynchronous and synchronous connect paths retain ownership of the opened handle and
return it to the caller only after verification. No hello, host context, supervision token or application record
can be written through a returned connection before those checks succeed. A refusal includes the endpoint and
`unavailable` in its diagnostic. `ensure_core` propagates this typed refusal before opening the workspace store or
spawning, and during startup probes; it neither counts the foreign endpoint as running nor selects a different
endpoint. Default endpoint resolution may first create/canonicalize the state directory: it is not a no-state-access
claim. Explicit invalid Windows names fail namespace validation before opening any endpoint.

The trust boundary is the connected pipe object's owner == our process TokenUser SID, not an executable identity:
same-user processes are trusted. PID/OpenProcess identity proof is removed, avoiding its numeric-PID lifetime and
namespace assumptions. The legitimate server's DACL and first-instance flag alone cannot authenticate a foreign pipe.

Every Rust client open and listener bind uses the same local-name guard; `paths::socket_path` calls it too. Accept
only `\\.\pipe\<name>` (ASCII-case-insensitive prefix, non-empty name, no additional backslash or embedded NUL).
Reject remote UNC, device and file forms before opening; pass valid names through with original spelling. This is
the Lead-approved D22 refinement, not an inferred extension of the original opaque-endpoint contract.

Native API evidence (Microsoft):
- [GetFileType](https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-getfiletype): require
  `FILE_TYPE_PIPE`; disk/character/unknown types are refused. A socket or anonymous pipe can also have this type,
  so this check alone is insufficient.
- [GetNamedPipeInfo](https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-getnamedpipeinfo):
  require success and no `PIPE_SERVER_END` bit, identifying a client-end handle.
- [GetSecurityInfo](https://learn.microsoft.com/en-us/windows/win32/api/aclapi/nf-aclapi-getsecurityinfo): query the
  **connected handle**, `SE_KERNEL_OBJECT`, `OWNER_SECURITY_INFORMATION`; this supports named pipes and requires
  READ_CONTROL (included in the read/write open). The returned owner SID points into its allocated descriptor;
  retain it through comparison, then LocalFree exactly once. Check its returned status directly, not GetLastError.
  Any error or null owner/descriptor refuses. No process ID is used or looked up.
- [OpenProcessToken](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-openprocesstoken):
  query only our current process token, requesting `TOKEN_QUERY`; its owned handle closes on every return.
- [GetTokenInformation](https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-gettokeninformation):
  query `TokenUser` using the required-size probe, then retain the aligned owned buffer through comparison. The
  same helper supplies the current process user for server DACL construction; no token-owner/group substitution.
- [EqualSid](https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-equalsid): require
  the pipe object's owner SID to match our TokenUser SID exactly, not a token group or default owner; zero refuses.
  The allocated pipe descriptor and owned TokenUser buffer keep both inputs alive throughout comparison.
- [Owner validity](https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-createprivateobjectsecurityex)
  requires TokenUser or a token group with SE_GROUP_OWNER and without deny-only; we explicitly assign TokenUser on
  creation and reject all other owners on connect. [TOKEN_OWNER](https://learn.microsoft.com/en-us/windows/win32/api/winnt/ns-winnt-token_owner)
  is merely a token default and can name a group, so it is deliberately not the client comparison identity.
- [Privilege constants](https://learn.microsoft.com/en-us/windows/win32/secauthz/privilege-constants) document the accepted
  residual: SeRestore permits arbitrary valid owner SIDs; SeTakeOwnership permits only legitimately assignable owner
  values by itself. Administrators/these privileged actors are outside the approved same-user boundary, not a
  claimed adversary this mechanism defeats. Microsoft's
  [ownership/access-rights explanation](https://devblogs.microsoft.com/oldnewthing/20050818-09/?p=34533) explicitly
  distinguishes WRITE_OWNER/SE_GROUP_OWNER, SeRestore and SeTakeOwnership. No native ownership-change experiment
  was used as proof.

SQOS is a separate protection, not authentication. The locked Tokio 1.54.1 source
`src/net/windows/named_pipe.rs:2407–2414,2442–2463,2614–2616` initializes `ClientOptions` with
`SECURITY_IDENTIFICATION | SECURITY_SQOS_PRESENT`, warns against omitting it, and combines it with overlapped I/O
in the actual CreateFile flags. The async path keeps that default unchanged. Sync opens now explicitly use
`OpenOptionsExt::security_qos_flags(SECURITY_IDENTIFICATION)`; the
[stdlib contract](https://doc.rust-lang.org/std/os/windows/fs/trait.OpenOptionsExt.html#tymethod.security_qos_flags)
says this automatically includes `SECURITY_SQOS_PRESENT` and documents why omitting it is unsafe. Identification
permits identifying the client, not impersonating it for access to its resources.

Linux fake-query controls cover exact user-owner acceptance, other-user/group-owner refusal, non-pipe/wrong-end
refusal, query failures, zero application writes and automatic-start refusal propagation. Fake-opener controls prove
UNC/device/file/empty/nested/NUL names reach neither opener nor writer, while mixed-case local prefixes preserve
original spelling. The server security-construction control requires an explicit user owner even if the token's
native default owner would be a group. Windows-only
controls additionally inspect real token-user equality and reject regular files/server-end handles. Existing
Windows async/sync request/reply tests now exercise the full native authentication chain. Cross-compilation is not
native API/security validation; actual Windows different-account and privilege-level checks remain pending.

## Validation and remaining native work

Linux tests exercise stale replacement, private mode, two independent live clients, second-server refusal,
async duplex I/O and sync request/reply. `src/main.rs` CLI/status and TUI reconnect already use `client`, so they
inherit the transport without a second connect path. `rg 'UnixStream|UnixListener' src` is confined to transport.

Windows-only tests add a busy acquisition deadline, standby-instance availability, accept cancellation recovery,
retirement of an early-disconnected reachability probe,
and inspection of the actual pipe's DACL: one allow ACE for the process user's SID, never a null DACL.
On native Windows, run `cargo test --lib transport` after integrating the other portability slices.

Still unverified here (Linux host only): native pipe behavior and DACL assertions; a second unprivileged user's
connection denial; rejection of a remote-machine connection; the detached background core's console behavior;
full CLI/TUI/provider reconnect and ordered shutdown/restart under Windows. A negative different-user connection
check needs a separate Windows account, so it is not silently skipped or simulated by the unit suite.
