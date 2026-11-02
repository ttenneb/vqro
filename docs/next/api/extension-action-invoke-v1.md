# Trusted executable action invocation

Status: **experimental, implemented**. This is a direct client/CLI path for enabled
trusted protocol-2 executable services, not Component Model Service Fabric, a
package-to-package consent mechanism, or a task registry. It reuses the existing
service supervisor, admission limits, capability negotiation and retirement path.

## Public JSON API

The optional advertised method is `extension.action.invoke`:

```json
{"id":"example","method":"extension.action.invoke","params":{"service_id":"example.service","payload":{"action":"example"}}}
```

Parameters are exactly `service_id` and opaque `payload`. Unknown top-level
parameters, including caller/actor flags, reject. The selected enabled service
must declare and be granted `action_invoke`, and advertise `action.invoke.v1`.
The same documented trusted-local service mechanism is available to third-party
services; Tasks receives no reserved admission or hidden host call.

The worker calls this fixed protocol-2 method with:

```json
{"caller":{"class":"unknown"},"payload":{"action":"example"}}
```

`caller` is **host supplied**, never merged from the client's payload. A successful
host response has `result.type = "extension_action_invoked"` and
`result.value = <opaque provider result>`. A provider result is not, by itself,
acceptance of work, nor evidence of a human gesture. The provider owns its
semantic result envelope and policy.

The capability authorizes this service invocation; it does not confer terminal,
message, package-fabric or other host-call authority. Executables are trusted
same-user code, not sandboxed packages. Installation/discovery/curation is not
consent for package-to-package calls. Explicit trusted-local enablement and the
existing capability grant are required for this invocation path.

## Caller observations

Linux/macOS local sockets and Windows local named pipes capture kernel peer PID
and process birth **before** reading the request, then capture bounded same-account
ancestry off the App actor and recheck process/parent incarnations. Unix account
evidence is the peer UID; Windows uses the process token user SID. The host combines that capture with its owned
terminal roots, cached original root birth, current foreground group and known
agent type. Presentation labels and pane IDs are not caller authority.

- `agent`: `terminal_id`, `agent`, `execution` (`boot-id:pid:birth`), with optional
  `session` metadata. The recognized ancestor must be the current owned-PTY
  foreground leader (Windows: host-selected job) and match the host's known agent
  type. Nested agents and shell wrappers cannot inherit that execution principal.
- `human_unverified`: `terminal_id`, kernel `uid` on Unix or token-user `sid` on
  Windows, never both. Requires a direct child of a positively known non-agent
  owned shell. Windows permits one exactly proved foreground extension launcher,
  not an arbitrary descendant; see [Windows provenance](tasks-windows.md).
- `unknown`: unsupported transport/platform, unmatched or uncertain ancestry,
  missing original root proof, or contradictory execution evidence.

This is **cooperative process-lifetime attribution**, not same-user security or
cryptographic agent/human authentication. Same-PID exec and same-process session
reset retain the execution principal. Session metadata is not authority. An
agent executing through a shell pane can obtain `human_unverified`; do not claim
human-gesture verification. Handoff roots without original birth proof stay
unknown; generic terminal recovery remains usable.

Capture bounds: 64 ancestors and 16384 bytes per command line. Linux reads at
most 8192 bytes per process stat. macOS uses kernel `LOCAL_PEERPID`/`getpeereid`,
exact-sized `proc_pidinfo(PROC_PIDTBSDINFO)` PID/euid/parent/start observations,
and a bounded `kern.bootsessionuuid` execution namespace. Its birth value is
checked start-time seconds × 1,000,000 + microseconds; this is a lifetime key,
not a portable timestamp format. macOS argument acquisition is capped at 1 MiB
(including environment), with exactly `argc` NUL-terminated arguments retained.
Caller capture uses XNU's target-pointer-aligned boundary after the executable
path, validates its zero padding and rejects empty `argv[0]`; it never skips an
arbitrary NUL run into later arguments or environment. Environment is excluded
from caller matching. Target bitness is rechecked across argument capture too.
The size probe is advisory across exec: acquisition reserves bounded headroom
and rejects successful reads that fill the supplied capacity, since Darwin can
return a truncated tail with success. No such partial buffer reaches matching.

macOS stops **before** a positively observed foreign-euid ancestor, without
querying that ancestor's arguments. This is not an error fallback: every retained
same-euid ancestor needs complete identity and arguments, and PID, euid, birth
and parent are rechecked. Syscall, short-read, parse or recheck failures and
cycles fail closed. A pane root outside the retained chain (including beyond
the 64-entry bound) cannot prove a caller. No authority crosses the euid boundary.

Windows uses server-end `GetNamedPipeClientProcessId`, nonzero creation FILETIME,
fresh Toolhelp parent observations, TokenUser SID and a validated kernel boot
GUID. It stops before a positively matched current-server incarnation or foreign
SID; inaccessible or missing metadata is never a boundary. Every retained entry
needs bounded complete argv and fresh SID/birth/parent rechecks, with parent-birth
ordering to reject reused-parent topology. A complete process snapshot is capped
at 32768 entries; selected-job evidence queries at most 256 relevant processes
per pass, then rechecks them. Cap, partial-query or inconsistent evidence yields
no agent proof. Selection bypasses both foreground caches, covers shell and direct
known-agent roots, and excludes escaped/out-of-chain jobs. Each request-local
observation binds root and selected PID/birth; actor lookup still requires the
original runtime root birth. Explicit API probes compare selected incarnations,
not only PIDs. No process queries are added to actor classification or rendering.
This reuses Windows host selected-job semantics, not kernel console-focus proof.

Root birth is cached on fresh terminal creation. The initial single peer-birth
read occurs on every local API connection, before its method is known; ancestry
capture and foreground classification are action-only.
Presentation and PTY parsing do not perform this work. Unrelated terminal roots
do not allocate evidence or query foreground state.

## Bounds and uncertainty

- Client payload: at most 64 KiB serialized JSON.
- Existing global work-slot bound, eight queued requests per worker, existing
  provider-count limit; admission failure is definitive before execution.
- Queue **start** deadline: two seconds from worker admission. This is not an
  atomic effect deadline or client-submission-time guarantee.
- Action invocation receives an absolute 2.5-second deadline computed before
  startup, independent of the unchanged 750 ms render timeout. Startup still has
  its separately bounded three-second handshake and can overrun that nominal
  budget after queue wait. The invocation/API may then return `outcome_unknown`;
  do not assume the nominal 4.5-second queue+action budget is a hard end-to-end
  guarantee. API response wait is five seconds; CLI wait is six seconds.
- Existing cancellation; no worker retry.
- Generation/revocation checks before dispatch and after execution. Retirement
  cancels active work and rejects queued requests, even with a full channel.

Host error `action_rejected` means dispatch did not start. Once execution may
have started, timeout, runtime loss or retirement yields `outcome_unknown`.
An effect may already be durable. A truthful completed reply may survive a later
revocation; revocation does not erase historical outcomes. A lost reply is not
a rejection. Consumers requiring recovery must implement scoped idempotency and
inspection in their semantic store, as [Tasks does](tasks-core-v0.md).

This additive advertised method does not modify generation-1 core codecs,
baseline handshake/snapshot fields or existing method shapes. Its method shape
has a separate expectation. Missing method/provider/capability disables only the
calling feature, not compatible sessions or terminal access.
