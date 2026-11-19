# vqro provider contract, v0 (proposed)

> **Status (2026-11-17): section 9 implemented; broader D21 migration remains proposed.** vqro is a working
> prototype, and fluid. Core wire, Python/Rust provider SDKs and private fake/tmux integration are implemented;
> backends now run as provider extensions (`vqro-herdr` default, `vqro-tmux` private, off). Essential extensions,
> relay and the proposed safe-mode policy are
> not implemented. Nothing here is frozen (`../roadmap.md`, step 5); it changes as
> the first two implementations on each side (herdr-core and tmux; Pi and Claude Code) teach us.

Evidence keys: **src** vqro source at `main`; **rec** recorded result (file named); **unverified** not yet checked.

## Shape

A backend adapter (herdr-core, tmux, later Rex) and an agent-runtime bridge (Pi RPC first) are both **providers**:
out-of-process programs speaking the core protocol (`core-protocol.md`) over the core's local endpoint (opaque
`VQRO_SOCKET`: a Unix socket or a Windows named pipe; `core-protocol.md`, Endpoint and transport). A provider
**registers** an instance with declared capabilities, **publishes resources** (kinds `terminal`, `agent`; open set)
with stable identity, **serves effects** the core routes to it with deadlines, and **serves streams** with byte
credit. The core keeps a registry, an identity map, routing, bounds and outcome reporting, and nothing specific to a
backend or runtime. Adding one means adding a provider, never editing `src/server.rs`, `src/main.rs` or the TUI
(today `server.rs` and `main.rs` select a built-in `Backend`).

Backend and runtime capabilities keep separate namespaces (`terminal.*`, `agent.*`) over one registration mechanism.
`backend-adapter-v0.md` remains the catalogue of what each backend can do; this file is how a provider talks to vqro.

## 1. Registration and lifecycle

Backend providers are launched and supervised as extensions (`extension/<name>`, `src/ext.rs`). Runtime bridges are
the program of a backend-owned terminal (section 6); vqro does not supervise or restart them (D14).

```jsonc
// provider → core, after hello
{"method": "provider.register", "params": {
  "name": "herdr@local",                 // unique among live providers; default: the extension name
  "role": "backend",                     // "backend" | "runtime"
  "instance": "<opaque, see Identity>",
  "software": {"name": "herdr-core", "version": "0.1.21"}, "provider_version": "0.1.0",
  "capabilities": { },                   // section 3
  "limits": {"max_streams": 512, "max_item_streams": 1024, "stream_window_bytes": 262144}
}}
// → {"provider": "herdr@local", "generation": 7}
// generation: core-assigned, increases on every registration of this name. (The core's event cursor
// has its own boot `epoch`, core-protocol.md; the two are unrelated.)

{"method": "provider.sync", "params": {"generation": 7, "complete": true, "resources": [ /* facts */ ]}}
{"method": "resource.upsert", "params": {"generation": 7, "facts": { }}}
{"method": "resource.gone", "params": {"generation": 7, "ref": "t3", "incarnation": "", "reason": { }}}
```

Rules:
- **Bound to the connection.** A registration belongs to one connection and generation, as command registrations
  already do (`core-protocol.md`, Commands). Messages carrying an older generation are rejected. A second live
  registration of the same name is a `conflict` unless the old connection has closed.
- **Disconnect is not delete.** When a provider disconnects, the core marks it `disconnected` and its resources
  `available: false` (facts kept, flagged stale), closes its streams with `reason: "provider_disconnected"`, and
  resolves its in-flight effects as `indeterminate` (section 5). Views and layouts are untouched.
- **Re-registration.** The next `provider.sync` with `complete: true` is authoritative for that provider. Resources
  whose identity matches come back `available: true`; absent ones are removed and their vqro ids retired. Nothing
  is resent.
- **Several hosts mean one provider per host** (`herdr@rohan`, `tmux@studio`): its own extension entry, process,
  connection, generation and queues. One host stalling or failing affects only its resources. The provider owns
  its transport (`hc connect`, `ssh host tmux -CC`, ...), which is private to it; the core sees only the provider's
  own connection to the endpoint.
- `workspace.snapshot` gains `providers: {name: {role, instance, software, capabilities, state:
  "connected"|"disconnected", generation, since, error}}`, and `provider` events report changes. It replaces today's
  `adapters` field, which carries connection state only.

## 2. Identity

vqro resource ids (`r1`, ...) are never reused. Each maps to **(provider name, instance, ref, incarnation)**:
- **`instance`**: opaque. The provider guarantees that within one instance a ref keeps meaning the same resource.
  herdr-core returns its core identity from `hello` and `server.status` (herdr-core 0.1.21 schema), and keeps
  terminal ids across a core restart (rec `research/review-1-backends.md`, finding 9), so it reports a stable
  instance. tmux reuses `$0 @0 %0` exactly after a server restart (rec tmux-check 2026-11-16, item 5), so a tmux
  provider reports a per-server-lifetime instance (for example the server pid plus its start time; exact formats
  unverified).
- **`incarnation`**: opaque. It changes when the same ref starts meaning a different process lineage, for example
  herdr-core's `restarted` event (`src/adapter/herdr.rs`, `"created" | "restarted"`) if the provider treats that as
  a new lineage. Empty when refs are never reused within an instance.
- A change of instance or incarnation retires the old vqro id (`resource.removed` with `reason: "replaced"`) and mints
  a new one. Views pointing at the old id show it as gone. Rebinding them is an extension's choice, never automatic.
- The identity map is persisted under the store's durability rules; failures are reported as `persistence.error`
  (`core-protocol.md`). Retired records are dropped while a durable monotonic high-water mark prevents their ids
  ever returning, including after loading/pruning older tombstone files.
- An agent's "who am I" is its agent resource id (section 6), not a backend environment variable.

## 3. Capabilities

Declared per provider, exported verbatim in the snapshot; the core never emulates a missing one. Unknown names are
ignored. Backend-specific extras are namespaced (`dev.herdr.machines`).

| Capability | Parameters (examples) | herdr-core | tmux 3.7b |
|---|---|---|---|
| `terminal.spawn` | `{env, cwd, placement?}` | yes; `terminal.run` takes `env` and `on` (0.1.21 schema) | yes |
| `terminal.input` | `{stream, effect}` | stream only; no raw-input RPC | stream only, `send-keys -H` (rec tmux-check item 1) |
| `terminal.output` | `{raw, history, repaint: "screen"\|"none", lossy_when_paused}` | raw, history | raw; repaint via `capture-pane`; lossy when paused |
| `terminal.resize` | `{owner: "latest_viewer"\|"per_client"\|"none"}` | latest viewer (open question 7) | native `resize-window`, then `resize-pane` within layout constraints; latest viewer request sizes the window |
| `terminal.kill`, `terminal.exit` | | yes | yes |
| `status.osc7501` | `{stored, lossy}` | stored | parsed from the stream by the provider, not stored, lost after a pause (rec item 3) |
| `events.resync` | absent: the provider may miss changes it can't report | yes (`missed`) | absent; re-list to resynchronise (rec item 4) |
| `layout.native` | informational; vqro never mirrors it | absent | present |
| `identity` | `{refs_survive_backend_restart}` | true | false |
| `remote.machines` | informational | yes (`on`) | absent; one provider per host |

Extensions read `providers[*].capabilities` and degrade: hide status chips when `status.osc7501` is absent, mark them
possibly stale when `lossy`. Panels can name capabilities in `requires` as today.

**Windows hosts (D12).** A terminal hosted through ConPTY reaches the provider as output re-rendered by the console
host, not the program's raw bytes, and unknown OSC sequences may be dropped (depends on the Windows build and
ConPTY passthrough support; unverified). A provider on such a host declares what it observed:
`terminal.output {raw: false}` and `status.osc7501` absent or `{lossy: true}` until tests show otherwise. The
capability model covers this; no contract change. tmux has no native Windows build, so a Windows host needs another
backend provider.

## 4. Terminal streams through a provider

Every hop is bounded and credit-driven. On `main` the in-process herdr adapter already returns credit only when the
core's bounded per-connection writer accepted the bytes, and slow viewers are closed with `slow_consumer`
(`core-protocol.md`, Resources). **The exact v0 messages are in section 9 and supersede the sketch below**: in v0
a gap ends the stream (no in-stream `stream.repaint`), repaint is the initial data of a fresh open, and credit
follows acceptance by the core's bounded writer, not a viewer acknowledgement.

```jsonc
// SKETCH, superseded by section 9
// core → provider (request)
{"method": "stream.open", "params": {"stream": 42, "ref": "t3", "incarnation": "", "cols": 120, "rows": 40,
  "history_lines": 2000, "window_bytes": 262144}}            // → {"opened": true}
// provider → core (notifications)
{"method": "stream.data",    "params": {"stream": 42, "offset": 1048576, "data": "<base64>"}}
{"method": "stream.gap",     "params": {"stream": 42, "from": 1048576, "to": null,
                                        "reason": "viewer_slow" | "backend_dropped" | "paused"}}
{"method": "stream.repaint", "params": {"stream": 42, "screen": "<base64 VT>", "cols": 120, "rows": 40}}
{"method": "stream.closed",  "params": {"stream": 42, "reason": { }}}
// core → provider
{"method": "stream.credit",  "params": {"stream": 42, "bytes": 65536}}
{"method": "stream.input",   "params": {"stream": 42, "data": "<base64>"}}   // ordered per stream, at most 64 KiB
{"method": "stream.resize" | "stream.close", "params": { }}
```

- **Credit follows consumption.** The viewer returns `stream.credit` after it has consumed bytes; the core forwards
  credit for bytes the viewer took, and a provider never sends past its window. The provider returns credit to its
  backend by the same rule (herdr-core CREDIT frames; tmux `refresh-client -A '%N:continue'`).
- **Fan-out lives in the provider.** When one backend feed serves several viewers (tmux sends one `%output` per pane
  per control client), the provider keeps a bounded buffer per viewer stream. A slow viewer gets `stream.gap`
  followed by `stream.repaint` (when `repaint: "screen"`) or `stream.closed {reason: "overflow"}`; it never stalls
  other viewers.
- **Gaps are explicit; nothing is replayed.** After a gap a viewer resets its terminal state; only a
  `stream.repaint` restores it, and it is labelled a repaint. `offset` lets a viewer detect gaps itself. tmux shows
  why the label matters: below the `pause-after` threshold output is delayed but complete (1.2 MB received exactly,
  `%extended-output` with up to 1.5 s lag); past it the pane is paused, raw output is dropped, nothing is replayed
  after `%continue`, and `capture-pane` recovers screen and history but **not** OSC 7501 (rec tmux-check item 3).
  `%pause` may only arrive after the reader resumes, so a provider can't rely on seeing it in time.
- **Session scope (tmux).** A control client receives output only for panes linked into its attached session
  (rec item 5). A tmux provider either keeps every vqro terminal in one session it attaches, or holds one control
  client per session; either way this is the provider's business, invisible to the core.
- **Core bounds, per connection.** Separate control and data outbound queues with byte caps. Data overflow closes that
  connection's streams with `overflow`; control overflow disconnects the client, which resumes from its
  `(epoch, seq)` cursor or a snapshot. Inbound line length is capped. The numbers come from the scale harness
  (`tests/scale.rs`, whose budgets are placeholders until the human sets them), not from this document. The
  proposed test floor is 350 workspaces, 600 panes and 200 agents over two hosts (three times one observed Herdr
  snapshot; not a measured peak).
- **Encoding.** JSON with base64 for now. Binary framing is a later, measured choice; the credit and gap rules don't
  change with it.

## 5. Effects and outcomes (D10)

Public calls stay resource-centric: `resource.spawn {provider?, kind, ...}`, `resource.kill`, and one generic
`resource.invoke {resource, op, args, deadline_ms?}` for any declared capability (`agent.steer`, ...). The core checks
`op` against the owning provider's capabilities (`unsupported` otherwise) and forwards it:

```jsonc
// core → provider (request)
{"method": "effect.invoke", "params": {"effect": "e-9f2c", "op": "agent.steer", "ref": "a1", "incarnation": "",
  "args": { }}}
// provider reply
{"outcome": "accepted", "data": {"disposition": "queued"}}
```

| Outcome | Meaning |
|---|---|
| `dispatched` | vqro handed it to the provider; nothing more is known (stream input) |
| `accepted` | the layer below acknowledged it (Pi `disposition`, a herdr-core reply) |
| `observed` | the layer below reported that the effect took place (resource appeared, process exited) |
| `rejected` | the layer below refused it; no effect |
| `indeterminate` | sent, but the answer was lost (deadline, disconnect); the effect may or may not have happened |

- **Deadlines.** Every forwarded call has one: the caller's `deadline_ms`, else a core default. When it expires the
  caller gets `timeout` with `outcome: "indeterminate"`. In v0 a late reply is ignored, as for forwarded commands
  (section 9.5); observers reconcile from resource facts. A published `effect.outcome` event is OPEN.
- **No automatic resend**, anywhere: not on reconnect, not on restart, not after `indeterminate`. Retrying is an
  extension's explicit choice, with the outcome visible.
- The same deadlines apply to `command.invoke` forwarded to extensions.

## 6. Agent runtimes (bridges)

> **Successor draft (D28):** `agent-v0.md` standardises agent facts, `agent.send {mode}`, the item stream, requests
> and conformance on top of this contract. The Pi capabilities below describe today's bridge until it is ported;
> the "no shared `deliver(mode)`" rule below is replaced by declared delivery modes.

- **Placement.** A bridge is the program of a backend-owned terminal, started by an extension through
  `resource.spawn {argv: ["vqro-bridge-pi", ...]}`. It runs the runtime as its child over fresh pipes (Pi RPC: JSONL,
  not JSON-RPC; split on LF only; drain stdout and stderr separately). The backend owns the process; vqro never
  restarts a bridge. The terminal shows a minimal fallback view, so the real process stays one action away (D11).
- **Reaching the core.** At spawn the core mints a bootstrap bridge credential (also usable as the previous resume credential after first registration; see9.6) and adds `VQRO_SOCKET` (opaque endpoint) and
  `VQRO_BRIDGE_TOKEN` to the terminal's environment (`terminal.spawn {env}`; herdr-core `terminal.run` takes `env`).
  The bridge connects and sends `provider.register {role: "runtime", token}`. The token links its agent resource to
  the hosting terminal (`facts.host`). Not through the PTY: that mixes with output, has no credit, and corrupts on
  any stray byte.
- **Remote hosts (backlog; D23 proposed supersedes D21's relay default).** A bridge on another machine, or in WSL2 when
  the core is native Windows, can't reach the core's local endpoint. The mechanism is backend-neutral and chosen
  later (D23 lists candidates: a stdio connector given as `VQRO_CONNECT` argv, endpoint forwarding started from the
  vqro side, or a backend byte channel as one optional transport). Rules that hold now: the bridge never derives how
  to reach the core; the provider that places it on a host supplies a usable value (`VQRO_SOCKET`, or later
  `VQRO_CONNECT`) or refuses the spawn with `unsupported`; whatever carries it is a byte stream of the same
  LF-delimited JSON-RPC. Today's code injects the core's own endpoint for every bridge spawn (`src/server.rs`,
  bridge spawn), which is correct only for terminals on the core's host.
- **Resource kind `agent`.** Facts: `{status: "idle"|"working"|"blocked"|"done", run: {active, settled, aborted?},
  queues: {steering: [], follow_up: []}, requests: [{id, kind, prompt}], host: "r7"}`.
- **Native capabilities**, declared per runtime, never emulated (D4). For Pi 1.5, from its RPC docs (active-run
  behaviour unverified):

  | Capability | Pi declaration |
  |---|---|
  | `agent.prompt` | `{while_running: "needs_mode"}` (prompt while streaming must say steer or follow-up) |
  | `agent.steer` | `{applies: "after_current_tool_calls"}` |
  | `agent.follow_up` | `{applies: "when_idle"}` |
  | `agent.abort` | `{clears_queue: false, waits_for_idle: true}` |
  | `agent.clear_queue` | `{returns: "texts"}` |
  | `agent.answer` | `{kinds: ["select", "confirm", "input", "editor"]}` |
  | `agent.correlation` | `{per_input: false}` (events carry no originating input id) |
  | `agent.command` | `{v: 0, declared_in: "data.agent.commands", args: "string"}` |
  | `agent.configure` | `{v: 0, declared_in: "data.agent.config", observed: "get_state"}` |

  Claude Code declares its own set later. There is no shared `deliver(mode)`.
- **Accepted is not settled.** `resource.invoke` returns `accepted` with the runtime's disposition. Completion is a
  fact and an event (`agent.settled {aborted}`) at the level of a run; the contract never promises that a given input
  completed.
- **Config options and commands: agent standard v0 section 6.3, adopted ahead of the rest of `data.agent`.** A
  runtime that has them declares `agent.configure` / `agent.command` and publishes, republished whenever they change:
  - `data.agent.config: [{id, label, description?, category?, type: select|boolean|string|number, value,
    choices?: [{value, label, description?}], rev}]` (`category` open: `model | mode | thought_level | _…`);
    `agent.configure {v, id, value, rev?}`. A stale `rev`, an undeclared id or a value outside `choices` rejects
    (`stale` / `unsupported`) before any native write. The outcome is `observed` only once the runtime reports the
    new value, else `accepted` or `dispatched`.
  - `data.agent.commands: [{name, description?, input?: {hint}}]`; `agent.command {v, name, args?}` with `args` a
    string runs only an advertised name. Slash text inside a plain send is never executed.
  `data.agent` carries only `v`, `config` and `commands` until the bridge adopts the rest of the standard; the other
  top-level facts are unchanged. Pi: commands `new_session`, `compact` (args: custom instructions), `cycle_model`, plus
  Pi's own `get_commands` entries (templates, skills, extension commands), which `agent.command` sends as the prompt
  `/name args`. Because Pi expands such text in any prompt, a plain `agent.prompt`/`steer`/`follow_up` that starts
  with an advertised `/name` (or `/skill:`) is rejected `unsupported`, nothing sent. Config: `model` (category
  `model`, value `provider/modelId`, choices from `get_available_models`), `thinking_level` (`thought_level`),
  `steering_mode` / `follow_up_mode` (`_queue`: `one-at-a-time` | `all`), `auto_compaction` (`_compaction`,
  boolean); every value is read back with `get_state`. Not yet: `caps.rev` bumps for queue-mode changes (the bridge
  does not publish `data.agent.caps`). Clients show both generically (the agents extension's Agent settings popup).
- **Dialogs.** Pending runtime requests are `requests` facts, answered with `agent.answer`. A bridge cancels the kinds
  it can't surface instead of leaving the runtime waiting.
- **View content.** The bridge publishes a bounded transcript window and status with `view.publish` on
  `view/agent-<rid>` (coalesced, size-capped). Richer agent UIs are extensions that read the same facts and events.
  If panels prove insufficient, one generic primitive is added then.
- When the bridge exits, the agent resource is gone (with the bridge's or terminal's reason) and in-flight effects are
  `indeterminate`.

## 7. Core and providers

**Core** (mechanism; works with every extension broken): the provider registry and generations, the identity map and
monotonic id high-water mark, routing of effects and streams, capability checks and export, deadlines and outcomes, per-connection
bounds and credit accounting, disconnect handling, and the recovery commands.
**Providers and extensions** (backend or runtime knowledge, and opinion): every adapter and bridge, the default
backend and per-host choices, spawning agents, delivery policies and queues, agent views, and what to do after
`indeterminate` or gone.

**Safe mode (human-confirmed D21 revision).** Safe mode starts no extensions or providers. The built-in client
recovery chooser offers declared providers/backends and UI extensions with descriptions and checkboxes. Only the
human's session allowlist runs; nothing is privileged as `essential`. Choices never edit persisted `enabled` values.
Provider links stop, not backend-owned execution. Registration during safe mode requires an explicitly allowed
extension owner; choices and remembered owner grouping are exposed in snapshots (`core-protocol.md`).

## 8. Migration and order of work

Already on `main`: crash-durable, failure-atomic commits; connection-bound command ownership; boot epochs on event
cursors with snapshot fencing; the scale harness. Providers reuse these rules.

1. **Internal reshape, no wire change.** Registry, identity tuple, `available: false` on disconnect, capabilities in
   the snapshot, effect outcomes and deadlines, with the in-process herdr and fake adapters behind them.
2. **Bounded streams with credit that follows consumption**, end to end on the in-process herdr path: viewer
   `stream.credit`, gap and repaint, per-connection byte caps. Slow-viewer and flood tests.
3. **Wire protocol.** `provider.*`, `effect.invoke` and the provider side of `stream.*`. The fake backend moves out of
   process as a stdlib-Python fixture, the first external implementation; one conformance suite (identity,
   disconnect, deadlines, gap and repaint, credit, no resend, isolation between providers) runs against both.
4. **herdr-core out of process**, one declared provider extension per host; `Backend`, `--herdr` and `--fake` leave
   `main.rs` and `server.rs`.
5. **In parallel after 3:** the tmux provider and the Pi bridge, each passing the conformance suite; tmux on two hosts
   with failure isolation.
6. A Windows compile and smoke check of the provider transport before any part is a freeze candidate.

## 9. Wire messages (v0, pinned 2026-11-17)

The core side is implemented in `src/provider_wire.rs`; Python SDK/fake and Rust `vqro-tmux` interoperate through
private extension-backed socket tests (`tests/provider_wire.rs`). `vqro serve` runs no in-process backend:
herdr-core is the `herdr` provider extension (vqro-herdr), tmux the optional `tmux` one. Safe mode uses the
empty-start chooser and session allowlist from revised D21, not the superseded essential-provider proposal.

This section is the implementable contract; where sections 1–6 differ, this wins. It answers the SDK gap list
(G1–G20 below refer to it). Items marked **OPEN** carry a recommendation and are not built in v0.

### 9.1 Framing, connection and errors

- JSON-RPC 2.0 over the core's local endpoint (opaque `VQRO_SOCKET`, `core-protocol.md`), one JSON object per line, **LF only** (a trailing CR is stripped; U+2028/U+2029
  inside strings are not separators). Maximum record **1 MiB** on a provider connection; a longer line is a protocol
  error and the core closes the connection (G18).
- **One provider per connection, registered once** (G3, G16). `hello {extension?}` first, then exactly one
  `provider.register`. A second register on the same connection is `conflict`. Re-registering means a new connection,
  a new generation and a full sync; streams are never resumed and effects never resent.
- Every provider-originated message after registration and every core-originated provider message carries
  `generation` (integer). Requests with a wrong generation fail with `stale_generation`; a notification with a wrong
  generation is a protocol error and the core closes the connection (on one connection a mismatch is a bug).
  Generation is present only in request/notification **params**, never a reply envelope. JSON-RPC replies carry
  `id` plus `result`/`error`; a field explicitly defined inside a result (register's `generation`) is ordinary data.
- **Errors** are JSON-RPC errors with `data.kind` (G12). Kinds used here: `invalid` (bad params, bad base64, bad
  dimensions), `unknown_method` (code -32601), `unsupported` (op or stream feature not in the provider's
  capabilities), `not_found` (unknown ref, incarnation or stream), `stale_generation`, `conflict` (duplicate
  register, duplicate effect id, duplicate live provider name), `forbidden` (bad or used bridge token), `overloaded`
  (a bound is full; nothing was done), `timeout` (core-side deadline; `data.outcome: "indeterminate"`),
  `unavailable` (provider disconnected), `backend` (the layer below failed in a way not covered by `rejected`).
  `unsupported` and `stale_generation` are new kinds; `core-protocol.md` lists them once the core implements them.
  SDKs preserve structured remote error data (including refusal reasons, outcome and future fields), distinct
  from local transport/deadline failures. Rust `ProviderError` exposes `remote` and `data`; `with_data` serializes
  extra object fields in a provider reply while keeping its `kind` authoritative, but the core's effect-error
  mapping currently drops that extra data. A remote error does not itself prove no effect: its kind/outcome still
  determines that. Adapter refusals, including agent-v0 `busy`/`stale`, must use `Ok(Outcome::Rejected { .. })`, not
  an error that the core could report as indeterminate (§9.4). Rust `Outcome::Dispatched` represents native handoff
  without acknowledgement, not accepted or observed.
- **Reasons** are always objects `{"reason": <string>, "message"?: <string>, ...}` (G11), matching `stream.closed` on
  `main`. Minimum reason strings: `provider_disconnected`, `slow_consumer`, `gap`, `exited`, `killed`, `replaced`,
  `closed`.

### 9.2 Registration and sync

| Method | Dir | Kind | Params | Result |
|---|---|---|---|---|
| `provider.register` | P→C | request | `name`, `role` (`backend`\|`runtime`), `instance` (string), `software {name, version}`, `provider_version`, `capabilities` (object), `limits? {max_streams?, max_item_streams?, stream_window_bytes?}`, `token?` (required for `runtime`) | `{provider, generation, limits: {stream_window_bytes, max_record_bytes}}` |
| `provider.sync` | P→C | request | `generation`, `resources: [Facts]`, `final` (bool) | `{}` once applied |
| `resource.upsert` | P→C | notification | `generation`, `facts: Facts` | — |
| `resource.gone` | P→C | notification | `generation`, `ref`, `incarnation`, `reason` | — |

- **Capabilities** (G19): an object mapping name → parameter object (`{}` when none). Absent means unsupported;
  `false`, `null` or a non-object value is `invalid` at register. Unknown names are accepted and exported verbatim.
  An op is allowed only if a capability of the same name is declared (`terminal.spawn`, `agent.steer`, ...).
  Capability presence does not imply every transport: explicit `effect: false` excludes generic `resource.invoke`
  (and hence forwarded `effect.invoke`) before dispatch, with `unsupported` and `outcome: rejected`. For
  `terminal.input`, explicit `stream: false` likewise excludes `stream.input`. Missing flags retain legacy
  handler behaviour; unknown parameters are preserved, not invented support guarantees.
  Current native herdr/fake and wire herdr/tmux/Python fake declare `terminal.input {stream: true, effect: false}`.
  They accept literal bytes only on attached streams. Native hc has no raw-input RPC: `terminal.send` adds Enter
  and quiet-time policy, and `terminal.keys` also waits. Neither is emulated, and no hidden temporary attach/resize
  is created to make an unsupported effect appear supported. Callers attach, send bytes, then close explicitly.
- **Admission bounds (native and wire):** 4096 retained resource references per provider; 128 KiB per whole facts
  record (including status/argv/name/cwd, in addition to the 64 KiB data bound); 32 MiB globally for published plus
  pending-IO resource JSON. Candidates reserve space before publication. Registration metadata is at most 128 KiB,
  provider names 128 UTF-8 bytes and instances 256 bytes. The registry admits **1024** providers, enough for one
  provider per agent at target scale plus backends/headroom, with **4 MiB aggregate registration metadata** including
  a 1 KiB accounting floor per provider. These limits are shared by native/wire registration, checked before runtime
  credential rotation. Disconnected runtime names are reclaimed once all identities/resources/pending facts are
  gone; host-bound resume credentials do not depend on retaining an empty registry row. Core-wide lease allocation
  keeps generations monotonic even when such a name returns. Violations return
  `overloaded` or close a notifying provider; global-budget diagnostics identify the provider that hit the bound.
  These are payload/accounting bounds, not an RSS or latency promise.
- **Limits** (G10): `max_streams` is the provider's hard cap on concurrently open terminal streams and
  `max_item_streams` (default 1024) on item streams, separate budgets; the core does not open more and returns
  `overloaded` to the viewer. The effective window is `min(stream_window_bytes, 262144)` and is returned in the
  register result. The current core additionally admits at most 512 terminal streams and 8192 item streams per
  provider (item streams need at least agents × viewers, target 200 × 4 = 800), 128 concurrent effect calls and,
  separately, 1024 concurrent `stream.open` calls per provider; overflow is rejected, never queued without a bound.
  Stream opens are not effects: they never count against the effect limits (core or SDK `max_in_flight`).
- **Sync durability:** each native inventory/wire chunk admits and reserves its changed facts, writes one indexed
  identity-map candidate, then publishes. The final chunk includes authoritative removals in that same replace.
  No id from a failed pre-rename candidate is published; post-rename uncertainty adopts/reports the complete batch.
  Credential revocations for a retired-host batch likewise use one existing durable replace. No input is replayed.
- **Sync** (G9, G20): a sync is one or more `provider.sync` chunks; the chunk with `final: true` ends it. Each chunk
  merges (upserts) its resources as it is applied. At the final chunk, every resource of this provider not listed in
  any chunk of this sync is removed (`reason: replaced` if its ref reappeared with another incarnation, else
  `closed`). An empty `resources` with `final: true` removes all. Duplicate (ref, incarnation) in one sync: last wins.
  If the connection drops mid-sync, nothing is removed. Resources of a new registration stay `available: false`
  until listed. The first sync must start before any `resource.upsert`; upserts during a sync count as listed.
- `upsert`/`gone` are notifications: ordered with everything else on the connection, no acknowledgement. An SDK may
  expose an awaitable "written to socket", never "applied by core".

The Rust SDK's `Provider::registration()` exposes the acknowledged register result, including unknown metadata;
`resume_token()` exposes a nonempty opaque runtime credential when present (§9.6). Clones share that immutable
reply, and a later sync/setup failure does not erase the observed credential. Backend replies without one return
`None`. This is access to evidence already received, not persistence, retry or replay; callers must not log the
credential-bearing reply. `ProviderError` diagnostics redact known token fields recursively (and known token
values echoed in messages); `Config` diagnostics redact its bootstrap token. Explicit `data`/registration/token
access still returns the original sensitive evidence and must not be logged.

### 9.3 Facts (G1)

```jsonc
{"ref": "t3",                // required, provider's id, unique within instance
 "incarnation": "",          // optional, default ""
 "kind": "terminal",         // required, open set: "terminal", "agent", ...
 "state": "live",            // required: "live" | "finished"
 "name": null, "title": null, "cwd": null,   // optional strings
 "argv": [],                 // optional, default []
 "status": null,             // optional: OSC 7501-shaped status object as reported, or null
 "exit": null,               // optional: {reason?, exit_code?, signal?} when finished
 "data": {}}                 // optional, opaque object preserved verbatim (≤ 64 KiB); kind-specific facts
```

The core maps `ref` to today's internal `backend_ref` and publishes resources with vqro fields added (`id`,
`provider`, `available`, and for bridges `host`). Unknown top-level fields are ignored; put extras in `data`. Agent
facts live in `data`: `{run: {active, settled, aborted?}, queues: {steering, follow_up}, requests: [...]}`; the
agent's coarse state goes in `status` like a terminal's (`{state: "idle"|"working"|"blocked"|"done", ...}`), so
existing status consumers and partial resource events (`src/events.rs`) work unchanged.

### 9.4 Effects (G2, G4, G5, G15)

| Method | Dir | Kind | Params | Result |
|---|---|---|---|---|
| `effect.invoke` | C→P | request | `generation`, `effect` (string), `op`, `ref?`, `incarnation?`, `args` (object), `deadline_ms` (integer) | `{outcome, data?, error?}` |

- `deadline_ms` is the **remaining** time at dispatch, 1–300000, always present; default 30000 (as forwarded
  commands). The provider converts it to a local monotonic deadline. The core owns the caller's deadline.
- Result `outcome` is one of `dispatched`, `accepted`, `observed`, `rejected`. `dispatched` means handed to the layer
  below without an acknowledgement; the core passes it through unchanged and never resends. `rejected` carries
  `error: {kind, message}` and guarantees no effect. Protocol problems are JSON-RPC errors instead. If the provider
  cannot know whether an effect happened (lost reply, backend dropped mid-call), it returns a JSON-RPC error with
  `data.kind: backend` or `unavailable`, never a fabricated `rejected` result. The core maps only these protocol
  error kinds to definite rejection: `invalid`, `unknown_method`, `unsupported`, `not_found`, `stale_generation`,
  `conflict`, `forbidden`, `overloaded`, `too_large`. All other provider errors and deadline expiry become caller
  `outcome: indeterminate`, without resend; unknown kinds (including `busy` and `stale`) become `backend` errors.
  Adapter refusals must therefore use the `rejected` **result**, preserving their kind and certainty. Extra
  provider error data is not forwarded to the caller. Core-side pre-dispatch validation/admission remains a
  definite rejection. Providers never send an `indeterminate` result.
- **One response per request.** The provider replies once, when it knows, even after the deadline; the core ignores
  a reply that arrives after it timed out (as forwarded commands on `main`). No invented late `effect.outcome`
  event: independent native facts reconcile what happened; late original replies are ignored.
- `effect` ids are core-generated, unique for its boot epoch, never reused or resent. Active and recently completed
  duplicate ids are rejected with `conflict`; completed ids may be evicted from a bounded recent window, never
  active ids. This is bounded duplicate defense under the no-resend contract, not persistent exactly-once emulation.
  Core wire retains active calls (at most128) plus256 recently completed ids. SDK identity bounds cover active plus
  recent ids and evict completed entries instead of locking out healthy long sessions; only an all-active bound
  rejects new work as `overloaded`. SDK closed-stream ids use the same bounded recent policy; core stream ids are
  monotonic per connection and its stream map retains only active streams. Providers may use native idempotency
  keys internally. Reconnection creates a new generation, never a resend.
- **`terminal.spawn`**: no `ref`; `args {argv, cwd?, name?, env? (object string→string), cols?, rows?, placement?}`.
  An empty `argv` means the provider's default shell for its host (`$SHELL` on Unix, `%ComSpec%` or PowerShell on
  Windows); the core never picks a shell. `cwd` is a host path string in the host's own syntax.
  Result `{outcome: "observed", data: {facts}}`; the core assigns the vqro id from these facts. The provider may also
  send `resource.upsert` for it, before or after the reply; identity dedups. Creation completion does not overwrite
  facts for that already-published exact identity: independent observations may already describe a later state
  (including a finished short-lived terminal). The result exposes the current resource, not a promise it is live.
- **`terminal.kill`**: `ref`, `incarnation`, `args {}`. Result `accepted` (requested) or `observed` (with
  `data.facts` in the finished state). Final facts also arrive as `resource.upsert` (`state: finished`) or
  `resource.gone`; open streams then get `stream.closed`.
- **`resource.forget`** (capability `resource.forget`): `ref`, `incarnation`, `args {}`. Release a **finished**
  resource backend-side and stop reporting it. Result `observed` once released (an unknown ref counts as already
  released); a live resource is `rejected` with `invalid`. The provider also sends `resource.gone` (reason
  `{reason: "forgotten"}`), which frees its inventory slot; the core retires the id on either signal and never
  reuses it. The core only dispatches it for resources it holds as finished. A backend that cannot release
  finished resources does not declare the capability. Bundled: herdr (`terminal.rm`), tmux (exited panes are
  already deleted; clears the record), fake.
- Agent ops (`agent.prompt`, `agent.steer`, ...) use `ref`, `args` per runtime; result `accepted` with
  `data.disposition` (Pi: `started` | `queued` | `handled`).

### 9.5 Streams (G6, G7, G8, G13, G14)

`vqro-tmux` submits requested native dimensions on spawn (80×24 when omitted), before the observed spawn reply,
and on stream open before repaint, as well as subsequent resize notifications. It declares
`terminal.resize {owner: "latest_viewer", scope: "window", pane: "native_layout_constraints"}`: native
`resize-window` manually sizes the containing window, then `resize-pane` requests the pane size. Native split
minimums may constrain the pane; other native clients may resize the same window. Closing a viewer does not
restore an older size. Spawn uses native `new-window` followed by resizing, so a program can initially start at
tmux's creation size and receive SIGWINCH; this does not promise a pre-exec PTY size. tmux coalesces kernel PTY
resizes on its own timer, so native layout command completion is not a PTY-settlement acknowledgement. Unknown capability
parameters remain ignorable; this adds no core sizing arbitration.

| Method | Dir | Kind | Params | Result |
|---|---|---|---|---|
| `stream.open` | C→P | request | `generation`, `stream` (u64), `ref`, `incarnation`, `cols`, `rows`, `history_lines` (0 = none), `window_bytes`; item streams instead: `channel: "items"`, `after?` `{epoch, seq}`, `window_bytes` (no dimensions) | `{initial: "history"\|"repaint"\|"none"}`; item streams `{initial: "continue"\|"reset", epoch}` |
| `stream.data` | P→C | notification | `generation`, `stream`, `offset` (u64), `data` (base64) | — |
| `stream.gap` | P→C | notification | `generation`, `stream`, `offset`, `reason` (`{reason: "gap", gap: "backend_dropped"\|"paused"\|"viewer_slow"\|"retention"}`) | — |
| `stream.closed` | P→C | notification | `generation`, `stream`, `reason` | — |
| `stream.error` | P→C | notification | `generation`, `stream`, `error {kind, message}`, `outcome: "rejected"` | — |
| `stream.credit` | C→P | notification | `generation`, `stream`, `bytes` (u32 > 0) | — |
| `stream.input` | C→P | notification | `generation`, `stream`, `data` (base64, ≤ 64 KiB decoded) | — |
| `stream.resize` | C→P | notification | `generation`, `stream`, `cols`, `rows` (1–9999) | — |
| `stream.close` | C→P | notification | `generation`, `stream` | — |

- **Item channel** (agent-v0 §4). A provider declaring `items.stream` gets `stream.open {channel: "items", after?}`
  and replies `continue` only if `after.epoch` is its current epoch and `after.seq + 1` is still retained (records
  follow from there), otherwise `reset` (an in-stream snapshot fence, then records after its cut), always with the
  current `epoch`. The payload is UTF-8 JSON Lines; the core forwards bytes unparsed. A viewer that falls out of
  retention while attached gets `stream.gap` with gap `retention`. A gap with an unknown or malformed reason is a
  protocol error that still ends the stream (`reason: invalid`); the Rust SDK's `gap()` ends the stream as
  `invalid` too, never leaving it open.
- **Stream ids** are chosen by the core, unique for the life of the provider connection, never reused. Messages for
  a stream that is closed or unknown are ignored by the provider (`stream.credit`, `stream.input` after close) and
  dropped by the core; neither is an error.
- **Units.** All byte counts are **decoded** bytes. `stream.data` carries at most 64 KiB decoded.
- **Credit.** The initial credit is `window_bytes` (the effective window from 9.2). The provider never has more than
  that many unacknowledged decoded bytes of `stream.data` outstanding. The core sends `stream.credit` when its
  bounded per-connection writer has accepted those bytes for the viewer (the rule `main` uses internally; it is not
  a claim that the viewer parsed them). The core never returns more than is outstanding; a provider ignores the
  excess. There is no input-side credit: input is bounded by the core's 256 KiB queue (`overloaded` to the client)
  and a provider refusing new input while its input receiver is active sends `stream.error` with
  `outcome: "rejected"`. This is not acknowledgement of bytes already dispatched before a later native close/gap.
  **OPEN:** viewer-acknowledged credit, only if measurements show the writer rule is insufficient.
- **Offsets.** Per stream, starting at 0, counting decoded bytes the provider has sent on that stream. Each
  `stream.data.offset` equals the previous offset plus the previous length; the core closes the stream with
  `reason: invalid` on a mismatch.
- **An output gap is not an input-delivery acknowledgement.** `stream.gap` (and a provider's `stream.closed`) is
  the last output message for that stream. The core frees its output side and forwards the end/gap to the viewer,
  whose TUI reattaches. Its `offset` is where valid output ended; recovery is a fresh `stream.open`.
  The core sends accepted input and its final `stream.close` in order, including after provider output ends, but
  backend input lifetime is implementation-specific. The Rust SDK retains the command receiver until close;
  vqro-tmux drains it (bounded 30 s). vqro-herdr's pump can stop and close the native attach channel on a gap/end,
  and the Python SDK drops input after a gap. There is **no generic post-gap native-delivery guarantee**.
  `dispatched` reports core-queue acceptance, not backend consumption; bytes accepted before a later close may
  remain indeterminate and are never automatically resent. Do not emulate native input with a hidden reattach.
- **Initial data and repaint.** After a successful `stream.open` reply, the provider sends initial data from offset
  0, as the reply's `initial` says: `history` (backend raw history, e.g. herdr-core), `repaint` (a synthetic screen
  such as tmux `capture-pane`; labelled, not replay, carries no OSC status), or `none`. Repaint bytes consume credit
  like any data. The core passes `initial` to the viewer in the `resource.attach` result (an additive field).
- **Ordering.** The provider must not send `stream.data` before its `stream.open` reply. Per stream, the core sends
  `stream.input`, `stream.resize` and `stream.close` in the order the viewer sent them; any input the provider
  applies is applied in that order. A native close/gap can prevent consumption (above); streams interleave freely. `stream.closed`/`stream.gap` follow all data the provider sent for that
  stream. The core's control-over-data priority applies between different streams and connections, never within one
  stream's data and close. Resize and close are fire-and-forget; their effect is seen in output or `stream.closed`.

### 9.6 Runtime bridges (G17)

- **Placement (reserved, D23 proposed).** Facts may carry `machine: {id, name}` (opaque permanent id, display name)
  and `terminal.spawn` args may carry `placement.machine` (that id). Extensions use these, never ref syntax, to
  group or place resources by machine. Not implemented; additive.

- `resource.spawn {bridge: true, ...}` opts in. Ordinary shells receive no bridge credentials. The core injects
  `VQRO_BRIDGE_TOKEN` and opaque `VQRO_SOCKET` in spawn `env` (native herdr: `terminal.run.env`; wire:
  `terminal.spawn.args.env`). A provider explicitly declaring spawn `env: false` is rejected before dispatch.
- `role: "runtime"` requires `token` (string). Registration can wait up to five seconds for a pending spawn's
  durable host id. Failed/expired pending spawns invalidate bootstrap credentials. Success links published agent
  resources to `host` = that terminal id and returns `resume_token`. Unknown/expired credentials are `forbidden`.
- Bootstrap and resume credentials use the **same `params.token`** field. Resume is durably bound to the exact
  host/provider name/instance and survives core restart; same agent identities can return in a fresh generation
  and inventory without restarting Pi or replaying effects. Host removal expires credentials.
- Rotation retains **current + previous** per host. Presenting current retires the older previous credential;
  presenting previous reissues a current credential. The initial bootstrap becomes previous after first success,
  permitting recovery when the first registration reply was lost. Each successful registration rotates current;
  the presented credential remains previous, so a lost reply is retryable. Only the same bound identity may resume.
  Presenting that previous credential deliberately displaces a live bridge registration for the same bound
  host/provider/instance. A process retaining the bootstrap environment can therefore replace its bridge while
  that credential remains previous. This is lost-reply recovery within the same-user, owner-only socket trust
  boundary, not permission to replay effects or claim a different identity.
- Credentials live in private `runtime-tokens.json`, never snapshots/ordinary logs. File sync/rename/directory sync
  precede acknowledgement. Post-rename uncertainty is adopted and reported as `durability_unconfirmed`, with recovery `data.resume_token`;
  the spawn is never resent.
- **OPEN:** `provider.relay` for bridges on other hosts (D21 proposed; no wire schema in v0). v0 bridges are local.
  Whatever its schema, a relay carries the bridge connection as a **byte stream** (the same LF-delimited JSON-RPC)
  and never an endpoint path, file descriptor or handle, so it works the same over `hc connect`, ssh and named pipes.

### 9.7 Gap list disposition

G1 9.3. G2 9.4. G3 9.1. G4 9.4. G5 9.4 (late replies ignored). G6 9.5 (viewer credit OPEN). G7 9.5. G8 9.5.
G9 9.2. G10 9.2. G11 9.1. G12 9.1. G13 9.4, 9.5. G14 9.5. G15 9.4. G16 9.1. G17 9.6 (relay OPEN). G18 9.1 (other
queue caps: `core-protocol.md`, Resources). G19 9.2. G20 9.2.

## Unverified

Exact tmux formats for a server identity; tmux size arbitration between several clients; herdr-core incarnation
semantics on `keep` restarts; Pi active-run abort, steer and settle behaviour and dialog round-trips; the cost of JSON
with base64 beyond the measured external-fake floor (`docs/research/provider-wire.md`); how `provider.relay` maps onto `hc connect` channels and ssh.
