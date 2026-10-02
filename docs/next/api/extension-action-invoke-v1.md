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

Linux local API sockets capture kernel peer UID/PID and process birth **before**
reading the request, then capture bounded ancestry off the App actor and recheck
process/parent incarnations. The host combines that capture with its owned
terminal roots, cached original root birth, current foreground group and known
agent type. Presentation labels and pane IDs are not caller authority.

- `agent`: `terminal_id`, `agent`, `execution` (`boot-id:pid:birth`), with optional
  `session` metadata. The recognized ancestor must be the current owned-PTY
  foreground leader and match the host's known agent type. Nested agents and
  shell wrappers cannot inherit that execution principal.
- `human_unverified`: `terminal_id`, kernel `uid`. Requires a direct child of a
  positively known non-agent owned shell, not an arbitrary descendant.
- `unknown`: unsupported transport/platform, unmatched or uncertain ancestry,
  missing original root proof, or contradictory execution evidence.

This is **cooperative process-lifetime attribution**, not same-user security or
cryptographic agent/human authentication. Same-PID exec and same-process session
reset retain the execution principal. Session metadata is not authority. An
agent executing through a shell pane can obtain `human_unverified`; do not claim
human-gesture verification. Handoff roots without original birth proof stay
unknown; generic terminal recovery remains usable.

Capture bounds: 64 ancestors, 8192 bytes per process stat and 16384 bytes per
command line. Root birth is cached on fresh terminal creation. The initial
single peer-birth `/proc` read occurs on every local API connection, before its
method is known; ancestry capture and foreground classification are action-only.
Presentation and PTY parsing do not perform this work. Unrelated terminal roots
do not allocate evidence or query foreground state.

## Bounds and uncertainty

- Client payload: at most 64 KiB serialized JSON.
- Existing global work-slot bound, eight queued requests per worker, existing
  provider-count limit; admission failure is definitive before execution.
- Queue **start** deadline: two seconds from worker admission. This is not an
  atomic effect deadline or client-submission-time guarantee.
- Action deadline: 2.5 seconds including startup, independent of the unchanged
  750 ms render timeout; existing cancellation, no worker retry. Queue + action
  budgets remain below the API's five-second response and CLI's six-second waits.
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
