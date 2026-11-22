# In-process provider reshape (2026-11-17)

Scope: provider-v0 step 1, not its external wire protocol. Section 9 of `spec/provider-v0.md` is the pinned wire
contract; external registration, sync chunks, bridge tokens, stream offsets/limits and relay remain later work.

## Identity and lifecycle

- `src/provider.rs` owns registry types, connection leases, and the durable identity map. Core routes are fenced by
  generation and resource availability. Queued native adapter commands additionally carry the adapter-local
  connection token, so an old queued effect cannot execute through a newly reconnected transport.
- The map keys ids to `(provider, instance, reference, incarnation)` and retains retired records as tombstones.
  File sync, rename and directory sync precede publication. Pre-rename failure leaves the id counter unchanged;
  post-rename uncertainty is adopted and reported, and later observations must confirm persistence before success.
- Legacy `(adapter, ref)` records have no provable instance. They become tombstones, never implicit new bindings.
  Existing layout entries are not changed; views of retired ids show gone until explicitly rebound or reset.
  This migration was approved by the coordinator; no live prim state exists on the development machine.
- Herdr uses `hello.core.id` as instance, never pid/uptime. Evidence: local 0.1.21 schema
  `<private-notes>`, `CoreInfo.id` (permanent until state is wiped).
  `Terminal.created_ms` supplies the incarnation; this identifies a terminal, not an emulated process lineage.
  No hc command was run. Fake instances are random per start; conformance tests inject stable identities.
- Disconnect keeps facts with `available: false, stale: true`. A same-instance inventory restores ids; a new
  instance or incarnation retires old ids. Old event sources cannot disable a replacement registration.
- Map failure after backend spawn returns `io`, overall `indeterminate`, native `backend_outcome: observed`,
  provider/instance/native facts, and no unpersisted prim id. Pending facts deduplicate by native reference.
  Subsequent sync or successful map persistence recovers them without resending spawn. Failure diagnostics are
  coalesced until `persistence.recovered`; snapshots separately expose identity persistence health.

## Effects and clients

Spawn/kill/attach check declared capabilities and availability, use the shared RPC deadline helper (30 seconds,
1–300000 ms override), and report observed/accepted/rejected/indeterminate outcomes. Native errors with unknown
outcomes are indeterminate. Timeouts and generation loss never resend. Late replies are ignored per pinned 9.4;
independent resource facts reconcile what happened. The ordinary `effect` audit records caller and observed outcome
(D8); the proposed late `effect.outcome` event is not implemented.

Provider capabilities/software/health are exported as `providers` and `provider` events; `adapters`/`adapter` stay
as compatibility aliases. Kind-specific `data` is opaque and preserved. TUI availability is evaluated after merging
partial resource events, so a partial status update cannot accidentally restore a disconnected resource. A real
availability transition reuses the bounded-stream retry/history repaint path.

## Evidence

Before implementation, new public-API regressions failed on the old source: snapshot omitted provider capabilities;
changing an incarnation reused `r1`. Tests now cover same-instance restart, changed-instance and changed-incarnation
retirement, no resurrection, stale source rejection, queued native connection fencing, identity write failure and
legacy migration, IO diagnostic coalescing and pending-fact recovery, timeout/late-reply behavior, in-flight loss,
idle stream closure, and TUI partial availability transitions.

The rebased checkpoint suite passed without warnings (57 unit and 29 integration tests). The default release scale
profile passed every budget at the checkpoint: 600 terminals, 200 status publishers, eight floods, two adapters,
stalled and slow readers; RSS peak 47.96 MiB, slow echo p99 42.70 ms, max 343.70 ms, zero leaked processes.
This includes main's event-volume, status-phase, first-echo and extension-lifetime fixes. Final logs and receipt are
recorded separately in `<private-notes>` and
`reshape-scale-final.json`; the final delivery report identifies the exact tested commits.
