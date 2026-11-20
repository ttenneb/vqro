# P2: cooperative principal, effect author and presser-scoped undo (2026-11-19)

Historical prototype evidence. C2 supersedes the free per-call identity and core coordination-grants stand-in:
see [connection-principals.md](connection-principals.md) and the current core protocol. The measurements below
remain P2 measurements, not claims about the productized identity/default/delegation mechanism.

Prototype from `d904f12`; not a frozen agent standard or a security boundary. Binding refinements came from the
agent-standard coordinator: raw/recovery stays ungated, omitted principal is visibly undeclared, coordinated
operations enforce additive grants, and default u requires the presser's principal. No human/agent kind split.

## Smallest seam

Use optional `params.principal` on the existing named-params transport. The RPC Peer has an optional local default;
client/SDK configuration supplies it, not the server. A handle may override it per call or clone with another
assertion on the same connection. Existing epoch + connection counter supplies the independent boot-scoped actual
author. Existing ownership labels/proofs are retained; no terminal credential, principal registry or identity lookup.

Store metadata is additive: immediate `principal`, core-supplied `actual_author`, optional original
`on_behalf_of_principal`. Existing live command proofs carry the latter; the same helper feeds effects and
coordinated checks. Both ordinary and expanded committer admission charge the encoded metadata, and the candidate
contains it before persistence/publication. Old Store APIs keep their signatures; new context wrappers are additive.

Default undo selection stays inside the committer's prepare step, after preceding adopted commits. It matches the
effective principal (original invoker when proven, otherwise immediate), never the most recent other actor. Explicit
other-actor revert remains raw and attributed to its caller. Existing CAS and durability-unconfirmed semantics stay.

`effect.invoke.author` exposes the same assertion/provenance through Rust EffectRequest and Python Effect. Raw native
effects deliberately bypass coordination grants. A bounded `grants.check` query and a `coordination.invoke` stand-in
use ordinary transaction-managed `doc/vqro/grants`; the stand-in derives right from op and scope from resource, and
rechecks itself. A query is not an authorization token. An owner-asserting callback cannot replace its proven origin.

## Evidence

- Seven baseline tests compile/fail before the change and pass after it (`tests/principal_undo.rs`). They cover
  same-connection different assertions, interleaving/default undo, explicit other's revert, missing/malformed/unknown
  declarations, grants, callback context and restart.
- Additional tests cover caller spoof fields, 256-byte IDs, cross-principal same-key CAS conflicts, a non-person
  configured owner surviving restart, 1024-row/128-KiB limits, malformed grants, wildcards and raw recovery.
- A real Rust provider SDK + fake native effect ledger (`tests/principal_effect.rs`) proves actual authors arrive,
  raw unknown calls stay ungated, owner default works, delegated calls use the original principal, and missing,
  unknown or revoked grants dispatch zero native writes. No model calls.
- A private real tmux backend spawns a CLI from Alice's call with no principal inside its JSON payload. The injected
  assertion produces Alice's transaction. Alice's real TUI u then undoes it while leaving Bob's later transaction
  intact (`principal_from_spawned_pane_is_asserted_and_tui_u_only_undoes_its_presser`). Actual writers distinguish
  observer, spawned CLI and TUI. All processes/servers/endpoints are private; backend cleanup is explicit.
- Worst-table debug probe: 1024 grants, 1000 local RPC queries (900 distinct unknown assertions), p50 2586 us /
  p99 3642 us; retained serialized history stays 202 bytes, one entry and one client. Pinned 16–23/nice10, not the
  P1/P3 measurement cores. This is bounded correctness evidence, not a production throughput guarantee.

## Consequences and open risks

D24's global/type-filtered default must be replaced by principal equality, not a human/agent split. Pre-prototype
history is not backfilled from old connection labels; explicit revert still works. The UI must declare (and should
show) its current principal. Multiple UIs configured owner deliberately share that scope. Embedded callers use
`invoke_as` for default undo; unlabelled raw recovery remains available. SDK background extensions use separate
configured assertions; detached callbacks never borrow a completed invocation proof.

This is still the existing latest-transaction selector, not a new undo stack: repeated u may revert a prior revert.
The debug grant evaluator reparses a bounded document under the core lock; cache a validated entry by version before
making high-rate production claims. No team/scope membership, inbox, pause/resume or native delivery semantics are
implemented here. P1 must carry metadata through history:false without making those writes global u-targets, and
check coordinated authority at its own claim boundary rather than trust a prior grants.check response.
