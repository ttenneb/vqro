# Extension platform status

This document is the tracked capability map for Vqro's extension systems. It describes implementation and production authority; it is not a release schedule. Normative wire and data semantics remain in the individual contract documents and generated schemas.

## Terminology

Vqro currently supports two distinct extension models:

- **Legacy executable plugin** — a manifest-v1 plugin, hook, pane command, or protocol-2 service that runs as trusted same-user code. It may run repository build commands during legacy installation and is not sandboxed.
- **Component extension package** — a deterministic `vqro.package.v1` archive with a manifest-v2 WebAssembly Component Model service executed by Wasmtime with only the checked-in Vqro WIT world linked. Git-installed packages do not run repository build commands and cannot request the native process backend.

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
| Host document v1 | Production for admitted protocol-2 providers | Not the component package contribution path |
| `host.state.v1` lease | Not granted through the package dispatcher | Revocable read-only lease for an exact pinned, non-publishing candidate; no candidate write lease |
| `host.terminals.v1` lease | Not granted through the package dispatcher | Revocable exact-scope read lease for an exact pinned, non-publishing candidate |
| `host.document.render.v2` invocation and audit | Not part of the legacy surface | Bounded worker invocation and quarantined semantic audit for the Collections candidate |
| Document-v2 provider admission | Unavailable | Candidate-only; no visible provider admission |
| Document-v2 client-visible publication | Unavailable | Unavailable |
| Collections document profile/projections | Unavailable | Rebuildable non-authoritative shadow profile and compatibility audit; never read by presentation paths |
| Document-bound package actions/effect plans | Unavailable | Shadow declarations and representative typed plans are audited; no production ticket issuer or commit path |
| Collections policy authority | Bundled host implementation | Unavailable; the external package remains non-authoritative |
| Stable component author SDK | Unavailable | Unavailable |

Starting an ordinary component service proves verified runtime and lifecycle behavior but grants no host reads. The exact pinned Collections candidate is the sole exception: bounded workers may receive revocable state/terminal read leases into a quarantined shadow audit. That exception does not admit a visible provider, issue a production action ticket, commit package state, or transfer feature authority.

The `availability` strings in the frozen `host-state-v1` and `host-terminals-v1` schema snapshots still say that “production package activation is not enabled.” Those exact R0 bytes remain pinned by the external package and predate candidate read admission. They still correctly deny general package access and all publication/write authority; this status document records the narrower R1–R2 candidate exception.

## Collections authority today

The checked-in `vqro.collections` manifest is a legacy bundled compatibility provider. It starts the current Vqro binary with `_extension-service collections`; the Collections policy implementation, compatibility API, persistence projection, and current client behavior remain host-owned.

The separately pinned external Collections component now runs in bounded per-service candidate workers after exact-Git installation. The host rebuilds a nonpersistent legacy-policy overlay, issues revocable read leases, and audits package profiles, synchronized compatibility projections, action declarations, and representative one-state-CAS plans. Candidate results remain quarantined from every presentation and persistence path.

The [Collections v1 R0 contracts](vqro-collections-v1.md) freeze the profile, compatibility projections, typed actions, one-state-CAS plan, durable authority shape, snapshot reader floor, and operation baseline. R1–R2 implement read admission and rebuildable shadow evidence only. No production authority transition is admitted.

R3 implementation is in progress. The binary can read and validate snapshot version 7, startup now recovers the journaled session/extension-state coordinator, and package-authority compatibility publication, fail-closed routing, and typed `set_archived` checkpoint orchestration are implemented behind the authority phase. Legacy-authoritative sessions continue writing version 6. Reserved labeled creation now uses an unpublished container identity, a package-rendered provisional profile and declaration, one typed `set_label` plan, and one two-image checkpoint before in-memory publication. The cutover admission gate remains hard-closed until the required Local/SSH/Cloud, restart, rollback, and race evidence is complete; therefore none of this R3 machinery currently transfers production authority.

See the accepted [Collections package authority transition](../architecture/collections-authority-transition.md) for the implementation boundaries.

## Compatibility boundaries

- Endpoint generation 1 and its named codecs remain frozen.
- `host.document.v2` remains action-free. Actions and effect plans require separately named contracts.
- `vqro.collections` cannot have simultaneous bundled and package provider owners.
- Real PTYs remain host-owned and recoverable without any extension provider.
- Disabling or removing a package retains its namespaced state unless the user separately confirms data deletion.
- Native process containment is lifecycle containment, not an OS sandbox.

## Updating this document

Keep this matrix aligned with changes that cross a production boundary. A test-only fixture, schema, dormant implementation, successful preflight, or runtime handshake is insufficient evidence for a production status change. Production claims should cite an ordinary call path plus tests covering restart, revocation, stale completion, failure fallback, and relevant compatibility behavior.
