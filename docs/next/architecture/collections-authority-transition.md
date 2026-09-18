# Collections package authority transition

Status: accepted architecture decision; implementation incomplete.

This document defines the production boundary for making the independently built `vqro.collections` component package the sole Collections policy and document authority. Its durable boundaries remain tracked here. Temporary owners, dates, branch names, and test receipts belong in local execution ledgers rather than this decision.

## Current state

Vqro currently has:

1. neutral terminal containers owned by the host;
2. host-owned Collections policy, persistence, APIs, and client behavior;
3. an external Collections component exercised only through package conformance and semantic-audit gates.

The bundled `vqro.collections` service uses public protocol shapes but executes code compiled into Vqro. It is a migration oracle, not independent extension authority.

## Authority boundary

The host continues to own:

- PTYs and terminal runtime;
- stable workspace, tab, terminal, and neutral container identity;
- container membership, ordering, selection, placement, focus, and referential integrity;
- bounded namespaced state storage and durable compare-and-swap;
- package/runtime admission, capability leases, supervision, and revocation;
- document validation, cached publication, client compatibility projection, and neutral terminal recovery;
- effect validation, idempotency, and persistence coordination.

The external Collections package owns:

- Collection labels;
- archive/restore policy over stable terminal references;
- interpretation of its package-state schema after migration;
- the Collections semantic document;
- document-bound label and archive actions.

The host uses a frozen legacy-to-package importer for shadow setup and final migration. After cutover, ordinary host code treats package policy values as opaque. Client semantics come from the audited Collections document profile, not from host reimplementation of package policy.

The writable package effect surface is one host-validated compare-and-swap of package-owned state. Container or terminal mutation effects are outside this MVP.

## Package-unavailable behavior

Before cutover, package failure leaves the bundled provider authoritative. After cutover, package failure, disable, removal, or startup failure:

- marks Collections unavailable in endpoint/API projection;
- rejects every frozen Collection method with `provider_unavailable`: `collection.list`, `collection.get`, `collection.create`, `collection.add`, `collection.promote`, `collection.select`, `collection.reorder`, `collection.archive`, `collection.restore`, and `collection.close`;
- retains package state unchanged;
- publishes no stale Collections document;
- does not decode package policy in the host and does not reactivate the bundled provider;
- leaves every live PTY reachable through feature-neutral workspace/tab/pane and terminal paths.

Recovery requires the same package generation or an explicit retained-package rollback. There is no host-rendered degraded Collections mode.

## Production package distribution and pinning

The production pin is distinct from the external conformance-test pin. Its catalog record binds package/service IDs, credential-free source URL, exact 40-character Git commit, repository-relative `.vqrox` path, package/manifest/component digests, declared world/capabilities, and Collections profile version.

The MVP is exact-Git opt-in. Installation and each update require explicit digest-bound confirmation. Vqro fetches only the exact commit and deterministic archive, executes no repository build command, and retains the previous verified artifact for package rollback. Curated status grants no authority. Default/offline package bundling is deferred.

A user who has not installed Collections keeps the bundled provider before cutover. After legacy deletion, Collections is unavailable while generic terminal access remains.

## Collections document profile and client projection

`host.document.v2` remains read-only and action-free. R0 publishes only `vqro.collections.document-profile.v1` plus fixtures; it does not generalize v2.

The profile is audited against the v2 forest, exact neutral terminal/container dependency, and exact package-state revision dependency without exposing policy values in the host.

The profile requires one root group per live neutral container. Root order equals neutral container order. A root ID canonically embeds its container ID and its accessible name carries the label. Active terminal slots appear in neutral member order. Archived terminal slots appear under one canonical archive subgroup in neutral member order. Every live member appears exactly once. Text nodes may not carry hidden policy. The host validates shape, references, topology coverage, and dependency freshness; the package decides archive membership.

The read-only v2-to-v1 projection is deterministic:

- preserve root and depth-first child order;
- root groups map to v1 `group`, nested groups to `section`, text to `status`, and terminal slots to `terminal` items;
- use the frozen node text/accessibility mapping;
- use admitted provider generation and audited v2 revision;
- leave legacy v1 actions, required capabilities, and confirmation tokens empty.

The generation-1 Collections structure projection combines neutral host facts with the audited profile:

- ID, workspace, tab, focus, selected member, membership, and index come from neutral state;
- label comes from the canonical root;
- `archived` is true exactly for slots under the canonical archive subgroup;
- projection revision advances when document revision, neutral dependency, provider generation, or availability changes.

Both compatibility outputs are required. The current client joins `HostDocumentsProjectionV1` with `ClientShellStructureProjection.collections`: the structure supplies collection identity, selection, membership, index, and archive flags, while the v1 document supplies provider item order, terminal item labels/references, hierarchy/status text, disabled state, and—on the legacy surface—document actions. The MVP v2 projection keeps the v1 action list empty and routes its two actions through the typed compatibility paths described below. Both outputs must be published from one audited v2 result under the same shell projection revision. The structure projection alone would leave presentation policy in host fallbacks; the v1 document alone cannot represent the frozen archive and Collection method state.

While available, archived members remain in neutral membership/order, appear in the archive section, are omitted from Collections next/previous cycling, may remain explicitly selected, and participate in cascade-close or promote-members like ordinary members. Generic pane focus can always reach their PTYs.

Document-bound actions use the separate typed contract. Label/archive are reversible and require no confirmation token in the MVP. Before cutover, existing archive/restore methods use legacy behavior. After cutover, they translate into typed package actions. Older generation-1 clients receive no new field or reinterpreted method.

If no current audited document exists after cutover, Collections is unavailable and only feature-neutral terminal access remains.

## Thin typed action protocol

R0 publishes only:

- Collections action declarations;
- typed invocation for `set_label` and `set_archived`;
- one package-state CAS effect plan.

The public invocation contains an opaque one-shot host ticket, document/action revisions, typed subject/input, expected state-store generation/namespace revision, and idempotency key. It does not expose a reusable catalog/runtime generation envelope.

The server-side ticket binds complete authority generations and only the changing-data dependencies required by the action:

| Action | Typed input | Host-side fences |
| --- | --- | --- |
| `set_label` | existing or host-reserved prospective container ID plus string-or-null `label` | existing identity or exact create reservation, plus policy/state revision; no terminal-membership fingerprint |
| `set_archived` | container ID, terminal ID, and `archived: bool` | subject membership, policy/state revision, and membership/archive invalidation fingerprint |

An effect plan references the ticket and proposes only bounded state CAS. The host rejects expired, revoked, consumed, under-fenced, cross-scope, or extra-resource plans. Labels are unnormalized UTF-8, use `null` to clear, and are bounded by 4 KiB plus normal frame limits. The MVP has no dynamic choices, arbitrary JSON input, generic effect graph, or reusable authority-token API.

The frozen `collection.create` method retains atomic labeled creation without a package topology effect. The host reserves container/layout IDs and unpublished candidate topology, then issues `set_label` bound to that reservation and base tab revision. A per-tab ticket queues or rejects conflicting topology changes without blocking unrelated App work. After revalidation, the two-image coordinator persists candidate session topology and extension state before one App turn publishes either. Failure exposes neither an unlabeled container nor a label-only record. Unlabeled create remains host-owned topology work.

Cancellation before effect-plan acceptance aborts without a write. After durable-write acceptance, cancellation stops only the caller's wait; reconciliation remains authoritative. Success is visible only after persistence and a fresh audited document.

## Worker and App-actor protocol

No component invocation, host-call wait, filesystem operation, worker join, or durable write may block the App actor.

1. An App turn captures immutable observations and a revocable ticket, enqueues a bounded per-service job, and returns.
2. A worker invokes the component. Host calls become bounded App commands with one-shot replies; the worker holds no actor-owned mutable reference while waiting.
3. Completion returns as data. A later App turn revalidates the server-side ticket.
4. State effects reserve a per-namespace durable-write ticket and run on the persistence worker. Unrelated namespaces continue; conflicting writes queue or fail boundedly.
5. Completion returns for reconciliation, invalidation, and refresh. Late results cannot publish across replacement.
6. Shutdown/replacement revokes admission first, then drains or joins outside the App actor.

Provider execution is per changed scope, not per frame or attached client. Per-service queues prevent global head-of-line blocking.

## Shadow and atomic authority cutover

Shadow work never owns `vqro.collections`. The host importer builds a non-authoritative package-state overlay from current legacy policy. If legacy policy changes, the overlay is rebuilt or refreshed before the next accepted shadow result. It may be discarded after a crash and reconstructed; it is never a durable policy source or visible writer.

Session metadata gains `collections_authority.v1` with phase `legacy` or `package`, authority epoch, exact package/service/artifact/profile binding, state generation/revision, migration ID, and minimum snapshot reader version.

This record is authoritative. The package registry proves artifact availability, the persistence journal directs incomplete-write recovery, and runtime generations reject stale work. None independently grants authority.

After shadow parity, one full cutover replaces separate visible-document and policy phases:

1. acquire the persistence ownership lock and temporarily reject Collection mutations;
2. capture and preflight complete legacy policy plus current topology;
3. build candidate package state and invoke/audit the exact package against it, including the two action fixtures;
4. create candidate extension-state and new-version session images with one migration ID and `package` authority record;
5. stage and checksum both images, then publish a roll-forward journal before replacement;
6. replace and verify both images; after publication starts, failure is `recovery_required`;
7. in one App turn retire the bundled owner/writer, revoke queued legacy work, admit package document/actions, advance authority/catalog generations, invalidate caches, and publish only a result audited against the committed images;
8. remove the journal after durable and in-memory reconciliation.

Failure before journal publication leaves legacy authority unchanged. Startup finishes journal recovery before serving Collection APIs or reconstructing the provider catalog. There is no durable legacy-to-package mirror, visible read-only intermediate owner, bidirectional merge, or two-writer interval.

## Binary downgrade policy

Vqro binary downgrade after package cutover is unsupported; retained package-artifact rollback remains supported.

Cutover advances the session snapshot format/minimum reader version. A version-6-or-older server must reject the package-authoritative snapshot before mutation rather than read stale legacy policy. Package writes do not maintain a derived legacy copy. New Vqro versions continue to import supported older snapshots.

Before cutover, fixtures prove old-reader rejection, exact current restore, retained-package rollback, and journal recovery across every replacement boundary.

## Prior-client compatibility floor

The client floor remains endpoint generation 1 across Local, SSH, and Cloud. Binary downgrade is independent.

Cutover requires frozen generation-1 codec/method fixtures; new-server/current-client, new-server/oldest-maintained-generation-1-client, and current-client/baseline-server behavior; one smoke per transport rather than a Cartesian matrix; package available/unavailable/replaced/stale cases; old-snapshot import; new-snapshot restore; journal recovery; and old-reader rejection.

The baseline includes `endpoint-hello-v1.json`, `endpoint-welcome-v1.json`, `endpoint-snapshot-v1.json`, `endpoint-method-shapes-v1.json`, generation-1 bincode digests, `collections-document-v1.json`, and `collections-mutation-parity-v1.json`. New fixtures are additive; generation-1 expectations are not rewritten.

## Lean performance gate

Before R1, capture operation counts for one and 15 enabled services/scopes, one and 15 panes, and one versus three clients, plus one 100-service cold-capacity smoke. Do not run a full Cartesian failure matrix.

The baseline proves factories/Stores/Instances scale with enabled services rather than clients/frames/views; provider invocations are client-independent; no extension call occurs during frame composition; unrelated scopes are not invoked; client multiplication occurs only in cached fanout; and representative stale/cancel paths remain bounded. Detailed optimization benchmarks stay in R5.

## Required implementation sequence

### R0 — Thin contract and baseline freeze

Freeze the Collections profile/projections, two typed actions, opaque ticket, state-CAS plan, exact-Git catalog pointer, and lean operation baseline. Host-internal worker/journal machines remain implementation details of their owning stages.

### R1 — Production workers and read authority

Add bounded workers, host-call messages, exact bindings, revocable reads, replacement, shutdown, and recovery. Keep Collections in a non-publishing candidate slot.

### R2 — Rebuildable shadow

Use a non-authoritative imported state overlay to run package document/action parity, topology races, capacity, stale work, cancellation, restart, and failure tests. Legacy remains the only production owner/writer.

### R3 — Full atomic cutover

Production-wire the coordinator, pass shadow/readiness/client/old-reader/package-rollback gates, then atomically migrate state and transfer document, policy, namespace, and the two actions. Package failure disables Collections.

### R4 — Legacy deletion

After deterministic cutover/recovery/unavailable-package/remnant tests pass, remove the compiled provider, self-spawning manifest, mutable host policy, normal package-policy decoding, and degraded Collections projection. No release-long bake or downgrade mirror is required.

### R5 — Measure, then optimize independently

Compare against baseline, then independently consider lazy activation, safe epoch redesign, Engine sharing, and bounded compiled-component caching.

### R6 — SDK and authoring journey

Generate internal bindings after R0. Publish a stable SDK only after R3 production evidence. Installation continues to execute no repository build commands.

## Permitted host remnants after legacy deletion

The following may remain:

- neutral terminal/container topology and lifecycle code;
- generic state CAS, opaque-ticket validation, idempotency, and persistence machinery;
- reserved-topology orchestration for labeled create, without authority to choose the label;
- Collections profile validation and deterministic generation-1 projection;
- translation of frozen legacy archive/restore methods into typed package actions;
- read-only old-snapshot decoders and the frozen importer;
- authority-record, journal, and crash-recovery code;
- feature-neutral PTY recovery and generic terminal presentation.

The following must be removed or unreachable:

- the compiled Collections provider and self-spawning bundled manifest;
- mutable `CollectionCompatibilityPolicy` as authority/writer;
- normal-path decoding or projection of package policy values;
- host algorithms choosing/defaulting/reconciling/mutating label/archive policy after cutover;
- host-rendered degraded Collections output;
- automatic bundled fallback;
- any second namespace owner, shadow writer, or legacy persistence path.

A remnant is unacceptable if it answers label/archive policy without a current package document, accepts a policy write without a current package ticket, or publishes `vqro.collections` without package authority.

## Review-question disposition

1. Package unavailable disables Collections; only generic terminal access remains.
2. A frozen host importer performs shadow conversion and final migration.
3. Final migration is one two-image journaled roll-forward transaction.
4. Binary downgrade fails closed; package rollback remains supported.
5. R0 freezes forest/profile, endpoint mapping, revisions, action separation, and unavailable fallback.
6. Two actions use minimal dependencies in an opaque server-side ticket.
7. Inputs are typed/bounded; labeled create reuses `set_label` against a host reservation.
8. One readiness-gated transaction transfers namespace, document, policy, and actions; stale work is revoked.
9. Production is exact-Git opt-in; offline/default bundling is deferred and the conformance pin stays test-only.
10. Workers/persistence use bounded messages; the App actor never waits or joins.
11. Available archive semantics are frozen; unavailable mode disables all Collection methods without risking PTYs.
12. Endpoint generation 1 remains the client floor with representative transport tests.
13. A lean baseline precedes worker changes; exhaustive matrices are deferred.
14. `collections_authority.v1` is authoritative; registry, journal, and runtime generations have narrower roles.
15. Normal package-policy interpretation and degraded Collections are excluded from host remnants.

## Explicit deferrals

Offline/default package bundling, Vqro binary downgrade after package authority, staged visible read-only ownership, generalized action/effect systems, Spaces, Delegation, Agent Status, arbitrary embedded UI, remote catalog federation, dependency solving, and multiple language SDKs are deferred.
