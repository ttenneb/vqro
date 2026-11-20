# C2: connection-known principals (2026-11-19)

AS6 replaces P2's per-call principal assertion with a connection default. This is cooperative accident-proofing,
not authentication, a human/agent split, or a stable/frozen contract. The generic wire and data definitions are in
`../spec/core-protocol.md`; coordination policy remains outside core.

## The small mechanism

- `hello.identity` is explicit launcher configuration: `{kind:"principal",principal:"…"}` or `{kind:"owner"}`.
  Binding is immutable for that connection; display names and request labels cannot replace it. Core-derived
  `extension:*`, `host:*`, `system` and `system:*` cannot be asserted by a caller; invalid attempts emit bounded
  `identity.rejected` diagnostics without logging supplied identities or secrets. Same-user code
  can deliberately configure another identity, just as it can bypass the cooperative workspace.
- CLI/provider SDKs use configured `VQRO_PRINCIPAL`, otherwise undeclared. TUI explicitly defaults to the owner;
  an explicit launcher environment can select another principal. Verified extensions resolve to themselves.
  A runtime with no other known actor uses its credential-bound host, never the last endpoint it reported.
- `label` is only a bounded diagnostic string. Core supplies actual `author`, bound `actor`, effective `principal`
  and original logical `initiator`. Native effect fields and callback fields are flat, not author objects.
- The existing opaque callback proof is exclusively `invocation`. `on_behalf_of` means only a target principal.
  Proofs capture effective principal and initiator separately. A represents B -> callback -> write records the
  extension's actor, B as principal, A as initiator. It needs no static B-to-extension grant but gives no standing
  ability to represent B outside that callback. Proofs stay connection-bound, ancestor/timeout-bound and ephemeral.
- Standing representation uses exact `{from,to,until_ms?,note?}` rows under `doc/principals/delegations/`.
  The connection actor must be `to`; it may represent `from`. No transitive lookup, wildcard match, per-right scope,
  deny rule or coordination permission query exists in core. A delegation does not recall accepted native work.
- Generic owner/delegation writes, deletes and explicit reverts use the connection ACTOR, never the effective
  principal, at serialized commit prepare. Delegates therefore cannot mint chains, remove their own expiry or take
  over the owner. A row
  replacement checks both existing and replacement `from`; mixed transactions fail atomically. Queued work sees
  preceding adopted changes, including owner transfer and delegation revocation.
- Bounds: principal/label 256 bytes; delegation note 4096 bytes; table 1024 rows / 128 KiB encoded JSON. These are
  mechanism bounds, not policy. Core owns no principal registry and reads no `doc/agents/grants`.

## Undo, migration and launch

Default u is equality over the effective principal, selected inside committer prepare. Thus forwarded/delegated
changes belong to the principal they were made for, not their delegate/extension. It remains the existing latest
transaction/revert behavior, not a new undo stack; CAS and the undo byte horizon remain intact. Other principals'
ordinary changes remain explicitly revertible, with original attribution retained.

Startup initializes/migrates `doc/principals/owner` with an audited system transaction, never an owner-u target.
The old value, if malformed, is preserved in that transaction's preimage. Old free principal claims without actor
metadata are not backfilled into known-principal history. A rename-with-unconfirmed-directory-sync is adopted and
reported without replay. An oversized legacy workspace that cannot grow retains the default owner for raw
recovery and logs that the bootstrap could not persist; other IO failures remain errors, not success claims.

Generic spawn passes its effective principal into an environment-capable backend unless its launcher supplies one.
The bundled Pi launcher injects a new opaque `p:<uuid>` for each new agent, or preserves an explicit configured
bridge principal. This prevents an ordinary new agent from inheriting its launching TUI's undo scope. The bridge
retains this non-secret value for tools/reconnects while stripping supervision tokens and unrelated internal config.
A multi-endpoint runtime must inject an evidenced endpoint identity into tools, or its host identity; nothing in
core guesses endpoint identity from timing, process ancestry, names or pane placement.

## Upgrade and review notes

Old prototype SDKs must move live proofs from `on_behalf_of` to `invocation`; there is intentionally no ambiguous
fallback. Old free claims are not backfilled: explicit revert by id remains available, but default u excludes them.
Owner hellos resolve once; open TUIs reconnect after an owner transfer. Generic shells inherit the spawner's
principal, and hand-started agents share it unless a launcher injects a distinct principal.

Private native regression reproduced `VQRO_PRINCIPAL=extension:herdr@test` in a terminal started directly by hc,
not vqro. Herdr backend startup now removes it alongside existing supervision credentials. The tmux client already
uses an env-clear service boundary (`src/tmux/client.rs`); no new tmux workaround is introduced.

Ordinary calls with neither proof nor target no longer acquire the extra attribution core lock: immutable actor
plus diagnostic label need no core state. Live proofs and standing delegation still resolve/revalidate against
current state. This is a source-level justification, not an invented performance measurement.

Accepted separate follow-ups after C2 merges: full stream-open attribution/SDK exposure, and a visible owner-unset
recovery flow for missing/invalid owner (no guessed owner or coordination grants). Current oversized malformed
legacy-owner state may prevent an owner TUI hello; this delta does not claim that edge fixed.

## Evidence

- `tests/principal_binding_regression.rs`: unchanged regression on baseline `5139203` records Bob for a connection
  configured as Alice (red); C2 records Alice and keeps Bob only as label (green).
- `tests/principal_undo.rs`: immutable binding, raw/forged fields, owner bootstrap, exact/expired/non-chained
  delegation, old/new row grantors, actor-only chain/expiry/owner-takeover negatives, reserved-identity rejection
  and audit diagnostics, mixed-write atomicity, encoded bounds, legacy claims, restart and CAS.
- `tests/herdr_provider.rs::backend_autostart_does_not_inherit_supervisor_identity`: real private backend service,
  then a terminal created directly by its user; no supervisor identity/credentials in that terminal's environment.
- `tests/commit_off_lock.rs`: queued owner transfer and delegation revocation are observed at prepare; prior
  candidate-first adoption, cancellation, shutdown and admission-bound tests still pass. Fixtures now explicitly
  configure their actor and derive transaction/version expectations relative to the system bootstrap.
- `tests/principal_effect.rs`: actual Rust SDK + fake native ledger receive distinct actual/actor/effective/
  initiator/label fields; denied/revoked standing delegation adds no ledger entry. Scoped forwarding works without
  becoming standing delegation. Raw undeclared native calls remain raw.
- `tests/test_vqro_provider.py`: Python SDK exposes the same flat fields and rejects malformed provenance before
  its native handler runs. `tests/test_vqro_ext.py` checks scoped/concurrent proof propagation and no background leak.
- `tests/undo_attribution.rs --include-ignored`: real private Settings/theme callbacks and actual pane -> CLI ->
  Alice TUI u restore Alice's work while preserving Bob's later change. Private native tmux only; no live sessions.
- `tests/principal_operational.rs`: combined C1+C2 supervised callback stores and publishes effective B / initiator A
  with actual extension actor, without global history or persistent proof. Register/apply retain C1 ownership,
  separate files, queue lanes, snapshot pools, history:false rejection and irreversible-admission semantics.
  The C1 native-ledger crash/claim oracle also passes; its event barrier is an ordinary read, not an identity reset.
- `tests/provider_wire.rs`: host principal follows the existing bootstrap host through restart, not endpoint labels.
- `tests/test_manifest_deps.py`: only generic owner/delegation documents are core-owned; agent grants are not.

The first full run exposed a Pi exit/gone race and retry-status observation on existing main 6d477c5 as well as
C2. Independent review reproduced the blocked-drain cancellation mechanism on both trees. The coordinator owns
those product fixes separately; C2 does not claim to fix them. The retained first failure is not silently discarded
because a later complete run passed. C2's stale retag/legacy-attribution fixture expectations were corrected.

These are Linux/Python 3.14 fixtures and fake-native attribution oracles, not security, production throughput,
model behavior or native consumption evidence. Full release-gate evidence belongs in the C2 handoff after the
current-main/C1 integration; no live install, paid model, push or cutover is performed by this stream.
