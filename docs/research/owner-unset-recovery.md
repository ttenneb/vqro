# Owner-unset recovery and explicit bootstrap (2026-11-20)

First coherent post-C2 group. Keep existing audited startup bootstrap; do not turn a missing/invalid owner into
an unaudited guessed identity. A typed owner hello now resolves such state to the core-only owner-unset marker.
It has no effective known principal, so callbacks/native metadata remain undeclared and no implicit coordination
grants/default u arise. Raw workspace/resource/recovery stays usable. The marker itself is not caller-assertable.

`workspace.owner.bootstrap {principal,cause?,correlation?}` takes an explicit desired owner. It reuses the existing
committer Apply/candidate-first persistence/adoption path, not a new store or queue. At serialized prepare it checks
there is still NO valid owner, fences a managed caller's lifetime and any supplied callback proof, and uses the
current row version as CAS. Concurrent initializers have one winner; once an owner exists, bootstrap is refused.

The transaction has a fresh system author/actor/principal, is outside owner-u history, records the logical caller
when known and the actual requester connection in its diagnostic note. Explicit causal links are retained without
upgrading them. No target owner is derived from a caller, process or environment. Durability-unconfirmed adoption
uses the existing truthful result; inspect state, never replay an uncertain request.

The Rust TUI transports the hello state through normal/recovery snapshots as a private client field, keeping it
separate from workspace state. A persistent warning survives cache refresh, missing extensions and safe mode. Only
a new hello/reconnection can clear that connection's marker. The text UI displays the same bootstrap guidance.

Evidence is the new owner_bootstrap core suite (including missing/invalid resolution, no known effective identity,
raw recovery, protected-row refusal, callback principal undeclared, serialized one-winner bootstrap, causal/system
audit, no default-owner undo and no connection retag). Owner legacy recovery exercises a malformed owner in a
workspace too large for startup growth, then raw shrink + explicit repair. TUI tests cover actual hello -> snapshot
-> renderer and persistent safe-mode warning. Red receipts retain the old guessed owner/unknown method/missing UI
notice; focused and release-gate results are reported in the handoff, not promised here.

Post-green edge: the existing owner schema permits an explicitly system-named owner. A real RPC regression
showed principal-only default-u selection could then undo bootstrap. Default selection now excludes actual
core-generated system authors (not labels/claims); ordinary connection work and undo/redo still operate, and
explicit raw revert is unchanged. This fixes system-work classification without inventing target-name policy.

No new coordination policy, native execution guarantee, input replay, install, merge, push or live-state mutation.
