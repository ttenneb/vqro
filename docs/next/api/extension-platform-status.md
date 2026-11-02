# Extension platform status

This document is the tracked capability map for Vqro's extension systems. It describes implementation and production authority; it is not a release schedule. Normative wire and data semantics remain in the individual contract documents and generated schemas.

## Terminology

Vqro currently supports two distinct extension models:

- **Legacy executable plugin** — a manifest-v1 plugin, hook, pane command, or protocol-2 service that runs as trusted same-user code. It may run repository build commands during legacy installation and is not sandboxed.
- **Component extension package** — a deterministic `vqro.package.v1` archive with a manifest-v2 or manifest-v3 WebAssembly Component Model service executed by Wasmtime with only the checked-in Vqro WIT world linked. Git-installed packages do not run repository build commands and cannot request the native process backend. Manifest v3 adds exact Service Fabric imports/exports without changing v2.

The Extensions inventory reports provenance rather than flattening these models: independently installed component archives are **sandboxed packages**, distribution-owned entries are **bundled providers**, and detected coding tools are **agent integrations**. Curated discovery never converts one kind into another or grants authority.

Use these lifecycle terms precisely:

1. **Artifact commit** — verified package bytes are retained in immutable storage.
2. **Registry commit** — the durable package registry points at the retained artifact.
3. **Runtime instantiation** — a declared service is compiled/spawned and completes negotiation.
4. **Capability lease** — the host grants one bounded invocation access to named host methods.
5. **Provider admission** — the service owns a capability-specific provider slot and generation.
6. **Visible publication** — a validated contribution enters the client-visible cache.
7. **Feature authority** — product reads and writes use the package as their policy source.

“Activation” alone is ambiguous and should not be used to claim all seven stages.

## Capability matrix

| Capability | Legacy executable plugin | Component extension package |
| --- | --- | --- |
| Manifest/package validation | Production | Production |
| Git installation | Production; may execute confirmed build commands | Production from an exact commit and deterministic `.vqrox`; executes no repository build commands |
| Runtime | Trusted process protocol 2 | Restricted Wasmtime component using `vqro.service.v1` |
| Install/update/rollback/remove lifecycle | Production compatibility lifecycle | Production exact-Git package journey; file/link contracts remain lower-level foundations |
| Runtime instantiation after enable/restart | Production | Production |
| Direct client/CLI generic action invocation | Experimental protocol-2 `action_invoke`, bounded existing workers and cooperative Linux/macOS caller observations; see [action invocation](extension-action-invoke-v1.md) | Unavailable; package-to-package calls remain Service Fabric |
| Task policy/store/CLI | Experimental Linux trusted Tasks executable; extension-owned records/debts, scoped receipts and cooperative-unverified human accept/answer; see [Tasks core](tasks-core-v0.md) | Unavailable |
| Read-only task pane/tree | Experimental Tasks terminal pane via existing plugin tab/split; extension-owned filters/debts, optional API-only exact-execution observation; [task pane](tasks-view-v0.md) | Unavailable |
| Host document v1 | Production for admitted protocol-2 providers; trusted non-reserved publishers may request terminal-group, activity or relationship snapshots (see [trusted semantic documents](trusted-semantic-documents-v1.md)) | Validated cached output for the exact pinned Collections and Spaces authority packages |
| `host.state.v1` lease | Not granted through the package dispatcher | Revocable read-only lease for the exact pinned Collections package during audit and typed action planning; no package write lease |
| `host.terminals.v1` lease | Not granted through the package dispatcher | Revocable exact-scope read lease for the exact pinned Collections package |
| Package document invocation and audit | Not part of the legacy surface | Bounded, fenced invocation and semantic audit for exact pinned Collections and Spaces packages |
| Document-v2 provider admission | Unavailable | Production for the exact pinned Collections package after atomic authority cutover |
| Document-v2 client-visible publication | Unavailable | Production through cached generation-1 compatibility projections for Collections |
| Collections document profile/projections | Unavailable | Package-authoritative after cutover; profile and both compatibility projections publish from one audited topology |
| Document-bound package actions/effect plans | Unavailable | Production typed `set_label` and `set_archived` plans with host tickets, validation, state CAS, and atomic checkpointing |
| Collections policy authority | Unavailable; the compiled provider was removed in R4 | Exact pinned package after snapshot-v7 atomic cutover; fail-closed if absent, unavailable, or replaced |
| Spaces projection authority | Unavailable; the compiled and bundled providers are removed | Exact pinned zero-host-call package; fail-closed if absent, unavailable, stale, or replaced |
| Exact service contract declarations | Unavailable | Experimental manifest v3 with canonical descriptor digests and information-flow disclosure; manifest v2 remains frozen |
| Durable package-to-package bindings | Unavailable | Experimental exact-artifact approval/revocation API in extension-state schema 2; no discovery-based authority or auto-rebinding |
| Package-to-package read call | Unavailable | Experimental host-started binding invocation plus the `extension.doctor` product seam, using a direct worker relay, required-import gating, no provider host calls, and no nested App-event completion; independently published Doctor/Health artifacts and production pins remain unavailable |
| Stable component author SDK | Unavailable | Unavailable |

Starting an ordinary component service proves verified runtime and lifecycle behavior but grants no host reads. The exact pinned Collections package is the sole direct host-read exception: before cutover its bounded workers receive revocable state/terminal reads only for quarantined audit; after the atomic snapshot-v7 cutover the same exact package supplies cached profiles and typed action plans. The exact pinned Spaces package receives no host-call capability; the host passes one complete `host.spaces.v1` value as invocation parameters. A manifest-v3 consumer may receive a separately approved, one-call Service Fabric lease, which conveys only bounded JSON to one exact provider worker and never transfers host capabilities. The host still executes effects, owns PTYs, validates package output and current authority, and coordinates durable publication.

The `availability` strings in the frozen `host-state-v1` and `host-terminals-v1` schema snapshots still say that “production package activation is not enabled.” Those exact R0 bytes remain pinned by the external package and predate read admission. They still correctly deny general package access and all direct write authority; this live status document records the exact-pin Collections exception and package authority.

## Collections authority today

There is no compiled Collections provider or automatic fallback. A restored pre-package snapshot exposes no Collections API or document; its read-only legacy label/archive fields exist only so the exact production package can perform the one-way import. Once every live scope completes package profile/action audit, the host persists package state plus snapshot-v7 authority through the two-image journal and publishes package document, namespace, policy, state, and typed-action authority in one App turn without a dual-writer interval.

After cutover, cached package profiles produce both `HostDocumentV1` and generation-1 `ClientShellCollection` output. Every create reserves an unpublished container identity, validates a provisional package profile and `set_label` declaration/plan (including `null` labels), checkpoints topology plus package state, and publishes only after durable success. Archive/restore use the package `set_archived` plan through the same state-CAS/checkpoint boundary. Generic writes to `vqro.collections` are rejected.

Restart recovery validates the authority/state fence before mutation. Package disablement, digest replacement, retirement, timeout, or stale completion clears package projections and rejects every `collection.*` method with `collections_authority_unavailable`; generic workspace, tab, pane, and PTY access remains available. Recovery requires the exact retained artifact or an explicit lifecycle rollback to it—there is no compiled fallback after cutover.

The [Collections v1 R0 contracts](vqro-collections-v1.md) freeze the profile, compatibility projections, typed actions, one-state-CAS plan, durable authority shape, snapshot reader floor, and operation baseline. R1–R3 define the audited transition; R4 removed the legacy authority and fallback paths.

See the accepted [Collections package authority transition](../architecture/collections-authority-transition.md) for the implementation boundaries.

## Spaces authority today

There is no compiled Spaces projection and no bundled Spaces catalog entry. The exact Git-pinned `vqro.spaces` package receives a complete host-authored `host.spaces.v1` snapshot and no host-call capabilities. Its action-free `host.document.v1` result is published only after exact artifact/runtime fencing and structural parity validation of generation, revision, repository membership, workspace membership, labels, ordering, roles, flags, and full snapshot coverage.

Changed snapshots supersede older attempts; stale or canceled completions cannot publish or disable a newer attempt. The last audited document remains visible during an ordinary replacement invocation, then publication swaps atomically. Package absence, disablement, runtime failure, digest mismatch, or invalid output removes Spaces document authority without changing generic workspace, tab, pane, or PTY access. Re-enabling or explicitly rolling back to the exact production pin rebuilds the projection from a fresh host snapshot.

## Compatibility boundaries

- Terminal admission now requires endpoint generation 2 (or private protocol 24);
  generation 1 and private 22/23 are retired by the accepted
  [terminal admission decision](../architecture/terminal-client-admission-v2.md).
  Unchanged named v1 codecs remain frozen; raw `vqro.api.v1`, extension artifacts,
  adapter contracts and exact pins are unchanged.
- `host.document.v2` remains action-free. Actions and effect plans require separately named contracts.
- `vqro.collections` and `vqro.spaces` have no bundled provider owner; only their exact production packages may publish them.
- Real PTYs remain host-owned and recoverable without any extension provider.
- Disabling or removing a package retains its namespaced state unless the user separately confirms data deletion.
- Native process containment is lifecycle containment, not an OS sandbox.

## Updating this document

Keep this matrix aligned with changes that cross a production boundary. A test-only fixture, schema, dormant implementation, successful preflight, or runtime handshake is insufficient evidence for a production status change. Production claims should cite an ordinary call path plus tests covering restart, revocation, stale completion, failure fallback, and relevant compatibility behavior.
