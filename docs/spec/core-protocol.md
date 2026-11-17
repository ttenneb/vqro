# prim core protocol, v0

> **Status:** this is the living protocol of a **working prototype, and fluid**: it documents what the code does
> today and will change as we learn. Nothing here is frozen yet (`../roadmap.md`, step 5).

Status: implemented in slice 1 (`src/server.rs`), not frozen (D16). JSON-RPC 2.0, one message per line, over the
core's local endpoint (below). Both sides may send requests:
the core forwards `command.invoke` to the connection that registered the command. Errors carry a stable
`data.kind`: `invalid`, `conflict`, `not_found`, `unavailable`, `forbidden`, `backend`, `disconnected`, `io`,
`durability_unconfirmed`, `timeout`, `overloaded`, `unsupported`, `stale_generation`, `too_large`.

## Endpoint and transport (D12, D22 proposed)

- The core listens on one **local endpoint**: a reliable, ordered byte stream per connection, reachable only by the
  user who started the core. `PRIM_SOCKET` names it. Clients, extensions, providers and bridges treat the value as
  **opaque**: they pass it to their platform's connect call and never parse it, build paths next to it or assume a
  file exists. On Unix it is a Unix socket path (mode 0600; default `<state dir>/prim.sock`; implemented). On Windows
  it is a named pipe name (`\\.\pipe\prim-<per-user id>`) whose DACL admits only the owner and which rejects remote
  clients (not implemented yet).
- Messages are UTF-8 JSON, one per line, separated by LF. Open the endpoint in binary mode: no newline translation.
- Stdlib Python on Windows has no `AF_UNIX` (`socket` docs: the `AF_UNIX` constant is absent where unsupported).
  A Python client there connects to the named pipe with `asyncio`'s Windows proactor loop
  (`loop.create_pipe_connection`), not with a synchronous `open()` of the pipe, whose single handle serializes reads
  and writes. `extensions/prim_ext.py` does this once the Windows endpoint exists (`build-plan.md`, Windows work).
- The per-platform state directory holds the store and logs: `PRIM_STATE_DIR`; else on Unix
  `$XDG_STATE_HOME/prim` or `~/.local/state/prim`; on Windows `%LOCALAPPDATA%\prim\state` (not implemented yet).
- **Stopping the core** is a protocol request, not a pidfile and a signal: `core.shutdown` (below, Session). Platform
  signals (Ctrl-C, SIGTERM on Unix) remain extra ways in that run the same ordered stop.

## Session

| Method | Params | Result |
|---|---|---|
| `hello` | `client?` or `extension?` (name) | `server`, `version`, `protocol`, `you` (author id), `epoch`, `seq`, `safe_mode` |
| `core.shutdown` | | `outcome: "accepted"` |

Every change is recorded with the connection's author (`ext:<name>`, `<client>:<n>`).

`core.shutdown` is implemented (broader D22 transport work remains proposed). The accepted reply is locally
written/flushed before stop begins, not a remote-read guarantee. The core stops accepting, closes extension lifetime
pipes (500 ms grace, then kill/reap), stops provider transport links, closes clients/streams, removes the endpoint,
and exits. Backend-owned terminals are not killed; input is never replayed. Bounded cleanup waits report
unconfirmed cleanup honestly. Dispatch/restarts are rejected while stopping. `prim.shutdown` is the same built-in
recovery command, including safe mode. `prim stop` uses RPC even without a pidfile; only no-answer, timeout or
disconnect uses legacy positive-pid signal fallback, never a protocol refusal. SIGTERM/Ctrl-C use the same cleanup.

## Workspace (prim-owned state, D15)

| Method | Params | Result |
|---|---|---|
| `workspace.snapshot` | | `epoch`, `seq`, `entries` (key → `{version, value}`), `resources`, `commands`, `extensions`, `providers`, `adapters` (compatibility alias), `safe_mode` |
| `workspace.apply` | `ops: [{op: "set", key, value, expect?} \| {op: "delete", key, expect?}]`, `note?` | `txn`, `changes` |
| `workspace.history` | `limit?` | `txns` (newest first) |
| `workspace.revert` | `txn` | the reverting `txn` |

Transactions are validated against the resulting state and applied all-or-nothing. `expect` is compare-and-set
(0 = must not exist). Key namespaces and value shapes: `src/store.rs` header. Layouts may only reference
existing views; `workspace/active` must name an existing layout.

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
committer. Its FIFO admission is bounded by **128 requests and 8 MiB encoded input**, including the active
write; each request has a 256-byte metadata floor. Full admission returns `overloaded` with `outcome: rejected`
and no effect. Candidate copying/validation/encoding and durable filesystem work run off the core lock on a
blocking worker; snapshots, events and terminal I/O can continue while a write waits on storage.

CAS and default undo selection use the state after preceding queued commits have been adopted, not a snapshot
captured when the request arrived. Adoption and entries-event publication are ordered before the next commit
starts. Pending candidates are not visible in snapshot/history. A cancelled receipt, client timeout or disconnect
does not cancel admitted work or its extension reconciliation: inspect state/history, never replay automatically.
An unexpected writer-task failure returns `indeterminate` and stops further commits until restart; it is not a
clean persistence failure or a promise that replacement did not happen. Unadmitted requests remain rejected.

## Size admission and recovery

Writes reject growth above **1 MiB per encoded workspace entry** (key/version/value), **16 MiB total entries**,
**1 MiB per encoded live item**, or **8 MiB total live content**. Error `too_large` includes `outcome: rejected`,
byte count and limit; state/history/sequence remain unchanged. Old oversized state still loads, and deletion,
revert or shrinking a legacy violation remains possible. Snapshots carry `recovery_notice` for legacy entry limits.

The ordinary control backlog remains **4 MiB**. One separately reserved, prioritized **64 MiB maximum reply**
slot stays charged through actual writing: 16 MiB entries + 8 MiB live + 32 MiB resource allowance + 8 MiB
metadata/headroom. A busy large slot returns `overloaded`; a reply over the envelope returns `too_large` **without
disconnecting**. This is not a raised general backlog cap. No effect/input is replayed. The resource envelope
is enforced for native and wire providers: 4096 resources/provider, 128 KiB whole facts, 32 MiB globally retained
published/pending-IO resource JSON, 32 providers, 128 KiB registration metadata/provider, 128-byte names and
256-byte instances. Global-budget errors name the offending provider. Legacy workspace/live state can still
exceed other limits, so oversized recovery remains necessary.

If legacy/external state exceeds the reply envelope, the TUI starts in clearly labelled **recovery-only** mode,
not a partial authoritative workspace. Built-in undo, safe mode, reset, commands and quit remain reachable; CLI
apply/delete also works. Recovery commands/state changes trigger another fenced snapshot attempt. A snapshot
timeout explicitly closes the old peer and reconnects responsively rather than remaining fenced forever.

Incoming methods and queued application ingress are each capped at **4 MiB**, with a 64-byte metadata floor.
Framing, including unterminated records and replies, is capped at 64 MiB by default; registered providers lower
that to 1 MiB, replies included. `Peer::set_max_record_bytes` also checks an in-progress record. Queued response
values have 4 MiB plus one large-response permit, held until the call consumes them. Pending calls and admitted
server requests are capped at 128/connection. Overflow closes ingress explicitly: recover facts, never resend
effects. The Python helper caps notifications at 4 MiB/128 records through callbacks, request handlers at 32,
and pending calls at 128. These encoded/queue bounds exclude allocator overhead, framing/serialization copies
and a consumer-held record.

## Events

| Method | Params | Result |
|---|---|---|
| `events.subscribe` | `from?` (seq), `epoch?` (opaque string, required with `from`), `types?` (list of event types), `partial_resources?` (bool) | `epoch`, `seq` |

The event log retains at most **10,000 records and 8 MiB encoded JSON** (64-byte floor). Byte eviction, an
unretainable record, or a replay batch too large for the control backlog produces `missed`; subscription stays
active. `reason: backlog_budget` requests a snapshot instead of repeatedly disconnecting on that replay batch.
Client resync buffers have the same 8 MiB byte cap and restart recovery on overflow.

Subscription options shape what one subscriber receives; they never change the log or other subscribers.
- `types`: only events of these types arrive (unknown names are allowed; `missed` always arrives). Caches built
  from a type you don't receive go stale until the next snapshot.
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
(re-snapshot). `effect {effect, provider, generation, method, author, outcome, error?}` audits the result observed by
the caller, including an indeterminate timeout; it is not a late-reply notification. Late original replies are
ignored, with no invented `effect.outcome` event; independent resource facts reconcile the backend.

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
events temporarily remain aliases so existing extensions continue to work.

A provider registration/start is connection-bound and gets a new generation. Old event sources, queued commands,
effect replies and streams cannot act through a replacement connection. The native adapter's connection token is
internal, distinct from the core-assigned public generation. `provider.register`, chunked `provider.sync`,
`resource.upsert`/`gone`, provider effects and credited streams are implemented as pinned in `provider-v0.md`
section 9. Requests/notifications carry generation in params; reply envelopes never do. Wrong-generation requests
return `stale_generation`; wrong-generation notifications close the connection. Wire provider records, including
records queued before registration, are capped at 1 MiB. Relay remains unimplemented.

`resource.spawn {bridge:true}` opts into `PRIM_BRIDGE_TOKEN` + opaque `PRIM_SOCKET` spawn environment. Runtime
registration may wait five seconds for durable host identity and returns `resume_token`; bootstrap and resume use
same `params.token`. Private durable current+previous credentials bind exact host/provider/instance and expire
on host removal. Lost-first-reply recovery, rotation and core-restart identity restoration are tested. Ordinary
spawns receive no token. Native herdr remains the default; no proposed essential/safe-mode policy is implemented.

`terminal.exit {exit_code:true|false}` declares whether numeric exit codes are natively reportable; absent/unknown
parameters imply no numeric-code guarantee. Never manufacture a code from an unknown reason.

A resource's identity is `(provider, instance, backend_ref, incarnation)`. IDs never reuse a retired identity's id.
Herdr's instance is the stable `hello.core.id` (not pid/uptime); `created_ms` identifies a terminal incarnation.
The fake gets a random instance per start. Instance/incarnation changes retire old ids and emit
`resource.removed {id, reason: {reason: "replaced"}}`; an authoritative inventory removes absent identities with
`reason: {reason: "closed"}`. Views/layouts are never rebound automatically. A disconnect instead keeps known facts
with `available: false, stale: true`; the same instance and identity returns with its original id after fresh facts.

Identity reservations and tombstones are durably persisted before publication. A pre-rename failure publishes no
new prim id. Because the backend spawn may already have happened, the call returns `io` with
`data.outcome: "indeterminate"`, `backend_outcome: "observed"`, `provider`, `instance` and `resource_facts`
(including `backend_ref`); never resend automatically. Facts are retained internally and assigned ids after an
authoritative sync or a later successful map write. A post-rename directory-sync failure adopts the identity but
returns `durability_unconfirmed`, like the workspace store; later success must confirm durability. Legacy two-field
maps cannot prove an instance: their ids become tombstones, new ids are assigned from the first native inventory,
and old layouts may show gone resources until explicitly repaired/reset. No identity is guessed during migration.

## Resources (backend-owned, D14)

| Method | Params | Result |
|---|---|---|
| `resource.list` | | `resources` |
| `resource.spawn` | `argv?` (default: the provider's default shell for its host), `cwd?`, `name?`, `env?`, `cols?`, `rows?`, `placement?`, `bridge?` (default false), `provider?` (`adapter` alias), `deadline_ms?` | `outcome: observed`, `effect`, `resource` |
| `resource.kill` | `resource`, `deadline_ms?` | `outcome: accepted`, `effect`, `backend` |
| `resource.attach` | `resource`, `cols`, `rows`, `history?` (backend scrollback), `deadline_ms?` | `outcome: accepted`, `effect`, `stream`, `initial` (`history`, `repaint` or `none`) |
| `resource.invoke` | `resource`, `op`, `args?` (object), `deadline_ms?` | `outcome`, `effect`, `data?`; capability-checked generic provider operation |
| `stream.input` | `stream`, `data` (base64) or `text` | `outcome: dispatched` (also accepted as a notification) |
| `stream.resize` | `stream`, `cols`, `rows` | |
| `stream.close` | `stream` | |

Spawn, kill and attach check declared capabilities and provider/resource availability before dispatch. Their default
deadline is 30000 ms, override 1–300000 ms. Expiry is `timeout` with an indeterminate outcome; connection or resource
loss while in flight is `unavailable` with an indeterminate outcome. Pre-dispatch unavailability is rejected.
Late replies are ignored; independent resource facts reconcile what happened. No effect is automatically resent.

Published resources include `provider`, `instance`, `incarnation`, `available`, `stale`, and opaque kind-specific
`data` (object, at most 64 KiB); `adapter` and `backend_ref` remain for compatibility. Unknown kinds stay opaque.
Attach/resize dimensions are 1–9999. Unavailable views show that state and reattach when availability returns.

Streams push `stream.data {stream, data}` (base64) and `stream.closed {stream, reason}` (reason is an object
with a `reason` string). Input is handled in order; one input message may contain at most 64 KiB of decoded bytes.
A full bounded input queue returns `overloaded` without dispatching that input. A rejected input notification
produces `stream.error {stream, error, outcome: "rejected"}` instead; it is never replayed.

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

Resource ids (`r1`, ...) are prim's and stay stable across core restarts while the backend keeps the resource.

The in-process herdr adapter also caps native pending calls at 1024. Effect deadlines retire cancelled reply
senders within 100 ms; cancelled commands still queued before native dispatch are skipped. Late attachment
responses retain only bounded cleanup tokens so their returned channel is explicitly closed. At 1024 such tokens
the native connection is closed/recovered, never replayed. Hello and initial subscription each have a five-second
startup deadline; hello framing is capped at 1 MiB. These are distinct from the frontend pending-call limit.

The native herdr adapter reconciles a finished `terminal.kill` reply into resource facts even when no finished
event arrives with an attachment open. It retains unknown exit codes as null and does not replace newer-incarnation
records with an old reply. Its `events.subscribe` DATA channel returns exact payload-byte CREDIT only after a FIFO
consumer barrier; the native core acknowledges after applying facts, and `prim-herdr` after its bounded publisher
accepts prior facts. Credit waiting is off the native session loop. Finite-window fixture evidence is in
`tests/herdr_event_credit.rs`; real scale receipts are separate from this fixture claim.

## Commands

| Method | Params | Result |
|---|---|---|
| `command.register` | `name`, `title?` | the command; `prim.*` is reserved for the core |
| `command.list` | | `commands` |
| `command.invoke` | `name`, `args?`, `deadline_ms?` | whatever the owner returns; `unavailable` if its owner is disconnected |

Built-in, non-replaceable commands (recovery surface, D15): `prim.layout.reset`, `prim.revert`
(default: last change not made by an extension), `prim.safe_mode`, `prim.extension.disable|enable|restart`, and
`prim.shutdown` (stop the core, as `core.shutdown`; contract, implementation pending).
Everything opinionated is an extension (D17).

Command registrations belong to the registering connection, not just its author string. A replacement connection
with the same extension identity may re-register a command; closing the old connection does not disable or reroute
the replacement's command. Commands left registered only by the closing connection become unavailable.

Forwarded invocations default to a 30-second deadline. `deadline_ms` may override it from 1 to 300000 ms; built-in
commands are local and are not timed by this forwarding deadline. Expiry returns `timeout` with
`data.outcome: "indeterminate"`: the handler may already have run and is not cancelled. No resend occurs. Late
replies are ignored; timeout/cancellation removes the pending call entry. The Rust `Peer::call` helper independently
has a 60-second default; callers needing longer waits use `call_with_timeout` with an appropriate local deadline.

## Contributions (D17)

Extensions contribute `menu/*` and `binding/*` entries carrying `owner`; clients hide them while that extension is
disabled, removed or in safe mode, or when their command is unavailable.

Contributions are reconciled on activation (helper behaviour, `extensions/prim_ext.py` `contribute`). An extension's
first contribution of a run is its whole declared set, so entries it owns (`owner` = its name) under `menu/`,
`binding/` and `view/` that are not in that set are deleted in the same compare-and-set transaction. A new
version that drops an item leaves no ghost. On a conflict the helper reloads and retries. Entries of other owners and
other namespaces are never touched. Extensions that contribute over several calls opt out (`exclusive=False`) or
reconcile later with their full set (`exclusive=True`). Other runtimes should follow the same rule.

- `menu/<id>`: `{where: "pane"|"tab"|"tabbar"|"status", label, icon?, command, args?, input?, pick?: "resource", when?,
  order?}`. `icon` (see Icons) is drawn before the label; an empty label shows the icon alone. `input` makes the client prompt for text (passed as `args.text`); `pick` makes it offer a choice (`args.picked`);
  `when` is matched field by field against the invocation context.
- `binding/<id>`: `{key, command, args?, where?, label?, priority?, input?, pick?, hidden?}`. `key` is a prefix key
  (`"n"`, `"Left"`, see Keys) or a gesture (`"mouse.right"`, `"mouse.middle"`) with `where` = `"pane"` or `"tab"`.
  `input` and `pick` work as for menu items; `hidden` keeps a key out of the prefix menu (it still works).
- Clients add `args.context` when invoking from a place: `{layout, layout_version, path, view, kind, resource, live,
  title}` for a pane, `{layout, layout_version, tab_path, tab_index}` for a tab. Use `layout_version` as `expect`.
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
- **Precedence.** When several visible bindings share a key (or a gesture and `where`), the highest `priority`
  wins (an integer, default 0), then the client's own defaults, then the lowest entry id. A winning binding to
  `client.none` unbinds the key, so a profile can remove a key without deleting another extension's entry.
- **Client defaults** are bindings at priority 0: `:` `client.palette`, `o`/`O` `client.focus` next/prev, `1`-`9`
  `client.tab`, `u` `prim.revert`, `R` `prim.layout.reset`, `!` `prim.safe_mode`, `q` `client.quit`.
- **Recovery survives any keymap.** The prefix menu (prefix key, the clickable `prim` badge, or `client.prefix`)
  lists the winning binding of each key, then any recovery action (commands, undo, reset layout, safe mode, quit)
  that no key reaches any more, without a key. Safe mode hides every extension's bindings.
- **Live feedback.** While the prefix menu is open, and only then, a client may ask the terminal for richer key
  reports (the TUI uses the kitty keyboard protocol when supported) to show held modifiers in the prompt; after
  a key runs it echoes it (`C-g z → zoom pane`). Held modifiers display as dimmed words (`Shift-`, `Ctrl-`,
  `Alt-`); key names also accept these long forms (`Ctrl-a`, `Shift-Tab`).

The bundled `keys` extension owns keymap profiles (`prim`, `tmux`, `screen`; tables and gaps in
`extensions/keys.py`). Applying one with `keys.profile {value}` replaces its `binding/keys.*` entries (priority 10)
and sets the prefix; the active profile is `doc/keys/profile`, and the list is live data `keys`
(`{profile, profiles: [{label, value}]}`) for the settings page.

## Live view content and popups

- `view.publish {view, content}`: live content merged over a view's entry (e.g. `{"items": [...]}`). Not saved,
  not in history; broadcast as event `view.content`; cleared when the publishing connection disconnects, so a
  panel falls back to its entry (typically "not running"). Snapshots include `live`.
- A command's reply may ask the invoking client for UI: `{"client": {"popup": {"view", "width"?}}}` or
  `{"client": {"close": true}}`. Per client and temporary; clients refuse terminal views as popups.
- Panels: `{title?, items: [row]}` or `{title?, pages: [{title, items}]}`. Rows:
  `{type?: "item"|"toggle"|"header"|"separator"|"text", text, id?, detail?, detail_style?, description?, checked?,
  style?, disabled?, badges?: [{text, style?}], children?: [row], expanded?, command?, args?, input?, pick?,
  menu?: [menu item], icon?: an icon (see Icons) before the text, in the icon column, icon_style?}`, plus `chips`
  rows (`{type: "chips", chips: [{text, icon?, checked?, style?, command, args?, input?, options?}]}`, wrapping; a
  chip with an icon and empty text is the icon alone) and `select` rows (`{type: "select", text, value, options: [{label, value}], command}`);
  anything with `options` opens a pick-one menu and adds `value` to its args; an option with `input` (e.g.
  "custom…") prompts for text instead, passed as `text`. `toolbar: [row]` pins rows above the
  list. Clients handle `client.panel {action: expand_all|collapse_all|filter|clear_filter}` for the panel it came
  from. Panels list needed features in `requires`. Unknown types render as plain items. Selection, expansion, page, filter and scroll are client
  state. Styles are semantic names resolved through the theme (below): `bold`, `dim`, `muted`, `warn`, `error`, `ok`,
  `accent`, `plain`; unknown names render plain. A badge may also carry `color` (a theme colour value) to show a
  colour as data, e.g. a swatch (feature `badge_color`; clients without it show a neutral badge). Use styles for
  everything else, so extensions follow the theme.
- `ui.notify {text, style?}`: a transient message every client shows briefly (event `notify`). Not state.
- Status-bar menu items may name a live `view`; clients take the label, icon and style from its content
  (`{label, icon?, style}`), hiding it while the label is empty.

### Client rendering under load

The TUI coalesces background invalidations to a 60 fps frame budget; input-triggered UI changes remain immediate
and keyboard input takes priority over queued background work. Total fps can therefore exceed 60 while typing.
All resource events and terminal bytes are still consumed in order: drawing is coalesced, not protocol data.
Merged panel values and flattened rows are reused while content and page/filter/expansion remain unchanged;
entry/live/snapshot updates invalidate caches. Selection/scroll, current geometry, theme and icons stay fresh.
The default sidebar separately batches derived publications at most ten times/second in the extension, not core.
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
| `prompt` | keyboard modes: prompt line, open prim badge, filter | `magenta` |
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
terminals that bundle Nerd Font symbols, Ghostty and WezTerm, or `PRIM_NERD_FONT=1`; `nerd` falls back to `text`);
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

On first run the core declares the extensions listed in `<extensions dir>/defaults.json` (`PRIM_EXTENSIONS_DIR`,
default: the per-platform extensions directory, `~/.local/share/prim/extensions` on Unix and
`%LOCALAPPDATA%\prim\extensions` on Windows, not implemented yet) and records `doc/prim/defaults`, so removing one sticks. Bundled:
`tabs`, `panes`, `gestures`, `workspaces`, `sidebar`, `themes`, `icons`, `keys`, `settings` (`extensions/`, helper
`prim_ext.py`). Client options live in `doc/settings/tui`, the theme in `doc/settings/theme`, the icon set in
`doc/settings/icons`.

## Extensions

| Method | Result |
|---|---|
| `extension.list` | per extension: `status` (running, restarting, failed, disabled, safe_mode, stopped), `pid`, `crashes`, `last_exit` |

Declared by `extension/<name>` entries `{argv, enabled?, cwd?}`. `argv[0]` is resolved by the platform's process
launcher (`PATH` on Unix; `CreateProcess` search on Windows, where a `.py` file is not directly executable and
`python3` is often absent). The bundled `defaults.json` names `python3`, which is Unix-only for now; Windows defaults
need a portable interpreter choice such as the `py -3` launcher (`build-plan.md`, Windows work). Started with `PRIM_SOCKET`,
`PRIM_EXTENSION`, and `PRIM_EXTENSION_LIFETIME=stdin`; output goes to `<state>/ext-<name>.log`.
Crash budget: 3 exits in 5 minutes, then `failed` until the entry changes or `prim.extension.restart`.

**Lifetime channel (all platforms):** a managed extension's standard input is a pipe reserved for lifetime,
not application input or the JSON-RPC connection. Only the launching core holds its sending end open;
it sends no data. EOF means that core has ended, including a crash or forced termination where cleanup
code cannot run. The extension **must monitor this channel independently of RPC and command work and
exit promptly on EOF**, even if it has not connected to `PRIM_SOCKET` yet. It must not keep using a dead
core's socket or reconnect/replay automatically. Wrappers must preserve the channel through to the
extension. This contract requires no process ids, signals or platform-specific parent-death facilities.

`extensions/prim_ext.py` starts the lifetime watcher on import when the marker is `stdin`; EOF exits the
whole Python process, including blocked command/startup threads. Its existing socket-EOF exit remains
an additional disconnect path. Without the marker (for example a manually launched helper/client), it
does not consume stdin. Other runtimes must implement the same EOF rule. Standard input must not be
repurposed, and ordinary helper subprocesses should not inherit the marker unless they also receive
and follow a suitable lifetime channel.

Explicit stop/replacement and crash-budget behavior are unchanged. Noncompliant programs that ignore
the lifetime channel can still outlive a crashed core; prim does not promise to enforce this contract
through an external process supervisor. This channel governs **prim's extensions only**, not backend-owned
terminals, agents, or their process-lifetime guarantees (D14/D15).
