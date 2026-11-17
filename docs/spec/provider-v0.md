# prim provider contract, v0 (proposed)

> **Status (2026-11-17): proposed, not implemented.** prim is a working prototype, and fluid. This is the intended
> public contract for backend adapters and agent-runtime bridges as out-of-process providers (D21, proposed). Today
> the only adapters run in-process (`src/adapter/`). Nothing here is frozen (`../roadmap.md`, step 5); it changes as
> the first two implementations on each side (herdr-core and tmux; Pi and Claude Code) teach us.

Evidence keys: **src** prim source at `main`; **rec** recorded result (file named); **unverified** not yet checked.

## Shape

A backend adapter (herdr-core, tmux, later Rex) and an agent-runtime bridge (Pi RPC first) are both **providers**:
out-of-process programs speaking the core protocol (`core-protocol.md`) over the same socket. A provider
**registers** an instance with declared capabilities, **publishes resources** (kinds `terminal`, `agent`; open set)
with stable identity, **serves effects** the core routes to it with deadlines, and **serves streams** with byte
credit. The core keeps a registry, an identity map, routing, bounds and outcome reporting, and nothing specific to a
backend or runtime. Adding one means adding a provider, never editing `src/server.rs`, `src/main.rs` or the TUI
(today `server.rs` and `main.rs` select a built-in `Backend`).

Backend and runtime capabilities keep separate namespaces (`terminal.*`, `agent.*`) over one registration mechanism.
`backend-adapter-v0.md` remains the catalogue of what each backend can do; this file is how a provider talks to prim.

## 1. Registration and lifecycle

Backend providers are launched and supervised as extensions (`extension/<name>`, `src/ext.rs`). Runtime bridges are
the program of a backend-owned terminal (section 6); prim does not supervise or restart them (D14).

```jsonc
// provider → core, after hello
{"method": "provider.register", "params": {
  "name": "herdr@local",                 // unique among live providers; default: the extension name
  "role": "backend",                     // "backend" | "runtime"
  "instance": "<opaque, see Identity>",
  "software": {"name": "herdr-core", "version": "0.1.21"}, "provider_version": "0.1.0",
  "capabilities": { },                   // section 3
  "limits": {"max_streams": 512, "stream_window_bytes": 262144}
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
  whose identity matches come back `available: true`; absent ones are removed and their prim ids retired. Nothing
  is resent.
- **Several hosts mean one provider per host** (`herdr@rohan`, `tmux@studio`): its own extension entry, process,
  connection, generation and queues. One host stalling or failing affects only its resources. The provider owns
  its transport (`hc connect`, `ssh host tmux -CC`, ...).
- `workspace.snapshot` gains `providers: {name: {role, instance, software, capabilities, state:
  "connected"|"disconnected", generation, since, error}}`, and `provider` events report changes. It replaces today's
  `adapters` field, which carries connection state only.

## 2. Identity

Prim resource ids (`r1`, ...) are never reused. Each maps to **(provider name, instance, ref, incarnation)**:
- **`instance`**: opaque. The provider guarantees that within one instance a ref keeps meaning the same resource.
  herdr-core returns its core identity from `hello` and `server.status` (herdr-core 0.1.21 schema), and keeps
  terminal ids across a core restart (rec `research/review-1-backends.md`, finding 9), so it reports a stable
  instance. tmux reuses `$0 @0 %0` exactly after a server restart (rec tmux-check 2026-11-16, item 5), so a tmux
  provider reports a per-server-lifetime instance (for example the server pid plus its start time; exact formats
  unverified).
- **`incarnation`**: opaque. It changes when the same ref starts meaning a different process lineage, for example
  herdr-core's `restarted` event (`src/adapter/herdr.rs`, `"created" | "restarted"`) if the provider treats that as
  a new lineage. Empty when refs are never reused within an instance.
- A change of instance or incarnation retires the old prim id (`resource.removed` with `reason: "replaced"`) and mints
  a new one. Views pointing at the old id show it as gone. Rebinding them is an extension's choice, never automatic.
- The identity map is persisted under the store's durability rules; failures are reported as `persistence.error`
  (`core-protocol.md`). Retired ids are tombstoned so they never come back.
- An agent's "who am I" is its agent resource id (section 6), not a backend environment variable.

## 3. Capabilities

Declared per provider, exported verbatim in the snapshot; the core never emulates a missing one. Unknown names are
ignored. Backend-specific extras are namespaced (`dev.herdr.machines`).

| Capability | Parameters (examples) | herdr-core | tmux 3.7b |
|---|---|---|---|
| `terminal.spawn` | `{env, cwd, placement?}` | yes; `terminal.run` takes `env` and `on` (0.1.21 schema) | yes |
| `terminal.input` | | yes | yes, `send-keys -H` (rec tmux-check item 1) |
| `terminal.output` | `{raw, history, repaint: "screen"\|"none", lossy_when_paused}` | raw, history | raw; repaint via `capture-pane`; lossy when paused |
| `terminal.resize` | `{owner: "latest_viewer"\|"per_client"\|"none"}` | latest viewer (open question 7) | control client sets window size via `refresh-client -C` (rec item 6); arbitration between clients untested |
| `terminal.kill`, `terminal.exit` | | yes | yes |
| `status.osc7501` | `{stored, lossy}` | stored | parsed from the stream by the provider, not stored, lost after a pause (rec item 3) |
| `events.resync` | absent: the provider may miss changes it can't report | yes (`missed`) | absent; re-list to resynchronise (rec item 4) |
| `layout.native` | informational; prim never mirrors it | absent | present |
| `identity` | `{refs_survive_backend_restart}` | true | false |
| `remote.machines` | informational | yes (`on`) | absent; one provider per host |

Extensions read `providers[*].capabilities` and degrade: hide status chips when `status.osc7501` is absent, mark them
possibly stale when `lossy`. Panels can name capabilities in `requires` as today.

## 4. Terminal streams through a provider

Today the herdr adapter returns credit to herdr-core as soon as a chunk is queued (`src/adapter/herdr.rs:231`), and
connection queues are unbounded (`src/rpc.rs`). The contract makes every hop bounded and credit-driven, with the
viewer as the consumer.

```jsonc
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
  (rec item 5). A tmux provider either keeps every prim terminal in one session it attaches, or holds one control
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
| `dispatched` | prim handed it to the provider; nothing more is known (stream input) |
| `accepted` | the layer below acknowledged it (Pi `disposition`, a herdr-core reply) |
| `observed` | the layer below reported that the effect took place (resource appeared, process exited) |
| `rejected` | the layer below refused it; no effect |
| `indeterminate` | sent, but the answer was lost (deadline, disconnect); the effect may or may not have happened |

- **Deadlines.** Every forwarded call has one: the caller's `deadline_ms`, else a core default. When it expires the
  caller gets `indeterminate`. A late reply is published as an `effect.outcome {effect, outcome}` event so observers
  can reconcile.
- **No automatic resend**, anywhere: not on reconnect, not on restart, not after `indeterminate`. Retrying is an
  extension's explicit choice, with the outcome visible.
- The same deadlines apply to `command.invoke` forwarded to extensions.

## 6. Agent runtimes (bridges)

- **Placement.** A bridge is the program of a backend-owned terminal, started by an extension through
  `resource.spawn {argv: ["prim-bridge-pi", ...]}`. It runs the runtime as its child over fresh pipes (Pi RPC: JSONL,
  not JSON-RPC; split on LF only; drain stdout and stderr separately). The backend owns the process; prim never
  restarts a bridge. The terminal shows a minimal fallback view, so the real process stays one action away (D11).
- **Reaching the core.** At spawn the core mints a one-time bridge token and adds `PRIM_SOCKET` and
  `PRIM_BRIDGE_TOKEN` to the terminal's environment (`terminal.spawn {env}`; herdr-core `terminal.run` takes `env`).
  The bridge connects and sends `provider.register {role: "runtime", token}`. The token links its agent resource to
  the hosting terminal (`facts.host`). Not through the PTY: that mixes with output, has no credit, and corrupts on
  any stray byte.
- **Remote hosts (proposed default, D21).** A bridge on another machine can't reach a local socket. It reaches the
  core **through its host's provider**: the provider offers `provider.relay` and carries the bridge's connection over
  its own transport (a channel in `hc connect`, the ssh session for tmux). The bridge sees the same protocol over a
  different byte stream; the core sees a provider-relayed connection. Alternatives (a small per-host relay, or local
  bridges only) are open question 2.
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

  Claude Code declares its own set later. There is no shared `deliver(mode)`.
- **Accepted is not settled.** `resource.invoke` returns `accepted` with the runtime's disposition. Completion is a
  fact and an event (`agent.settled {aborted}`) at the level of a run; the contract never promises that a given input
  completed.
- **Dialogs.** Pending runtime requests are `requests` facts, answered with `agent.answer`. A bridge cancels the kinds
  it can't surface instead of leaving the runtime waiting.
- **View content.** The bridge publishes a bounded transcript window and status with `view.publish` on
  `view/agent-<rid>` (coalesced, size-capped). Richer agent UIs are extensions that read the same facts and events.
  If panels prove insufficient, one generic primitive is added then.
- When the bridge exits, the agent resource is gone (with the bridge's or terminal's reason) and in-flight effects are
  `indeterminate`.

## 7. Core and providers

**Core** (mechanism; works with every extension broken): the provider registry and generations, the identity map and
tombstones, routing of effects and streams, capability checks and export, deadlines and outcomes, per-connection
bounds and credit accounting, disconnect handling, and the recovery commands.
**Providers and extensions** (backend or runtime knowledge, and opinion): every adapter and bridge, the default
backend and per-host choices, spawning agents, delivery policies and queues, agent views, and what to do after
`indeterminate` or gone.

**Safe mode (proposed, D21).** Safe mode stops extensions, and providers are extensions, so stopping them all would
make every terminal unavailable exactly when the user is recovering. An extension entry may set `essential: true`
(the defaults set it for providers): it keeps running in safe mode, can still be disabled explicitly, and the recovery
surface never depends on it.

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
4. **herdr-core out of process**, one essential provider extension per host; `Backend`, `--herdr` and `--fake` leave
   `main.rs` and `server.rs`.
5. **In parallel after 3:** the tmux provider and the Pi bridge, each passing the conformance suite; tmux on two hosts
   with failure isolation.
6. A Windows compile and smoke check of the provider transport before any part is a freeze candidate.

## Unverified

Exact tmux formats for a server identity; tmux size arbitration between several clients; herdr-core incarnation
semantics on `keep` restarts; Pi active-run abort, steer and settle behaviour and dialog round-trips; the cost of JSON
with base64 at the target load; how `provider.relay` maps onto `hc connect` channels and ssh.
