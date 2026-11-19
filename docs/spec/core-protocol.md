# vqro core protocol, v0

> **Status:** this is the living protocol of a **working prototype, and fluid**: it documents what the code does
> today and will change as we learn. Nothing here is frozen yet (`../roadmap.md`, step 5).

Status: implemented in slice 1 (`src/server.rs`), not frozen (D16). JSON-RPC 2.0, one message per line, over the
core's local endpoint (below). Both sides may send requests:
the core forwards `command.invoke` to the connection that registered the command. Errors carry a stable
`data.kind`: `invalid`, `conflict`, `not_found`, `unavailable`, `forbidden`, `backend`, `disconnected`, `io`,
`durability_unconfirmed`, `timeout`, `overloaded`, `unsupported`, `stale_generation`, `too_large`.

## Endpoint and transport (D12, D22 confirmed)

- The core listens on one **local endpoint**: a reliable, ordered byte stream per connection, reachable only by the
  user who started the core. `VQRO_SOCKET` names it. Clients, extensions, providers and bridges treat the value as
  **opaque**: they pass it to their platform's connect call and never parse it, build paths next to it or assume a
  file exists. On Unix it is a Unix socket path (mode 0600; default `<state dir>/vqro.sock`; implemented). On Windows
  it is a named pipe name (`\\.\pipe\vqro-<per-user id>`) whose DACL admits only the owner and which rejects remote
  clients (not implemented yet).
- Messages are UTF-8 JSON, one per line, separated by LF. Open the endpoint in binary mode: no newline translation.
- Clean request-input EOF is a one-shot half-close: the serving core finishes already-read valid requests under
  normal admission limits, locally flushes their replies, then closes the response half and runs connection cleanup.
  It cannot issue new callbacks after input EOF. Clients must keep reading if they want those replies; subscriptions,
  interactive streams and command owners keep a full-duplex connection instead. This is not remote-read/delivery
  acknowledgement: reset, write failure, protocol/queue failure or core shutdown can still make an admitted effect
  indeterminate. Inspect state/history, never replay it automatically. No admitted write is rolled back.
- Stdlib Python on Windows has no `AF_UNIX` (`socket` docs: the `AF_UNIX` constant is absent where unsupported).
  A Python client there connects to the named pipe with `asyncio`'s Windows proactor loop
  (`loop.create_pipe_connection`), not with a synchronous `open()` of the pipe, whose single handle serializes reads
  and writes. `extensions/vqro_ext.py` does this once the Windows endpoint exists (`build-plan.md`, Windows work).
- The per-platform state directory holds the store and logs: `VQRO_STATE_DIR`; else on Unix
  `$XDG_STATE_HOME/vqro` or `~/.local/state/vqro`; on Windows `%LOCALAPPDATA%\vqro\state` (not implemented yet).
- **Stopping the core** is a protocol request, not a pidfile and a signal: `core.shutdown` (below, Session). Platform
  signals (Ctrl-C, SIGTERM on Unix) remain extra ways in that run the same ordered stop.

## Session

| Method | Params | Result |
|---|---|---|
| `hello` | `client?` or `extension?` (name), `extension_token?` (supervised lifetime proof), `profiles?`, `features?` (rendering clients, D29; at most 256 names of 1-128 bytes each) | `server`, `version`, `protocol`, `you` (author id), `epoch`, `seq`, `safe_mode` |
| `core.shutdown` | | `outcome: "accepted"` |

Every change records its connection's author: `ext:<name>` only after verifying the existing managed-lifetime
`extension_token`; otherwise `client:<connection id>`. Unverified extension/client labels cannot reserve an `ext:`
author. Human-readable client labels remain in `client.list`, with each client's declared `profiles` and `features`
(null when not declared; older cores ignore them). A rendering client declares the profiles it implements (`base`,
which includes recovery; `terminal`; `keys`) and the optional UI feature ids it renders (bare names such as `items`
mean `vqro.items`). They are reported for diagnostics and dependency facts, never used to tailor shared content:
content carries its own fallback. D24 invocation proofs still carry original-invoker
attribution; this is collision hygiene, not a new workspace permission scheme (D8/D24).

A supervised connection is bound to its exact extension lifetime, not merely `ext:<name>`. Disable, restart,
replacement, removal or child exit immediately fences its RPC mutations, provider publications/routes and command
handlers. Validation repeats at publication, queued commit preparation and irreversible writer admission, and at
adapter dispatch. Reusing the author name never revives an old connection. Stale, unstarted work is rejected with
`stale_generation` (or a closed provider transport), with no mutation. A durable writer already dispatched before
retirement still completes, adopts and reports its real result; retirement never discards a persisted candidate or
pretends that native work already dispatched was recalled. No uncertain effect is replayed.

`core.shutdown` is implemented (D22 is confirmed; Windows transport remains implementation work). The accepted reply is locally
written/flushed before stop begins, not a remote-read guarantee. The core stops accepting and rejects new workspace
commits, then gives admitted commits3s to persist, publish and run adoption hooks. At expiry queued/unstarted
commits are cleanly rejected without effects; an already dispatched durable write remains indeterminate, never
rolled back or replayed. Completed replacements stay on disk and are loaded on the next start. A further1s wait
lets mutation handlers flush replies before extensions/providers/peers stop; expiry reports indeterminate delivery,
not a clean write failure or a remote-read acknowledgement. An in-progress filesystem call cannot be safely cancelled.
A portable process-held `.workspace.lock` is acquired before state is read and retained by dispatched writer
snapshots: replacement startup is refused while an old write can still replace state, even after socket cleanup.
`ensure_core` reports the held state immediately instead of spawning a failing core and silently polling its socket.
On upgrade from an older build without this lock, wait for that older process to exit before replacement startup.
The core then closes extension lifetime pipes (1.5s grace, then kill/reap, inside a2s supervisor bound), stops
provider transport links, closes clients/streams, removes the endpoint, and exits. Backend-owned terminals are
not killed; input is never replayed. Bounded cooperative cleanup waits report
unconfirmed cleanup honestly. Dispatch/restarts are rejected while stopping. `vqro.shutdown` is the same built-in
recovery command, including safe mode. `vqro stop` uses RPC even without a pidfile; only no-answer, timeout or
disconnect uses legacy positive-pid signal fallback, never a protocol refusal. SIGTERM/Ctrl-C use the same cleanup.

## Workspace (vqro-owned state, D15)

| Method | Params | Result |
|---|---|---|
| `workspace.snapshot` | | `epoch`, `seq`, `entries` (key → `{version, value}`), `resources`, `commands`, `extensions`, `providers`, `adapters` (compatibility alias), `safe_mode` |
| `workspace.get` | `key` | the single entry `{version, value}`, or `null` if absent |
| `workspace.apply` | `ops: [{op: "set", key, value, expect?} \| {op: "delete", key, expect?}]`, `note?`, `on_behalf_of?` (invocation proof) | `txn`, `undoable`, `changes` |
| `workspace.history` | `limit?` | `txns` (newest first) |
| `workspace.revert` | `txn`, `on_behalf_of?` (invocation proof) | reverting `txn`, `undoable`, `changes` |

`workspace.get` reads only the exact requested entry, not an aggregate snapshot or live content. It is available
to clients, extensions and provider connections under the same normal entry/reply limits. A legacy oversized
entry can still return `too_large`; callers must not assume any single entry fits their connection's record cap.

Transactions are validated against the resulting state and applied all-or-nothing. `expect` is compare-and-set
(0 = must not exist). Key namespaces and value shapes: `src/store.rs` header. Layouts may only reference
existing views; `workspace/active` must name an existing layout.

**Several layouts; layout settings.** A workspace may hold any number of `layout/<name>` trees; the one
named by `workspace/active` is shown, and switching is a write of that entry (shared by every client
for now). A layout's optional settings live beside it, in `doc/layout/<name>` = `{cwd?, title?}`: `cwd`
is the folder new things start in (bundled `tabs.open`, `panes.split` and `pi.new` use it when not
given one; the Python helper's `layout_folder(args)` reads the invoking context's layout, else the
active one), `title` a display name. They are separate from the tree so that writers which replace a
layout's root node (wrapping it in a split, `vqro.layout.reset`) can't drop them; whoever deletes a layout
deletes its settings in the same transaction. The core does not interpret them. The bundled
`workspaces` extension gives each workspace folder its own layout `ws-<id>` (id from the folder's real
path) and `main` holds the rest; `vqro.layout.reset` rebuilds `main` with every live terminal and
selects it.

A successful apply or revert has persisted the candidate through a unique temporary file in the same directory,
then the platform's **durable replace**: on Unix file sync, rename, then directory sync (implemented); on Windows file
flush, then a write-through replace (`MoveFileExW` with `MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH`; not
implemented yet), where "directory sync" below means that final confirmation step. Before replacement, persistence failure returns `io` with no change
to entries, versions, history, transaction numbering or events. Failed writes clean up their own temporary files;
other writers' files and legacy `.tmp` paths are not swept or overwritten. Process crashes can leave harmless
orphaned temporary files, which are never loaded as workspace state.

If replacement succeeds but the final directory sync fails, the core **adopts and publishes** the new transaction
and returns `durability_unconfirmed` with `data.txn`. The entries event and history reflect this adopted state;
extension changes are reconciled too. The file now names the candidate, but a crash may recover the old or new
state. **Do not automatically replay**: inspect snapshot/history using the transaction id instead. This exception
reports uncertainty about crash durability, not partial application of operations.

All running workspace mutations (including built-in undo/reset/extension changes) share one serialized
committer. Its FIFO admission is bounded by **128 requests and 8 MiB encoded work**, including the active
write; each request has a 256-byte metadata floor. Revert reserves its currently known serialized undo operations,
not just its small request. At queue head the ordered target and expanded work are rechecked before copying undo
values or dispatching a writer. If growth cannot be reserved immediately, it returns `overloaded` with
`outcome: rejected` and no effect; it never waits for later queued work to release its budget. Full admission returns `overloaded` with `outcome: rejected`
and no effect. Candidate copying/validation/encoding and durable filesystem work run off the core lock on a
blocking worker; snapshots, events and terminal I/O can continue while a write waits on storage.

CAS and default undo selection use the state after preceding queued commits have been adopted, not a snapshot
captured when the request arrived. Adoption and entries-event publication are ordered before the next commit
starts. Pending candidates are not visible in snapshot/history. A cancelled receipt, client timeout or disconnect
does not cancel admitted work or its extension reconciliation: inspect state/history, never replay automatically.
An unexpected writer-task failure returns `indeterminate` and stops further commits until restart; it is not a
clean persistence failure or a promise that replacement did not happen. Unadmitted requests remain rejected.

### Undo horizon and history storage

Workspace files use compact JSON. History retains at most **500 records and 8 MiB of compact serialized history**
(including array delimiters); the oldest records are evicted first. This budget is separate from current entries.
An evicted id returns `not_found`, not a synthetic undo. Notes are capped at **4096 UTF-8 bytes**: new oversized
notes reject atomically with `too_large`. Existing pretty files and full `after` entries load unchanged; old notes
are visibly truncated to the note limit and history is bounded while reading. Loading alone never rewrites the
file; the next successful mutation persists the compact form and bounded horizon.

Each history change stores `key`, `before: {version, value} | null`, and `after_version: integer | null`.
The after value comes from current entries for publication, not a second retained history copy. Transactions keep
`author`, optional D24 `on_behalf_of`, `at_ms`, optional `note`, and optional `reverts`; reverting is another audited
transaction with fresh versions and CAS against the recorded after versions. Immutable history records and entry
preimages are shared in memory rather than deep-cloned for every candidate.

A single undo snapshot exceeding 8 MiB **does not reject a valid state change**: its record retains metadata, keys
and after versions but has `undoable: false` and no preimages. Apply/revert replies and entries events include
`undoable`; on history records it defaults to true when absent (including legacy records). Reverting such a record returns `unavailable` with a
clear horizon message. Default undo stops there, never silently reaching past it to older eligible work. New small
changes beyond that boundary remain undoable. A record whose metadata alone cannot fit is rejected with `too_large`;
for legacy files, such a record and older undo history are discarded rather than undoing across an unrecorded gap.
The D24 global selection rule still applies within this finite horizon; autonomous extension work is not selected.

Workspace snapshots capture immutable entry/resource/provider/live references and their epoch/sequence fence
under the core mutex, then construct and preflight JSON on a blocking worker outside it. Two construction slots
are reserved for ordinary recovery clients and two for the smaller provider envelopes; a provider snapshot storm
cannot consume the ordinary pool. Waiting for a construction slot is bounded to 250 ms, then `overloaded` is
returned without state changes. Oversized snapshots stop limited encoding before allocating a full JSON copy.
Cancelling a receipt does not free a slot while its blocking construction still runs. No cursor is fabricated:
clients use the captured sequence and replay subsequent events normally.

The TUI treats snapshot `overloaded`, `unavailable`, and `timeout` as temporary, including startup, reconnect and
post-install re-exec. Each snapshot attempt has a 5 s local deadline. On refusal, startup opens the built-in recovery
surface; an existing UI retains its last good workspace and terminal streams instead of disconnecting/reattaching.
A persistent `core busy` badge remains through bounded, jittered exponential backoff (0.5–3 s), until a snapshot
succeeds or the user quits. This is a cap on each delay, not a finite retry count that eventually exits the TUI.
Only read-only snapshots retry; terminal input, commands and effects are never replayed. Events while waiting are
subsumed by the next snapshot; in-flight deltas still use the captured epoch/sequence fence. Retry deadlines cannot
be postponed by incoming event traffic, and keys/recovery remain responsive. `too_large` keeps its separate explicit
recovery path; other failures still use normal error/disconnection handling.

CLI diagnostics do not silently retry refusals. `vqro status`/`call` allow 2 s for connection/hello; status allows
2 s for its optional client list and reports its failure rather than swallowing it. `vqro call workspace.snapshot`
has a 5 s request deadline; arbitrary calls/commands retain the existing 60 s request deadline. A timeout reports
what is known, never permission to replay a command/effect.

## Size admission and recovery

Writes reject growth above **1 MiB per encoded workspace entry** (key/version/value), **16 MiB total entries**,
**1 MiB per encoded live item**, or **8 MiB total live content**. Error `too_large` includes `outcome: rejected`,
byte count and limit; state/history/sequence remain unchanged. Old oversized state still loads, and deletion,
revert or shrinking a legacy violation remains possible. Snapshots carry `recovery_notice` for legacy entry limits.

Opaque JSON in workspace entries, live content, provider facts/metadata and invoked command arguments is
limited to **64 nested object/array containers** (scalars, including literal bracket strings and transaction
notes, add no depth; notes retain their4096-byte bound). Depth violations return `too_large`,
`outcome: rejected`, `unit: container_depth` before publication or a stored-state change. Emitted RPC frames
have a **96-container** envelope; other incoming method parameters and opaque replies are bounded to96.
Unsent over-deep calls are rejected. An unpublishable reply reports `outcome: indeterminate` because execution
may already have happened: inspect state, never replay. An unsafe notification closes its transport rather
than publishing a malformed/truncated value. These bounds leave headroom below the JSON decoder's128-container limit.

Startup reads legacy entry/preimage JSON as syntax-validated raw values before constructing trees. An
unsafe current declaration is quarantined; healthy entries/versions remain unchanged. Unsafe views/layouts
get inert recovery placeholders so healthy references remain valid; other unsafe entries are omitted.
Unsafe undo preimages become explicit `undoable:false` boundaries, not silently skipped undo targets.
Startup does not rewrite the original. Before the first later durable write, a private, byte-identical
`workspace.quarantine-*.json` backup must be file/directory-synced; failure rejects the write and preserves
the original. Backup ownership is shared across discarded writer snapshots. Snapshot `recovery_notice`
(and the existing TUI recovery message) identifies the quarantine and backup path; it survives restart.
Quarantine archives are retained for manual repair, not automatically interpreted or replayed.

The ordinary control backlog remains **4 MiB**. One separately reserved, prioritized **64 MiB maximum reply**
slot stays charged through actual writing: 16 MiB entries + 8 MiB live + 32 MiB resource allowance + 8 MiB
metadata/headroom. A busy large slot returns `overloaded`; a reply over the envelope returns `too_large` **without
disconnecting**. This is not a raised general backlog cap. No effect/input is replayed. The resource envelope
is enforced for native and wire providers: 4096 resources/provider, 128 KiB whole facts, 32 MiB globally retained
published/pending-IO resource JSON, 1024 providers, 128 KiB registration metadata/provider, **4 MiB aggregate
registration metadata** (with a 1 KiB/provider accounting floor), 128-byte names and 256-byte instances. Global-budget errors name the offending provider. Legacy workspace/live state can still
exceed other limits, so oversized recovery remains necessary.

Core connection admission is capped at **2048 concurrent serving connections**, including idle and draining
connections. Existing clients are not evicted. Above the cap, a bounded4096-byte/100ms first-request read
correlates a small `overloaded`, `outcome: rejected`, `reason: connection_limit` refusal, then closes the
new transport (silent/malformed clients may receive a null id). Descriptor exhaustion (`EMFILE`/`ENFILE` on
Unix) and other transient accept-resource failures are rate-limited in logs and retried after100ms; they do
not request core shutdown or stop existing connections. New admission resumes when descriptors/slots return.
At Unix startup, the core best-effort raises only its own **soft nofile limit to the inherited hard limit**;
it never raises the hard limit, changes another running process's limit, or requires administrator changes.
Success/failure is logged; future children inherit normal OS limits. No idle timeout or backend kill is added.

If legacy/external state exceeds the reply envelope, the TUI starts in clearly labelled **recovery-only** mode,
not a partial authoritative workspace. Built-in undo, safe mode, reset, commands and quit remain reachable; CLI
apply/delete also works. Recovery commands/state changes trigger another fenced snapshot attempt. A snapshot
timeout explicitly closes the old peer and reconnects responsively rather than remaining fenced forever.

Incoming methods and queued application ingress are each capped at **4 MiB**, with a 64-byte metadata floor.
Framing, including unterminated records and replies, is capped at 64 MiB by default; registered providers lower
that to 1 MiB, replies included. `Peer::set_max_record_bytes` also checks an in-progress record. Queued response
values have 4 MiB plus one large-response permit, held until the call consumes them. Pending calls and admitted
server requests are capped at 128/connection. Overflow closes ingress explicitly: recover facts, never resend
effects. The Python helper caps notifications at 4 MiB/128 queued records through callbacks, request handlers at 32,
and pending calls at 128. `Extension` opts into overflow recovery: discard only undispatched notifications,
queue one local `missed {reason: "notification_backlog"}`, then snapshot/fence before applying subsequent
notifications. Arrivals during snapshot acquisition are buffered; another overflow schedules another gap. A
64-byte gap-marker reservation stays inside the byte cap, and the active callback remains charged. Commands,
RPC replies and effects are never dropped or replayed by this mechanism. If the recovery snapshot fails with a
protocol/state error, the transport closes rather than continuing with a divergent cache. If it only gets no answer
in time (`timeout`, 60 s) or is `overloaded`, the extension is **degraded** instead: it keeps its last cache
(subsequent events still apply), runs no listeners on that known-stale cache, logs and notifies once, and retries
the snapshot with jittered backoff (0.5 s doubling, 30 s cap); on success listeners rerun as `missed`. Until `run()`
has subscribed, requests safe to resend (reads, registrations, `view.publish`, workspace writes whose every op has
`expect`) wait out a slow core the same way; other requests report `timeout` and are never resent. A listener whose
write gets `conflict` or `timeout` is rerun later as `missed`. A closed connection still exits the process, so
supervision restarts it. Raw `Vqro` users must explicitly opt in with a
snapshot-capable missed handler; otherwise overflow retains the explicit disconnect/restart behavior. These encoded/queue bounds exclude allocator overhead, framing/serialization copies
and a consumer-held record.

## Events

| Method | Params | Result |
|---|---|---|
| `events.subscribe` | `from?` (seq), `epoch?` (opaque string, required with `from`), `types?` (list of event types), `partial_resources?` (bool), `view_content?` ({names?: [string], prefixes?: [string]}) | `epoch`, `seq` |

The event log retains at most **10,000 records and 8 MiB encoded JSON** (64-byte floor). Byte eviction, an
unretainable record, or a replay batch too large for the control backlog produces `missed`; subscription stays
active. `reason: backlog_budget` requests a snapshot instead of repeatedly disconnecting on that replay batch.
Client resync buffers have the same 8 MiB byte cap and restart recovery on overflow.

Subscription options shape what one subscriber receives; they never change the log or other subscribers.
A peer closing terminates its subscription immediately, including while the event stream is idle; no subsequent
event is needed to release its receiver and buffers.
- `types`: only events of these types arrive (unknown names are allowed; `missed` always arrives). Caches built
  from a type you don't receive go stale until the next snapshot.
- `view_content`: restricts only `view.content` events to exact `names` or literal `prefixes` (OR). Omitted means
  all views; an empty object matches none. Matching includes content removal (`content: null`), replay and new
  events. Other event types and `missed` are unchanged. Prefixes are plain strings, not globs. Snapshots remain
  complete; caches of excluded views stay as of their last snapshot. Older cores ignore this optional field.
- `partial_resources`: the first `resource` event per resource after subscribing, after `missed`, or after its
  `resource.removed` carries the full record. Later ones carry only `id` and the fields that changed (a removed
  field as `null`) and have `"partial": true` at the top level of the event. Merge them onto the record you hold.
  A partial event for a resource you don't hold means your state diverged: take a snapshot. The base is always
  the state after this subscriber's previous event for that resource, so the snapshot fencing rules below hold
  unchanged. Why: status changes dominate event volume at scale and each one otherwise repeats the whole record
  (`docs/test-plan.md`, scale harness).

The cursor is the pair `(epoch, seq)`. A new random epoch is generated each time the core starts;
sequence numbers start at zero within that epoch. `hello`, snapshots and every event carry the epoch.
With no `from`, subscription starts at the current position (no backlog). With `from`, a missing or
nonmatching epoch, a cursor ahead of the current sequence, or an expired log position produces `missed`
instead of silently accepting an ambiguous cursor. `missed` carries the current `epoch` and, when known,
`seq`; the subscription remains active and streams subsequent events.

On `missed`, fence incoming event deltas while fetching a snapshot. Apply the snapshot, discard buffered
events from other epochs or with `seq <= snapshot.seq`, then apply the remaining events in order.
Only one snapshot generation may complete recovery; ignore replies superseded by another resync or
connection. A bounded recovery buffer may discard its batch and request a newer snapshot on overflow.

Then notifications `event {epoch, seq, type, data}` with types `entries`, `resource`, `resource.removed`, `command`,
`extension`, `provider`, `adapter` (compatibility alias), `safe_mode`, `effect`, `persistence.error`, `persistence.recovered`; or `missed` when the bounded log can't serve the cursor
(re-snapshot). `effect {effect, provider, generation, method, author, resource?, op?, target?, outcome, error?}` audits
the result observed by the caller, including an indeterminate timeout; it is not a late-reply notification. `resource`
is the addressed core id, `op` the actual provider operation (e.g. `agent.send`), and `target` the exact native
`{ref, incarnation}`. Spawn has no pre-existing target; cached retirement has no native target. These fields never
include input content. Late original replies are ignored, with no invented `effect.outcome` event; independent
resource facts reconcile the backend.

`resource.removed` keeps its mechanical `reason` (closed, replaced, forgotten, etc.). When a provider explicitly
reports removal it also carries `provider` and the unchanged `native_reason` object, including unknown future
reason strings. Administrative cached forgetting/replacement does not invent native end evidence. Consumers must
not equate a generic removal, unknown native reason, or viewer/bridge closure with agent-process exit; the relevant
provider's lifetime relationship and evidence determine that interpretation.

`persistence.error {scope: "resources", provider, message, error}` reports a resource-identity map failure and is
logged once per affected provider until recovery. Repeated resource/status events are coalesced, not logged as a
new failure each time. `persistence.recovered {scope: "resources", provider}` makes recovery visible.

## Providers (native and wire)

The registry exports `providers: {name: {name, role, instance, generation, software, capabilities, state,
connected, synced, since, error, identity_persistence}}`. `state` is `connected` or `disconnected`; `synced` means
a complete native inventory has arrived. `capabilities` maps names to parameter objects; absent means unsupported,
unknown names are preserved, and non-object declarations are invalid. Explicit capability parameter `effect:false`
rejects generic `resource.invoke` before dispatch with `unsupported`/`outcome:rejected`. `terminal.input` also
supports `stream:false` to reject `stream.input`; missing flags retain legacy handler behaviour. Current native
herdr/fake and wire herdr/tmux/Python fake declare `{stream:true,effect:false}` for terminal input: literal input
still works on attached streams, but no unsupported raw-input RPC, automatic Enter, or temporary attach is invented. `identity_persistence` separately reports
`{state: "ok"|"failed", error?}`. `provider {name, state}` events publish registry changes. `adapters` and `adapter`
events temporarily remain aliases so existing extensions continue to work. A disconnected runtime record is reaped
only when it has no active identities, retained resources or pending facts; `provider.removed {name,reason:"retired"}`
(and compatibility `adapter.removed`) removes it from registry snapshots. Backend records and stale runtime facts
remain while relevant. Host-bound resume credentials survive independently until their host retires. A core-wide
lease counter preserves monotonic generations when a reaped name returns; old sources/routes remain fenced.

Safe-mode provider admission refusals retain `kind: "forbidden"` and `outcome: "rejected"`, with the additional
structured `data.reason: "safe_mode"`. This identifies a temporary core admission state, not an authentication
failure or permission grant. Runtime bridges may keep their backend-owned process while backing off until
admission becomes possible; they never replay input. Other refusals do not acquire that transient meaning.

Wire registration has an eight-operation global admission bound (including pending host claims); excess
registrations return `overloaded` before consuming a credential or mutating the registry. Ordinary control and
stream traffic do not use these slots. Provider-local resource transitions use identity/reference indexes;
unchanged instance registration and authoritative inventories do not clone/write the whole identity map.

A provider registration/start is connection-bound and gets a new generation. Old event sources, queued commands,
effect replies and streams cannot act through a replacement connection. The native adapter's connection token is
internal, distinct from the core-assigned public generation. `provider.register`, chunked `provider.sync`,
`resource.upsert`/`gone`, provider effects and credited streams are implemented as pinned in `provider-v0.md`
section 9. Requests/notifications carry generation in params; reply envelopes never do. Wrong-generation requests
return `stale_generation`; wrong-generation notifications close the connection. Wire provider records, including
records queued before registration, are capped at 1 MiB. Relay remains unimplemented.

Every `resource.spawn` passes the core's opaque endpoint as `VQRO_SOCKET` in the terminal's environment (D22),
with `VQRO_SOCKET_SOURCE=terminal`, so `vqro call` works from any pane. When that marker is present and
`VQRO_STATE_DIR` is set, CLI endpoint selection uses that state directory's endpoint instead of the inherited
`VQRO_SOCKET`: a scratch run cannot silently call the live core. For a deliberate explicit endpoint plus state
pair, clear `VQRO_SOCKET_SOURCE`; unmarked `VQRO_SOCKET` retains precedence. An explicit `env.VQRO_SOCKET` is kept, and a provider declaring
`terminal.spawn {env: false}` gets none. `resource.spawn {bridge:true}` additionally opts into `VQRO_BRIDGE_TOKEN`. Runtime
registration may wait five seconds for durable host identity and returns `resume_token`; bootstrap and resume use
same `params.token`. Private durable current+previous credentials bind exact host/provider/instance and expire
on host removal. Lost-first-reply recovery, rotation and core-restart identity restoration are tested. Ordinary
spawns receive no token. Backends run as provider extensions. Safe mode uses the human-confirmed empty-start/
session-allowlist policy below; no provider is privileged as essential.

`terminal.exit {exit_code:true|false}` declares whether numeric exit codes are natively reportable; absent/unknown
parameters imply no numeric-code guarantee. Never manufacture a code from an unknown reason.

A resource's identity is `(provider, instance, backend_ref, incarnation)`. IDs never reuse a retired identity's id.
Herdr's instance is the stable `hello.core.id` (not pid/uptime); `created_ms` identifies a terminal incarnation.
The fake gets a random instance per start. Instance/incarnation changes retire old ids and emit
`resource.removed {id, reason: {reason: "replaced"}}`; an authoritative inventory removes absent identities with
`reason: {reason: "closed"}`. Views/layouts are never rebound automatically. A disconnect instead keeps known facts
with `available: false, stale: true`; the same instance and identity returns with its original id after fresh facts.

Identity reservations and the monotonic allocation high-water mark are durable before publication. Retired
records are dropped, not retained as a growing tombstone table. Opening old files accounts for every saved id
(including retired and legacy records) before pruning them, so restart cannot reuse or rebind an id. Active tuple
lookups are indexed. A native inventory or wire sync chunk uses one candidate/identity-map write, including
final-chunk removals, before publication; failed batches retain bounded pending facts. Host credential expiry is
likewise batched on retirement. Storage stays on the core lock; the 600-new-inventory regression bounds the hold. A pre-rename failure publishes no
new vqro id. Because the backend spawn may already have happened, the call returns `io` with
`data.outcome: "indeterminate"`, `backend_outcome: "observed"`, `provider`, `instance` and `resource_facts`
(including `backend_ref`); never resend automatically. Facts are retained internally and assigned ids after an
authoritative sync or a later successful map write. A post-rename directory-sync failure adopts the identity but
returns `durability_unconfirmed`, like the workspace store; later success must confirm durability. Legacy two-field
maps cannot prove an instance: their ids are retired via the high-water mark, new ids come from the first native inventory,
and old layouts may show gone resources until explicitly repaired/reset. No identity is guessed during migration.

## Resources (backend-owned, D14)

| Method | Params | Result |
|---|---|---|
| `resource.list` | `provider?`, `backend_ref?`, `id?`, `host?` (exact strings) | `resources` |
| `resource.spawn` | `argv?` (default: the provider's default shell for its host), `cwd?`, `name?`, `env?`, `cols?`, `rows?`, `placement?`, `bridge?` (default false), `provider?` (`adapter` alias), `deadline_ms?` | `outcome: observed`, `effect`, `resource` |
| `resource.kill` | `resource`, `deadline_ms?` | `outcome: accepted`, `effect`, `backend` |
| `resource.forget` | `resource`, `deadline_ms?` | `outcome: observed`, `effect`, `data` |
| `resource.forget_cached` | `resource` | `outcome: observed`, `effect`, `data: {scope: "cached_facts", backend_effect: "none"}` |
| `resource.attach` | `resource`, `cols`, `rows`, `history?` (backend scrollback), `deadline_ms?` | `outcome: accepted`, `effect`, `stream`, `initial` (`history`, `repaint` or `none`) |
| `resource.invoke` | `resource`, `op`, `args?` (object), `deadline_ms?` | `outcome`, `effect`, `data?`; capability-checked generic provider operation |
| `stream.input` | `stream`, `data` (base64) or `text` | `outcome: dispatched` (also accepted as a notification) |
| `stream.resize` | `stream`, `cols`, `rows` | |
| `stream.close` | `stream` | |

`resource.list` optionally filters by exact `provider`, `backend_ref`, vqro `id` and/or hosting-terminal `host`
(AND), before serializing records.
Absent filters retain the complete list. Malformed filter types are `invalid`; a nonmatching filter returns an
empty list. These scoped reads are generic, read-only, and retain normal reply caps: a broad filtered result
can still be too large. Runtime bridges read their own provider/ref records; launchers read a source id and the
new host's agents without full snapshots. The Pi launcher reads its settings via `workspace.get` and polls only
`resource.list {host}` after an observed spawn. Transient status-read failures may retry that read, never spawn;
expiry/UI failure retains the observed host/result and offers the terminal fallback, not an apparent failed launch.

Spawn, kill and attach check declared capabilities and provider/resource availability before dispatch. Their default
deadline is 30000 ms, override 1–300000 ms. Expiry is `timeout` with an indeterminate outcome; connection or resource
loss while in flight is `unavailable` with an indeterminate outcome. Pre-dispatch unavailability is rejected.
Late replies are ignored; independent resource facts reconcile what happened. No effect is automatically resent.
A spawn reply describes creation. If that exact `(provider, instance, ref, incarnation)` is already published,
completion reuses its id/current facts instead of rolling a newer observation back to the creation snapshot
(for example, a very short program may already be finished). An unknown identity follows normal durable admission.

`resource.forget` releases a **finished** resource: the owning provider drops it backend-side (herdr-core
`terminal.rm`, which deletes the terminal record and its history in herdr-core; tmux already deleted the exited
pane; the fake drops it), then the core retires its id like a `resource.gone` (`resource.removed` event; ids are
never reused, including across restarts; views showing it show it gone). **It is irreversible**: it is a backend
effect, not a workspace transaction, so undo/revert cannot bring the resource or its output back. A live resource
is `invalid` (end it with `resource.kill` first); a provider without the `resource.forget` capability is
`unsupported`; `resource.invoke` with that op is refused. When to forget is policy: the bundled `retention`
extension forgets a finished resource 60 s after no layout shows it any more (so undoing a tab or pane close within
that grace shows it again and cancels the forget), and keeps the newest 64 never-shown finished resources. Without a policy, finished resources stay until the backend drops
them, counted against the 4096-per-provider admission bound.

Agent config options and commands use the generic path: `resource.invoke {resource, op: "agent.configure"|"agent.command",
args}`. The core checks only the declared capability; the agent resource's `data.agent.config` / `data.agent.commands`
(agent standard v0 section 6.3, provider-v0 section 6) say what this agent supports, and its adapter rejects anything
stale or undeclared before a native write. The bundled agents extension wraps them as `agents.config` (popup),
`agents.configure {id, value, rev?}` and `agents.command {name, text?}`; grants (Control) come later, so for now
any connected client may use them, as with every other agent op.

`resource.forget_cached` is a separate core-owned mechanism: it retires **unavailable cached facts only**, with
fresh durable identity bookkeeping and a `resource.removed` event. Available resources reject with `invalid`;
a reconnect that wins the race therefore cancels cached forgetting. No backend capability is required, no native
operation is dispatched, and the backend program/terminal is neither killed nor forgotten. Independent later
observations may create a fresh resource id. This is not a fallback for native `resource.forget`; older cores
reject the distinct unknown method rather than accidentally executing a native effect.

The retention extension uses this mechanism only on an observed end: an unavailable agent whose host (bridge)
terminal has finished, or was removed after retention saw it, cannot reconnect and is retired at once. A host not
reported yet (an inventory still syncing after a restart) is not evidence. Time alone retires nothing: an agent
that disconnected for unknown reasons stays visible as unknown until it reconnects or a user explicitly forgets
it (`agents.forget`). It does not expire unavailable live terminals.
Disconnected runtime providers are then reaped when their last identity/resource/pending fact is gone. Retention
also prunes unreferenced orphan bridge fallback declarations; the agents extension prunes its own orphan panels
and their derived content. Layout-referenced views and user-owned panels stay intact. Disabling these extensions
disables the policy; core registry/facts/workspace byte and count limits remain the mechanical backstop.

Published resources include `provider`, `instance`, `incarnation`, `available`, `stale`, and opaque kind-specific
`data` (object, at most 64 KiB); `adapter` and `backend_ref` remain for compatibility. Unknown kinds stay opaque.
Attach/resize dimensions are 1–9999. Unavailable views show that state and reattach when availability returns.

Streams push `stream.data {stream, data}` (base64) and `stream.closed {stream, reason}` (reason is an object
with a `reason` string). Input is handled in order; one input message may contain at most 64 KiB of decoded bytes.
A full bounded input queue returns `overloaded` without dispatching that input. A rejected input notification
produces `stream.error {stream, error, outcome: "rejected"}` instead; it is never replayed.

Clients paste like terminals: wrapped in bracketed-paste markers (`ESC[200~` … `ESC[201~`) when the program in the
pane enabled bracketed paste (DECSET 2004, tracked by the client's emulator), raw otherwise; markers inside the
pasted text are removed so a paste can't end its bracket early. The Rust TUI sends a paste over 64 KiB as ordered
`stream.input` requests of at most 64 KiB, the markers once around the whole paste, each request answered before
the next. A chunk refused with `overloaded` had no effect and is offered again (for up to 30 s); any other error
stops the paste with a message saying how many of its bytes were sent, and nothing more is sent or resent. If the
stream ends meanwhile (a slow viewer's gap ends a stream; the TUI reattaches), the chunk refused with `not_found` or
`unavailable` had no effect and is sent on the view's new stream, continuing from there; chunks already accepted
are never sent again. `dispatched` means core-queue acceptance, not native consumption: a later provider/native
input close can leave those bytes indeterminate. vqro-tmux drains queued input until ordered close (bounded 30 s),
while vqro-herdr/Python input can end with the output path; there is no generic post-gap delivery promise
(provider-v0 9.5). Keys
typed meanwhile queue behind the paste. The largest paste is **4 MiB** (after markers); a larger one is refused
whole with a message and nothing is sent. Providers must accept any 64 KiB input message: vqro-tmux splits it into
`send-keys -H` commands of at most 2 KiB (tmux's command parser fails above about 8 000 arguments) and writes input
from its own task, so a long paste never stops it draining the pane's output.

Terminal input/output buffers are byte-bounded (256 KiB per direction per stream). Each connection has separate
outbound byte budgets: 1 MiB for stream data, 4 MiB for replies, events and other control messages. Small messages
have a 64-byte accounting floor to bound queue metadata too. Permits stay charged through the socket write.
Control has priority over queued stream data; an in-flight JSON line must finish before control can overtake it.
These are payload/queue bounds, not a promise about allocator overhead, operating-system socket buffers or latency.
The herdr adapter also bounds its frame queues (1 MiB outbound, 32 MiB inbound; individual frames at most 16 MiB).

When a viewer cannot keep up, its affected stream closes with `reason: {reason: "slow_consumer"}`. Queued data for
that stream may be discarded; closure is an explicit gap, not a complete-output claim. Other streams/connections
continue. A normal backend closure follows accepted output in order; if its closing marker cannot fit, it becomes
`slow_consumer` rather than silently dropping final output. Control queue overflow disconnects the connection:
reconnect and recover events using the epoch cursor/snapshot mechanism, never resend input.

Late attachment completion after viewer disconnect is closed before registration. Idle output tasks select
viewer shutdown without waiting for backend bytes. TUI early-attach buffers have a 256 KiB per-stream / 1 MiB
total bound with a 64-byte chunk floor; overflow closes that stream and schedules a fresh repaint. Incoming
drains are batched to keep key handling reachable under sustained traffic.

After a gap, discard the old VT parser state and attach again with backend history for a fresh repaint; do not
concatenate partial escape sequences across streams. The TUI retries closed live views after a paced one-second
retry, without requiring a new resource event. It also handles `stream.gap {stream, reason}` from future producers
by closing the old stream and reattaching. Provider-side `stream.credit` counts decoded bytes accepted into the
bounded core writer queue, not an unbounded enqueue or a claim that the viewer parsed bytes. Data precedes ordered
gap/closed markers; wrong offsets or exceeded credit close that stream. `stream.error` reports asynchronous input
rejection and does not close output. Native herdr CREDIT follows the same consumption boundary. Viewer credit is
still not negotiated; slow viewers use explicit closure/repaint rather than simulated end-to-end acknowledgement.

Resource ids (`r1`, ...) are vqro's and stay stable across core restarts while the backend keeps the resource.

The shared native herdr adapter also caps native pending calls at 1024. Effect deadlines retire cancelled reply
senders within 100 ms; cancelled commands still queued before native dispatch are skipped. Late attachment
responses retain only bounded cleanup tokens so their returned channel is explicitly closed. At 1024 such tokens
the native connection is closed/recovered, never replayed. Hello and initial subscription each have a five-second
startup deadline; hello framing is capped at 1 MiB. These are distinct from the frontend pending-call limit.

The native herdr adapter reconciles a finished `terminal.kill` reply into resource facts even when no finished
event arrives with an attachment open. It retains unknown exit codes as null and does not replace newer-incarnation
records with an old reply. Its `events.subscribe` DATA is an incremental **NDJSON byte stream**, not one JSON
object per frame. Coalesced and split records are decoded with a 16 MiB unfinished-record bound; malformed/oversized
records close/recover the native link rather than silently discarding observations. Subscribe/`missed` snapshot
sequence cutoffs suppress queued older events. Exact per-frame payload-byte CREDIT follows a FIFO consumer barrier:
the native core acknowledges after applying facts, and `vqro-herdr` after its bounded publisher accepts prior facts.
Bounded partial-record acceptance also returns credit, avoiding a record-larger-than-window deadlock. Credit waiting
is off the native session loop. Finite-window evidence is `tests/herdr_event_credit.rs`; the separate real private
load/pruning/forget/no-reconnect regression is `tests/e2e_herdr_drift.py`. No polling reconciler or effect replay.

## Commands

| Method | Params | Result |
|---|---|---|
| `command.register` | `name`, `title?` | the command; `vqro.*` is reserved for the core |
| `command.list` | | `commands` |
| `command.invoke` | `name`, `args?`, `deadline_ms?`, `on_behalf_of?` (invocation proof) | whatever the owner returns; closing owners can be unavailable/disconnected, removed names are `not_found` |

Built-in, non-replaceable commands (recovery surface, D15): `vqro.layout.reset`, `vqro.revert`
(default: latest client-authored or client-started extension transaction), `vqro.safe_mode`, `vqro.extension.disable|enable|restart`, and
`vqro.shutdown` (implemented: the same ordered stop and accepted-reply flush as `core.shutdown`, including safe mode).
Everything opinionated is an extension (D17).

Command registrations belong to the registering connection, not just its author string. A replacement connection
with the same extension identity may re-register a command; closing the old connection does not disable or reroute
the replacement's command. Commands left registered only by the closing connection are **removed**, not retained
as historical names. The existing `command {name, available:false, ...}` event withdraws the row; clients remove it
from live caches. Built-in core commands remain. The live registry is bounded to 1 MiB of encoded rows/separators,
including names/titles/owners and built-ins. A single oversized row is `too_large`; aggregate pressure is `overloaded`
without changing registration. Same-name replacement releases its old charge before admission; disconnect frees it.

`vqro.revert` accepts an omitted `txn` for default/latest selection. A supplied target must be an unsigned JSON
integer; strings, floats, booleans, objects and explicit null are `invalid` before any undo selection or mutation,
never silently treated as latest.

### Command attribution and undo (D24)

A forwarded command receives `command.invoke {name, args, caller, invocation, on_behalf_of}`: `caller` is the immediate
invoker, `on_behalf_of` is the original invoker's author label, and `invocation` is an opaque proof for this callback.
An extension returns that **proof**, not an author label, in the `on_behalf_of` request field on `workspace.apply`,
`workspace.revert` or a nested `command.invoke`. The core validates the proof against the callback's owning connection
and in-flight scope; forged, cross-connection, completed, expired or revoked-parent proofs fail `forbidden` before a
workspace write is admitted. Nested commands preserve the original invoker, with no longer lifetime than their parent.
Proofs are not grants or user identities (D8); they cannot restrict an agent's ordinary workspace primitives.

History and entries events retain actual writer `author` plus optional **author label** `on_behalf_of`; proofs are never
persisted or broadcast. Legacy history without the field remains valid. Default `vqro.revert`/TUI `u` keeps the existing
**global** history behavior across windows/reconnects: it selects the latest non-extension writer, or extension write
whose original invoker was not itself an extension. Autonomous extension writes/event reactions stay excluded. Explicit
transaction revert and CAS conflict rules are unchanged; this does not introduce a per-user identity or undo stack.

The Python SDK propagates proofs automatically within the command-handler thread, including nested calls, without
mutating caller dictionaries. Event callbacks and detached worker threads do not inherit them. Complete user writes
before replying: an explicit proof used after reply/timeout is rejected, rather than silently reattributed. Detached
work without a proof is autonomous. A write validated/admitted while the command was active keeps its attribution if
commit completion outlives the callback, disconnect or receipt, with the usual no-replay durability rules.

Active proofs are ephemeral: at most1024 entries and8MiB encoded attribution metadata, nesting32; full admission returns
`overloaded` before callback dispatch. Scope expiry follows the requested command deadline and all ancestors. Proofs
are retired on callback completion/error/cancellation; expired entries are pruned on admission. They do not resume
across core restart, and this mechanism does not make native execution effects reversible.

Forwarded invocations default to a 30-second deadline. `deadline_ms` may override it from 1 to 300000 ms; built-in
commands are local and are not timed by this forwarding deadline. Expiry returns `timeout` with
`data.outcome: "indeterminate"`: the handler may already have run and is not cancelled. No resend occurs. Late
replies are ignored; timeout/cancellation removes the pending call entry. The Rust `Peer::call` helper independently
has a 60-second default; callers needing longer waits use `call_with_timeout` with an appropriate local deadline.

## Safe-mode recovery (D21 revised)

`vqro.safe_mode {on}` is a built-in command (`command.invoke`): entering/re-entering stops all extensions and
provider transport links, clears the session allowlist and rejects unapproved provider registration. Backend-owned
terminals are not killed. `vqro serve` has no in-process backend: herdr/tmux are declared provider extensions. Explicit in-process
adapters used by tests/embedders also remain stopped in safe mode and have no chooser entry. Leaving restores ordinary enabled declarations
and reopens the explicitly configured embedder adapter. Repeated leave does not restart it. Entering safe mode
preserves failed-extension status and last-exit diagnostics.

`vqro.safe_mode.allow {name,on}` temporarily starts/stops one declared extension while safe mode is on. It never
writes `extension/<name>.enabled` or history; an explicit allowance may override a saved disabled value for this
session. Healthy repeated allowance is idempotent, not a provider restart. Unknown names fail `not_found`; use
outside safe mode fails `conflict`. Provider owner removal immediately fences old generations/streams, closes
links and rejects late registration. Snapshot fields `safe_mode_allow: [name]` and `provider_extensions: [name]`
are session-only; `safe_mode {on,allow,provider_extensions}` events keep clients current.

The Rust client opens a built-in recovery popup when safe mode is on, also reachable through the prefix menu:
provider/backend and UI groups, descriptions, checkboxes, Enable selected and Leave safe mode. Classification uses
`extension/<name>.kind: "provider"` or remembered authenticated registration owners. Managed processes receive a
fresh `VQRO_EXTENSION_TOKEN` for each supervised lifetime and send it in `hello.extension_token` alongside their
name. A name alone is a label, not supervision proof. Safe-mode provider admission requires a still-running,
allowed lifetime; registration rechecks it after asynchronous waits. Credentials are not persisted or published,
and are stripped before auto-starting hc services so terminals do not inherit them. Provider
entries may also declare `default:true`; that default-backend migration is independent of this chooser. Recovery
works without any extension, retains the prefix inside the popup, and ignores extension themes while in safe mode.
Snapshots/reconnects preserve unapplied checkbox choices and do not reopen a dismissed chooser; a new safe-mode
transition opens it. Failed declarations show their retained last-exit reason.

## Contributions (D17)

Extensions contribute `menu/*` and `binding/*` entries carrying `owner`; clients hide them while that extension is
disabled, removed, not explicitly allowed in safe mode, or when their command is unavailable.

Contributions are reconciled on activation (helper behaviour, `extensions/vqro_ext.py` `contribute`). An extension's
first contribution of a run is its whole declared set, so entries it owns (`owner` = its name) under `menu/`,
`binding/` and `view/` that are not in that set are deleted in the same compare-and-set transaction. A new
version that drops an item leaves no ghost. On a conflict the helper reloads and retries. Entries of other owners and
other namespaces are never touched. Extensions that contribute over several calls opt out (`exclusive=False`) or
reconcile later with their full set (`exclusive=True`). Other runtimes should follow the same rule.

- `menu/<id>`: `{where: "pane"|"tab"|"tabbar"|"status", label, icon?, command, args?, input?, prefill?, pick?: "resource", when?,
  order?}`. `icon` (see Icons) is drawn before the label; an empty label shows the icon alone. `input` makes the client prompt for text (passed as `args.text`); `pick` makes it offer a choice (`args.picked`);
  `when` is matched field by field against the invocation context.
- `binding/<id>`: `{key, command, args?, where?, label?, priority?, input?, prefill?, pick?, hidden?, when?}`. `key` is a prefix key
  (`"n"`, `"Left"`, see Keys) or a gesture (`"mouse.right"`, `"mouse.middle"`) with `where` = `"pane"` or `"tab"`.
  `input`, `prefill` and `pick` work as for menu items; `hidden` keeps a key out of the prefix menu (it still works).
  `when` (prefix keys) is matched against the focused pane's context exactly like a menu item's: a binding whose
  `when` does not hold is not a candidate there, so the key falls through to the next binding for it by the usual
  rank (priority, then defaults, then id), or does nothing and says so. It is checked before any `input` prompt, so a
  pane-specific key never asks for text it cannot use. The prefix menu lists what each key does on the focused pane.
  Example: the agents extension binds `p` to `agents.prompt` with `when: {"resource_kind": "agent"}`; a keymap
  profile's `p` (priority 10, no `when`) still wins everywhere when selected.
- `prefill` explicitly supplies an input's initial text: a string, or `{"context": "field"}` to read a string
  from invocation context. Without it, inputs start empty; pane/tab titles and option `value` never implicitly
  become input text. Missing/unknown context fields or unsupported values produce empty input. Options may
  override their parent row's explicit prefill. For example pane rename asks for `{"context":"title"}`, while
  tab rename uses `{"context":"tab_title"}`; ordinary agent prompts provide none.
- Clients add `args.context` when invoking from a place: `{layout, layout_version, path, view, kind, resource,
  resource_kind, live, title, tab_title?}` for a pane (`resource` is the view's `resource` field, for terminal views
  and any other view that names the resource it shows, such as agent panels; `resource_kind` is that resource's
  `kind`, or null), `{layout, layout_version, tab_path, tab_index, tab_title}` for a tab.
  `tab_title` is the current/innermost tab's title, or null when none exists. Use `layout_version` as `expect`.
- Client-side commands (mechanism, never sent to the core): `client.menu {where}`, `client.prefix`, `client.palette`,
  `client.zoom` (toggle: the focused pane fills this window; per window, not workspace state; moving focus leaves it).
- More client-side commands, so bindings can reach what the prefix keys do: `client.focus {dir: next|prev|left|right|
  up|down}` (directions pick the adjacent pane), `client.tab {to: next|prev|<n>}` (n counts from 1, in the tab group
  holding the focused pane), `client.quit` (this UI only), `client.none` (does nothing: binds a key to nothing). A
  client answers an unknown `client.*` command with a message and never sends it to the core.

## Keys

- **Prefix.** `doc/settings/tui.prefix` names the prefix key (default `C-g`). It must have Ctrl or Alt, or be a
  function key; anything missing, unparsable or unsuitable means `C-g`, never "no prefix". Clients apply it live,
  show it in their prompt, and send it to the program when it is pressed twice.
- **Key names** (tmux-like): modifiers `C-` (Ctrl), `M-` (Alt), `S-` (Shift; named keys only, since a character
  already carries Shift: `O`, `%`), in any order, then a character or one of `Up Down Left Right Home End PageUp
  PageDown Insert Delete Tab BackTab Enter Esc Backspace Space F1`..`F12`. Matching is case-insensitive for names
  and modifiers (`C-space` = `C-Space`), exact for characters. A key that doesn't parse never matches.
- **Display labels.** `doc/settings/tui.key_labels` is `"full"` (default) or `"short"`; missing, non-string or
  unknown values use full names. The native TUI formats `C-g`, `M-x`, `S-Tab`, `C-S-Left` as `Ctrl-g`, `Alt-x`,
  `Shift-Tab`, `Ctrl-Shift-Left` by default, including the prefix prompt/menu, status hints, help and panel key
  hints. Short mode restores compact notation. `BackTab` displays as `Shift-Tab`, `Esc` as `Escape`, and paging
  hints use `PageUp/PageDown` in full mode. This works with all extensions off and in safe mode; it does not
  change stored keys, matching or bytes sent to programs. Editable configuration values retain their syntax.
  Settings exposes the choice through the generic `settings.set` Interface select. Extensions embedding key ids
  in text read this same workspace option and use `vqro_display.key_labels(text, setting)`; Settings' Keys page
  and key-profile labels do this without new RPC or rich-text fields. Native and Python formatters share the
  test table `tests/fixtures/key_labels.json`. Unknown/future key tokens stay literal; formatting never writes
  bindings or profile state. Other clients may retain their own presentation, ignoring this native TUI option.
- **Precedence.** When several visible bindings share a key (or a gesture and `where`), the highest `priority`
  wins (an integer, default 0), then the client's own defaults, then the lowest entry id. A winning binding to
  `client.none` unbinds the key, so a profile can remove a key without deleting another extension's entry.
- **Client defaults** are bindings at priority 0: `:` `client.palette`, `o`/`O` `client.focus` next/prev, `1`-`9`
  `client.tab`, `u` `vqro.revert`, `R` `vqro.layout.reset`, `!` `vqro.safe_mode`, `q` `client.quit`.
- **Recovery survives any keymap.** The prefix menu (prefix key, the clickable `vqro` badge, or `client.prefix`)
  lists the winning binding of each key, then any recovery action (commands, undo, reset layout, safe mode, quit)
  that no key reaches any more, without a key. Safe mode hides every extension's bindings.
- **Live feedback.** While the prefix menu is open a client may ask the terminal for richer key reports (the TUI
  uses the kitty keyboard protocol when supported) to show held modifiers in the prompt; after a key runs it
  echoes it (`Ctrl-g z → zoom pane` by default). Held modifiers use dimmed full words (`Shift-`, `Ctrl-`, `Alt-`)
  or compact labels according to the display choice; key names
  also accept these long forms (`Ctrl-a`, `Shift-Tab`).
- **Keys sent to programs** are encoded like an xterm (`src/tui/keys.rs`, `encode`): cursor keys, Home and End as
  `CSI A`..`F` (`SS3` in application cursor mode, DECCKM), with modifiers `CSI 1;m X` where m = 1 + Shift + 2·Alt +
  4·Ctrl; Insert, Delete, PageUp/Down and F5–F12 as `CSI n~` / `CSI n;m~`; F1–F4 as `SS3 P`..`S` / `CSI 1;m P`..`S`;
  Ctrl combinations as control bytes and Alt as an ESC prefix. Keys legacy bytes can't distinguish (Shift- or
  Ctrl-Enter, -Tab, -Backspace, Ctrl-Shift-letter, Ctrl-1, Ctrl-;) are sent the legacy way (Shift-Enter is Enter;
  Ctrl-1 nothing), **unless the focused program asked for extended keys** in its output: xterm modifyOtherKeys
  (`CSI > 4;n m`, n ≥ 1) gives `CSI 27;m;code~`; the kitty keyboard protocol with its "disambiguate" flag
  (`CSI > 1 u`, also `CSI = … u`, popped with `CSI < u`) gives `CSI code;m u`, also for Alt combinations and Esc.
  The client tracks these requests from each terminal's output (kept across a reattach repaint; RIS resets them).
  A viewer that starts later (a restarted UI, another client, a new view) relies on the backend to repeat the
  current modes in its attach history: herdr-core's first paint does, as `CSI = 0;1u` `CSI = flags;1u` and
  `CSI > 4;n m` (measured on hc 0.1.21; tested end to end by `tests/e2e_tui_keys.py`, UI restart). A backend
  that doesn't repeat them leaves later viewers on legacy keys until the program sets its modes again; the core
  does not reconstruct them (D14). pi-tui pushes kitty flags 7 unconditionally and accepts `CSI 13;2u` as
  Shift-Enter even without a reply to its `CSI ? u` query.
  While the focused program has asked, the TUI asks the outer terminal for "disambiguate" reports so such keys
  reach it at all; this needs an outer terminal with the kitty keyboard protocol (Ghostty, kitty, WezTerm, foot,
  recent iTerm2); with others Shift-Enter stays Enter. Only "disambiguate" is emulated: other kitty flags (event
  types, alternate keys, all keys as escape codes) are not, and the client does not answer the kitty query
  `CSI ? u`, so programs that probe before enabling keep their legacy behaviour. Programs inside the vqro-owned
  tmux server talk to tmux, which consumes their mode requests: there extended keys depend on tmux and arrive
  the legacy way. The prefix key always works and is never sent on unless pressed twice.

The bundled `keys` extension owns keymap profiles (`vqro`, `tmux`, `screen`; tables and gaps in
`extensions/keys.py`). Applying one with `keys.profile {value}` replaces its `binding/keys.*` entries (priority 10)
and sets the prefix; the active profile is `doc/keys/profile`, and the list is live data `keys`
(`{profile, profiles: [{label, value}]}`) for the settings page.

## Live view content and popups

- `view.publish {view, content}`: live content merged over a view's entry (e.g. `{"items": [...]}`). Not saved,
  not in history; broadcast as event `view.content`; cleared when the publishing connection disconnects, so a
  panel falls back to its entry (typically "not running"). Snapshots include `live`. A different live author cannot
  overwrite or clear the publisher (`conflict`), matching command-registration collision hygiene. Verified same-
  extension replacement connections may take over; late old disconnects do not clear the replacement. Live encoded
  sizes and the total are cached: each publish encodes only its candidate, not all other live content.
- **Read-only panel references (`embedded_views`).** A panel's ordinary `items` (including nested `children`)
  may contain `{type:"view", view:"source-id", readonly:true, text:"fallback"}`. Declare `embedded_views` in
  `requires`. The client displays the source panel's current ordinary items in place, without another writer or
  publication; source entry/live changes and removal invalidate the containing panel. Only declared panel views
  or undeclared live fragments with an `items` array are eligible; terminal sources never attach. Source toolbar,
  pages and detail panels are not embedded by this initial capability. Missing/unsupported sources, cycles and
  reference nesting beyond eight show `text` (or a built-in unavailable placeholder). At most 2048 source rows are
  expanded, with a visible bound warning. Referenced rows are always read-only: commands/args/input/options/menu,
  interactive chips and detail panels are not imported; text/description/detail and nested children/badges remain.
  Control text is escaped and text fields are bounded to 8192 characters. Parent-owned controls/context remain
  separate. `readonly:false` does not enable interactions. References do not create effects, persistence, backend
  lifetime or resource-retention promises. Older clients reject the declared capability gracefully rather than
  silently showing an incomplete composition; the badge/prefix recovery path remains independent.
- A command's reply may ask the invoking client for UI: `{"client": {"popup": {"view", "width"?}}}`,
  `{"client": {"close": true}}`, or `{"client": {"focus": {"view": "view-id"}}}`. These fields may be combined.
  Per client and temporary; clients refuse terminal views as popups. `focus` leaves zoom and focuses an already
  visible view after the command's layout changes; it never opens a view or changes another client's focus.
  Missing/hidden targets are ignored. Older clients ignore unknown reply fields. Extensions choose what to show;
  the focus mechanism is independent of resource/view kind. A drop-down, prompt or picker opened from a popup is
  drawn over it, anchored at its row; the popup stays visible underneath and takes input again when that closes.
  The prefix key cancels it and opens the prefix menu.
- Panels: `{title?, items: [row]}` or `{title?, pages: [{title, items}]}`. Rows:
  `{type?: "item"|"toggle"|"header"|"separator"|"text", text, id?, detail?, detail_style?, description?, checked?,
  style?, disabled?, badges?: [{text, style?}], children?: [row], expanded?, command?, args?, input?, prefill?, pick?,
  menu?: [menu item], icon?: an icon (see Icons) before the text, in the icon column, icon_style?}`, plus `chips`
  rows (`{type: "chips", chips: [{text, icon?, checked?, style?, command, args?, input?, prefill?, options?}]}`, wrapping; a
  chip with an icon and empty text is the icon alone) and `select` rows (`{type: "select", text, value, options: [{label, value}], command}`);
  anything with `options` opens a pick-one menu and adds `value` to its args; an option with `input` (e.g.
  "custom…") prompts for text instead, passed as `text`. The TUI makes every pick-one list with more than 10
  rows (option menus, row/pane menus, the resource picker) searchable: typing filters it (case-insensitive
  substring), the title shows "n of m", arrows/Enter/clicks act on the kept rows, Esc clears the text before
  closing. Client-only; extensions need nothing new. `toolbar: [row]` pins rows above the
  list. Clients handle `client.panel {action: expand_all|collapse_all|filter|clear_filter}` for the panel it came
  from. Panels list needed features in `requires`. Unknown types render as plain items. Selection, expansion, page, filter and scroll are client
  state. Styles are semantic names resolved through the theme (below): `bold`, `dim`, `muted`, `warn`, `error`, `ok`,
  `accent`, `plain`; unknown names render plain. A badge may also carry `color` (a theme colour value) to show a
  colour as data, e.g. a swatch (feature `badge_color`; clients without it show a neutral badge). Use styles for
  everything else, so extensions follow the theme.
- `ui.notify {text, style?}`: a transient message every client shows briefly (event `notify`). Not state.
- Status-bar menu items may name a live `view`; clients take the label, icon and style from its content
  (`{label, icon?, style}`), hiding it while the label is empty.

### Optional list with detail (`list_detail`)

Any panel, or individual page, may carry `list_detail: {requires: ["list_detail"]}`. Its ordinary `items`
remain a complete plain-list fallback. A selectable row may carry `detail_panel: {title?, requires?, toolbar?,
items: [row]}`. Selecting it shows that content beside the list at wide widths, or below it at narrow widths.
These are ordinary rows/actions, not an extension-specific client screen. `list_detail` enhancement requirements
are scoped inside that object: an unknown requirement disables the enhancement, not the fallback list. Do **not**
put this optional feature in the outer panel's mandatory `requires` if older clients should retain the list.
Older clients ignore the extra objects; row `menu` actions can provide the same controls in the plain list.

- Selection, pane focus and each pane's scroll are per-client state. Detail scroll/selection resets when the
  selected row id changes, not on a status refresh. List clicks select without running the row command; clicking
  a toggle's checkbox, or Enter/Space on the list, explicitly activates it. Detail actions activate normally.
- Tab/Shift-Tab switches list/detail focus. Within either pane arrows retain normal tree/chip semantics.
  `[`/`]` selects outer pages; PgUp/PgDn moves by the visible selectable-row count; Home/End selects first/last.
  The list shows matching `n of m`, visible row range and a scrollbar; scrolled groups retain a readable header.
- `/` filters the list over text, detail, description, optional `summary` and string `tags: [...]`. Header rows
  with expanded children form groups and remain when descendants match. Headers/toolbars do not count as results.
- Text rows may use `wrap:true` (feature `wrap`) for plain word-wrapped paragraphs and `focusable:true` for
  keyboard reading/scrolling; they remain inert unless they explicitly carry a command. Styles stay semantic.
- Mouse wheel scrolls the detail text independently. A keyboard activation after manual scrolling first reveals
  its selection rather than invoking an off-screen action. The prefix menu remains reachable inside popups and
  while filtering, regardless of extension failure.

Illustrative row (inside an enhanced panel/page):
```json
{"id":"record-a","text":"Record A","description":"One-line summary","tags":["example"],
 "command":"example.open","detail_panel":{"title":"Record A","requires":["wrap"],
 "items":[{"type":"text","text":"A short explanatory paragraph.","wrap":true,"focusable":true},
          {"text":"Open","command":"example.open"}]}}
```

### Client rendering under load

The TUI coalesces background invalidations to a 60 fps frame budget; input-triggered UI changes remain immediate
and keyboard input takes priority over queued background work. Total fps can therefore exceed 60 while typing.
All resource events and terminal bytes are still consumed in order: drawing is coalesced, not protocol data.
Merged panel values and flattened rows are reused while content and page/filter/expansion remain unchanged;
entry/live/snapshot updates invalidate caches. Selection/scroll, current geometry, theme and icons stay fresh.
The default sidebar separately batches derived publications at most twice/second in the extension, not core,
and publishes one bounded tree page rather than every off-screen resource. Page choice is shared extension
state (not history); filtering applies to that page, while its popup resource chooser lists the full inventory.
See `research/tui-scale.md` for the opt-in private probe, measured floor and non-enforced placeholder budgets.

## Themes

The active theme is the entry `doc/settings/theme` = `{name, palette: {token: colour}}`. Clients read it live and
draw their own chrome with it; terminal content keeps the colours its program set. Every client has a built-in
default palette, so it looks right with no theme and no extensions. A token the theme leaves out, or whose colour
the client can't read, keeps its default; unknown tokens are ignored (a theme may carry tokens for other clients).
An empty palette means the client's own default. In safe mode clients ignore the theme (recovery, D15).

Colours: a name (the 16 ANSI names such as `cyan`, `darkgray`, `lightblue`), `#RRGGBB`, a 256-colour index
(number or numeric string), or `default` for the terminal's own colour.

| Token | Used for | TUI default |
|---|---|---|
| `text` | ordinary text; `plain` and `bold` | `default` |
| `muted` | secondary text: inactive tabs and pages, labels, `text` rows, status message; `muted` style | `gray` |
| `dim` | details, descriptions, disabled rows, hints; `dim` style | `darkgray` |
| `accent` | headers, values, buttons, scrollbar thumb, open page; `accent` style | `cyan` |
| `ok`, `warn`, `error` | the matching styles, badges and toasts | `green`, `yellow`, `red` |
| `prompt` | keyboard modes: prompt line, open vqro badge, filter | `magenta` |
| `border` | unfocused borders, rules, separators, scrollbar track | `darkgray` |
| `border_focus` | the focused pane, popups and menus | `cyan` |
| `selection`, `selection_text` | the selected row; unset `selection` means reverse video | unset |
| `badge`, `badge_text` | neutral badge and unchecked chip fill; text on any filled background | `darkgray`, `black` |
| `surface` | fill of plain chips | `#282830` |
| `tab_active`, `tab_active_text` | the active tab | `blue`, `white` |
| `tab_bar` | background of the whole tab bar row | `235` |

The bundled `themes` extension publishes its themes as live data `themes` and applies one with
`themes.apply {name}`; settings shows them on its Theme page.

## Icons

Extensions ask for icons by name; the active icon set says how each name looks (D20). An icon (row `icon`, chip
`icon`, menu item `icon`, a status segment's live `icon`) is one of:

- a string: one or two characters, drawn as is;
- `{name?, nerd?, text?}`: `name` is an icon name, or a list of names tried most specific first (an agent that
  is working: `["working", "agent"]`); `nerd` and `text` are the extension's own glyphs.

The active set is the entry `doc/settings/icons` = `{name, width?, glyphs: {name: glyph}}`. Clients resolve an
icon to: the set's glyph for the first name it has; else the icon's own glyph, `nerd` or `text` by the client's
`icons` setting (`doc/settings/tui.icons`: `auto`, `nerd`, `text`; fonts can't be detected, so `auto` trusts
terminals that bundle Nerd Font symbols, Ghostty and WezTerm, or `VQRO_NERD_FONT=1`; `nerd` falls back to `text`);
else nothing. With no set, or an empty `glyphs`, every icon looks as the extension drew it. Unknown names, and
glyphs a client can't use (empty, control characters, wider than two cells), fall back the same way, so sets and
extensions can evolve independently. In safe mode clients ignore the set (recovery, D15).

Width is in terminal cells by Unicode width (UAX #11, as `unicode-width` measures it): most Unicode and Nerd Font
glyphs take 1, emoji 2. A set's glyphs share one icon column of `width` cells (1 or 2; default 1), widened to its
widest glyph; clients pad narrower glyphs to it and put one space after, so text after icons lines up across a list.
An own glyph wider than the column widens only its own row. Inline places (status bar, tab bar, chips) need no
column: the glyph, a space, the label. Panels whose rows rely on named icons, or icons on chips, list
`icon_names` in `requires`.

| Name | Marks |
|---|---|
| `repo`, `folder` | a workspace that is a git repository, or a plain folder |
| `worktree` | a git worktree of a workspace |
| `terminal`, `agent` | a terminal; one whose program reports agent status (OSC 7501) |
| `working`, `blocked`, `done`, `error` | an agent's reported state (tried before `agent`); `blocked` also counts waiting agents |
| `add`, `filter`, `refresh` | actions: add (a tab, a workspace), filter a list, refresh derived state |
| `settings`, `sidebar` | the buttons that open them |

The names are an open set: an extension may use others, and a set may leave any out. The bundled `icons`
extension publishes its sets as live data `icons` (`{data: [{name, title, description, credit, width, glyphs}],
preview, names}`) and applies one with `icons.apply {name}`; settings shows them, with the `icons` setting, on its
Icons page.

## Default extensions

On first run the core declares the extensions listed in `<extensions dir>/defaults.json` (`VQRO_EXTENSIONS_DIR`,
default: the per-platform extensions directory, `~/.local/share/vqro/extensions` on Unix and
`%LOCALAPPDATA%\vqro\extensions` on Windows, not implemented yet) and records `doc/vqro/defaults`, so removing one sticks. Bundled:
`tabs`, `panes`, `gestures`, `workspaces`, `sidebar`, `themes`, `icons`, `keys`, `settings` (`extensions/`, helper
`vqro_ext.py`), and two backends as provider extensions (D21; human decision 2026-11-17):
- `herdr`: `vqro-herdr --name herdr`, `kind: "provider"`, `default: true`. Terminals in herdr-core, reached with
  `$VQRO_HERDR` (JSON argv, e.g. `["ssh","box","hc","connect"]`, or a single literal executable), else `hc connect`.
  `--hc PATH` with repeated `--hc-arg ARG` passes literal prefix arguments before `connect`; PATH lookup uses
  platform-native path splitting. A legacy whitespace command string must be converted to JSON argv.
- `tmux`: `vqro-tmux --name tmux`, `kind: "provider"`, `enabled: false`. Terminals in a private tmux server owned by
  vqro, never the user's tmux sessions.

`vqro serve` runs no backend in process; `--fake` adds the in-process fake for tests. A bare `vqro-*` program in a
default stays bare in stored state and resolves on each start: executable sibling of the running `vqro`, then
platform-native `PATH` search (including `.exe` on Windows). An unavailable program reports `failed` and an
actionable diagnostic; hello/snapshot `backend_notice` and the built-in status bar expose unavailable backends.
`entries`, `provider` (and compatibility `adapter`), and `safe_mode` event data also carry the current
`backend_notice` (string or null). Null clears a previous notice; absent fields from older cores leave it unchanged.
The status bar prioritizes this recovery warning over contributed status buttons, including when an alternative
backend is connected but the configured default is unavailable.
Only unchanged installed defaults with an old absolute program path are normalized; custom/edited entries stay intact. `defaults.json` is versioned (`version`; entries may carry
`since`). Each entry is offered once per state dir: all on first run; later, only entries newer than the recorded
version. This is how existing installs gain new defaults (here herdr, tmux and the split Pi launcher). An entry that exists is left as it
is, and a removed default stays removed. Client options live in `doc/settings/tui`, the theme in `doc/settings/theme`, the icon set in
`doc/settings/icons`.

## Extensions

| Method | Result |
|---|---|
| `extension.list` | `extensions`: per declaration, runtime facts and static manifest metadata (below); no process ids |

Declared by `extension/<name>` entries `{argv, enabled?, cwd?, kind?, default?, description?, manifest?}`. `kind: "provider"`
marks a backend or runtime provider (the safe-mode chooser groups by it). `default: true` makes the backend that
registers under that name the provider for spawns that name none. Without one, the first backend to register is
used; a runtime bridge never is. A declared default that has not registered rejects unnamed spawns as unavailable;
registration order must never route them to a different backend. Other `argv[0]` values are resolved by the platform's process
launcher (`PATH` on Unix; `CreateProcess` search on Windows, where a `.py` file is not directly executable and
`python3` is often absent). The bundled `defaults.json` names `python3`, which is Unix-only for now; Windows defaults
need a portable interpreter choice such as the `py -3` launcher (`build-plan.md`, Windows work). Started with `VQRO_SOCKET`,
`VQRO_EXTENSION`, `VQRO_EXTENSION_TOKEN`, and `VQRO_EXTENSION_LIFETIME=stdin`. Stdout/stderr are drained through
bounded pipes into `<state>/ext-<name>.log`, with one `.log.1` backup, each at most 1 MiB. Rotation works during a
single long-lived generation and across overlapping replacement cleanup; there are no child-held log file descriptors.
All log filesystem work runs off the core mutex on blocking workers, with one bounded chunk per pump and no
unbounded output queue. A log failure discards diagnostics but continues draining the child; logs are not durable
evidence. Shutdown drains briefly, then abandons pipes a descendant keeps open. Removed extension names' files are
retained for manual diagnosis; the per-name bound is not a global disk quota.
Crash budget: 3 exits in 5 minutes, then `failed` until the entry changes or `vqro.extension.restart`.

### Static manifests and observed facts

A manifest is a UTF-8 JSON object with required string `name` matching the declaration name. Every other field
is optional; unknown fields are retained for forward-compatible consumers and ignored by this UI. It is the same
metadata object a future registry can list; it does not install packages, execute code or resolve dependencies.
Bundled files are `<extensions dir>/manifests/<name>.json`. A declaration may override the path with
`manifest: "path/to/manifest.json"` (relative to the core's extensions directory, or an explicit core-host path).
Reading metadata never starts the extension, so disabled, failed and safe-mode declarations remain discoverable.

Fields:
- `title`, `summary` (one line), `description` (short plain-language paragraph: purpose and what turning it off loses),
  `version`, `kind` (open names such as `provider`, `runtime`, `interface`, `management`), `author`, `license`, `homepage`.
- `requires: {protocol?: integer, client_features?: [string]}`: informational compatibility requirements, not an
  authorization grant or an automatic dependency resolver. `client_features` are UI needs (`ui/vqro.<feature>`).
- `provides: [capability]`, `needs: [capability | {cap, suggest?, why?}]`, `works_with: [capability | {cap, suggest?,
  without?}]` (D29): what the extension offers, what its main purpose requires, and what it merely works better with.
  A capability is `kind/name[@major]` (major defaults to 1): `command/<command>`, `view/<view>` (a trailing `*` names a
  view family, `view/agent-*`), `data/<live data name>`, `doc/<workspace key>`, `ability/<name>` (`vqro.terminals`,
  `vqro.agents`, `vqro.resource.forget`; third parties use their own namespace) and `ui/<client feature>`. `suggest`
  names a provider to offer for enabling or installing; any provider of the capability satisfies the need. The core
  provides `command/vqro.*` and the spec-defined shared settings `doc/settings/tui`, `doc/settings/theme` and
  `doc/settings/icons` (their readers always have a fallback). Declarations are static: actual commands and live data
  stay authoritative. `scripts/check_manifest_deps.py` checks the bundled ones against their source.
- `contributes: {commands?: [string], keys?: [string], menu_items?: [string], panels?: [string], settings?: [string]}`:
  human-readable summaries, not registrations. Actual commands/capabilities remain authoritative.
- `settings_command?: string`: its own settings command, enabled in the catalogue only while running and available.

Files must be regular files and at most **32 KiB**; cached manifest records (including source/diagnostics) have a
**1 MiB** aggregate budget. Malformed, mismatched, unreadable or oversized metadata reports `manifest_error`, never
blocks normal extension execution. No program is run to discover a version. File stamps are checked during
reconciliation and `extension.list`; unchanged parsed values are reused. Snapshots contain the last observed cache
without filesystem I/O. This is not a file watcher or a registry/install mechanism.

**Dependency facts (D29).** `extension.list` and snapshot `extensions` also carry `dependencies` (null without a
manifest): `{status: "ok"|"degraded"|"missing", needs: [row], works_with: [row], used_by: [{name, cap, hard,
display_status}]}`. A row is the declared entry plus `status`: `satisfied` (a provider is running, or the core provides
it), `off` (providers are declared but none runs; `providers: [{name, display_status}]`), `missing` (no declared
provider; show `suggest`), `version` (only another major is provided) or `invalid`. `ui/*` rows resolve against
connected rendering clients' `hello.features` instead: `satisfied`, `lacking` (`lacking_in: [client]`) or `unknown`
(`unknown_in`: clients that declared no features); they never affect `status`. `status` is `missing` when a non-UI
need is unmet, `degraded` when only `works_with` rows are. Events do not carry these facts: refresh with
`extension.list`. Resolution only reports. `vqro.extension.enable|disable` never cascade; their replies add
`needs_unmet: [row]` (enable) or `breaks: [{name, cap}]` (disable: enabled users whose need no running provider meets
any more) so the caller can tell the user. Safe mode, the recovery chooser and built-in commands ignore dependencies.

`extension.list` and snapshot `extensions` export `manifest` (object or null), `manifest_source` (display-only
core-host path or null), `manifest_error`, `enabled`, raw `status`, `display_status` (`running`, `off`, `failed`,
`starting`, `stopped-by-safe-mode`, `provider-not-connected`), `safe_mode`, `allowed_in_safe_mode`, `running`, `generation`, `spec_version`, `crashes`, `crashes_total`, `crash_window_seconds`, `last_exit`, `last_exits`, and `failure`. Unknown
statuses should display as text. `provider-not-connected` means a declared/observed provider extension's process
is running but no provider authenticated to that extension is connected and fully synced; raw `status` remains
`running`. A provider may register under a different name. Settings displays process and readiness separately,
and an available settings command remains usable to repair a disconnected provider. `generation` is an ephemeral supervisor counter (changes on launch/stop, resets
with a core), not an execution-lifetime guarantee. Process ids stay internal. Last-exit evidence survives disabling
or safe entry even when the display badge is off/stopped-by-safe-mode. Logs remain core-host files at
`<state dir>/ext-<name>.log`; no generic in-app log viewer exists, so Settings reports the path and that limitation.

`crashes` counts unexpected exits still in the current restart-budget window (300 seconds), excluding a budget
cleared by explicit restart/spec replacement. It is computed when facts are serialized, so a healthy run ages
old exits out even without another exit. `crashes_total` separately counts unexpected exits since this
declaration's runtime was created in this core: budget resets preserve it; removing the declaration or
restarting the core resets it. Intentional stops are not crashes. `last_exit` remains historical. A crash-budget
`failed` state stays latched until restart/change, even when recent crashes age out; there is no automatic
cool-down retry. `failure` is null or `{kind, reason, at_ms, crashes, window_seconds}`: the frozen cause/time and
recent count at failure (`kind`: `crash_budget`, `startup`, `shutdown`; unknown kinds remain displayable).
`last_exits` retains at most three `{at_ms, reason}` records independently of the budget window; timestamps are
epoch milliseconds. Explicit restart/spec replacement clears the current `failure` and recent budget, not the
historical exit list/total. Removing the declaration/core lifetime ends these ephemeral facts.

Settings shows local failure/exit times, frozen failure counts and recovery instructions beside the existing
Restart action. The native failed-status badge opens built-in recovery; its prefix menu contains explicit restart
rows even if Settings is broken/off. Disabled or safe-mode-disallowed entries are labelled as clearing failure
while remaining stopped; restarting never enables a declaration or bypasses safe-mode admission.

Settings uses these same public facts and generic panel features for every declaration, including itself; it has
no Pi-specific child row or always-on exception. If explicitly allowed to run in safe mode, its on/off controls
use `vqro.safe_mode.allow` session choices rather than editing normal enabled declarations (D21). Turning Settings
off stops that page, not built-in recovery.

**Lifetime channel (all platforms):** a managed extension's standard input is a pipe reserved for lifetime,
not application input or the JSON-RPC connection. Only the launching core holds its sending end open;
it sends no data. EOF means that core has ended, including a crash or forced termination where cleanup
code cannot run. The extension **must monitor this channel independently of RPC and command work and
exit promptly on EOF**, even if it has not connected to `VQRO_SOCKET` yet. It must not keep using a dead
core's socket or reconnect/replay automatically. Wrappers must preserve the channel through to the
extension. This contract requires no process ids, signals or platform-specific parent-death facilities.

`extensions/vqro_ext.py` starts the lifetime watcher on import when the marker is `stdin`; EOF exits the
whole Python process, including blocked command/startup threads. Its existing socket-EOF exit remains
an additional disconnect path. Without the marker (for example a manually launched helper/client), it
does not consume stdin. Other runtimes must implement the same EOF rule. Standard input must not be
repurposed, and ordinary helper subprocesses should not inherit the marker unless they also receive
and follow a suitable lifetime channel.

Explicit stop/replacement and crash-budget behavior are unchanged. Noncompliant programs that ignore
the lifetime channel can still outlive a crashed core; vqro does not promise to enforce this contract
through an external process supervisor. This channel governs **vqro's extensions only**, not backend-owned
terminals, agents, or their process-lifetime guarantees (D14/D15).
