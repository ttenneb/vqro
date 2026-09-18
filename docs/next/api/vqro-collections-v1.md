# Vqro Collections extension contracts v1

Status: **R0 contract and fixture baseline; not production-enabled**.

These Collections-specific contracts sit beside `host.document.v2`. They do not add actions,
effects, or feature authority to that generic contract. The host remains authoritative for live
terminal topology, durable PTYs, provider admission, tickets, compare-and-swap commits, and the
atomic authority cutover described in
[`../architecture/collections-authority-transition.md`](../architecture/collections-authority-transition.md).

## Frozen names

| Purpose | Contract | Proposed service method |
| --- | --- | --- |
| Collections profile | `vqro.collections.document-profile.v1` | `vqro.collections.document-profile.render.v1` |
| Typed declarations | `vqro.collections.actions.v1` | `vqro.collections.actions.list.v1` |
| Typed invocation | `vqro.collections.action-invocation.v1` | `vqro.collections.action.plan.v1` request |
| One-state-CAS plan | `vqro.collections.effect-plan.v1` | `vqro.collections.action.plan.v1` response |

R0 freezes the DTOs, validators, schemas, and fixtures. It does **not** register these methods,
grant read leases, publish a provider, or change any `collection.*` endpoint.

## Production pin and durable authority

The production catalog record uses `vqro.collections.production-pin.v1`. It binds the fixed package
and service IDs, credential-free HTTPS source, exact lowercase 40-character commit, repository-
relative `.vqrox` path, package/manifest/component SHA-256 digests, component world, exact declared
capabilities, and profile contract. It accepts exactly `host.state.read` and
`host.terminals.read`: the component proposes a state plan but never receives a direct state-write
capability. Schema:
[`vqro-collections-production-pin-v1.schema.json`](vqro-collections-production-pin-v1.schema.json).

The existing `extensions/index-v1.json` Collections entry remains a bundled compatibility/
conformance pin. The separate dormant production record is
[`extensions/first-party/collections/production-pin-v1.json`](../../../extensions/first-party/collections/production-pin-v1.json):
it binds public commit `f3f4d2f8ce1dce8c319bf4c4d0e7224850afa868`, archive path
`dist/vqro-collections.vqrox`, and the reviewed package/manifest/component digests. The record does
not install, enable, admit, publish, or grant authority to that package. R1 must consume it through
the explicit digest-confirmed exact-Git journey; a branch name, locally rebuilt artifact, or
repository build instruction remains invalid.

Session snapshot version **7** introduces `collections_authority.v1`; its minimum reader version is
also **7**. The `legacy` phase has no package/state/migration fields. The `package` phase requires an
authority epoch, exact package/service/package/component/profile binding, package-state generation
and namespace revision, and migration ID. Version-6 readers must reject a version-7 snapshot before
mutation. Schema: [`collections-authority-v1.schema.json`](collections-authority-v1.schema.json).
The package-authority and old-reader fixtures freeze this boundary without changing the current
version-6 writer during R0.

## Document profile

`vqro.collections.document-profile.v1` contains an unchanged `host.document.v2` document plus an
ordered `collections` sidecar. The sidecar preserves the distinction between an absent label and an
explicit label for generation-1 clients; it does not carry terminal topology.

The normative validator checks the profile against one host-authored tab topology snapshot:

- the producer package is `vqro.collections`, the scope is `host.tab.v1`, and the scope ID is the
  host tab ID;
- there is exactly one root per live neutral container, in host order;
- root IDs are `collection:<container_id>` and sidecar entries have the same order;
- a nonempty sidecar label equals the root accessible name; `null` or an empty label uses the
  canonical effective root name `Collection` while preserving the exact sidecar value;
- every container terminal appears exactly once as `terminal:<terminal_id>`;
- active terminal slots are direct root children and preserve neutral relative order;
- an optional final `archive:<container_id>` group contains only archived terminal slots, also in
  neutral relative order;
- text nodes may be direct children before the archive group; no other nested groups are valid;
- terminals outside the neutral container topology are invalid.

The host derives both compatibility outputs from the same validated profile and topology snapshot:

1. `HostDocumentV1` under namespace `vqro.collections`, preserving hierarchy, labels, terminal
   references, selected state, and producer/document revision. Root/nested groups map to
   `group`/`section`; text content maps to a `status` label; terminal accessible names map to
   `terminal` labels; accessible descriptions map to details; and legacy action/capability arrays
   remain empty; and
2. generation-1 `ClientShellCollection` records, preserving Collection identity, container order,
   member order, selection, exact optional label, and per-member archive flags.

The two outputs are published with one shell projection revision. The render loop reads only the
cached projection; it never calls the package.

Schema: [`vqro-collections-document-profile-v1.schema.json`](vqro-collections-document-profile-v1.schema.json)

## Typed actions

`vqro.collections.actions.v1` declares only:

- `set_label`, whose subject is a Collection root; and
- `set_archived`, whose subject is a terminal slot.

Action IDs and `(kind, subject)` pairs are unique. Declarations are bound to one profile document
revision and have their own nonzero revision. No generic action, command, effect, or confirmation
framework is introduced.

Schema: [`vqro-collections-actions-v1.schema.json`](vqro-collections-actions-v1.schema.json)

## Invocation tickets and dependencies

The host issues an opaque, one-shot ticket only after validating the cached declaration and current
server authority. `vqro.collections.action-invocation.v1` carries that ticket, an idempotency key,
the declaration revisions, the expected package-state CAS versions, one typed input, and the
minimum host dependency for that action:

- `set_label` carries only a `container` dependency. Before commit the host checks that the reserved
  or published container still exists. It does not bind an unrelated terminal fingerprint.
- `set_archived` carries a `terminal_membership` dependency with container ID, terminal ID, terminal
  snapshot fingerprint, and provider generation. Before commit the host requires exact current
  membership and topology identity.

Labels are unnormalized UTF-8, may be empty, are at most 4096 encoded bytes, and may not contain
Unicode control characters. `null` clears an explicit label.

Schema: [`vqro-collections-action-invocation-v1.schema.json`](vqro-collections-action-invocation-v1.schema.json)

## Effect plan

A successful package invocation returns `vqro.collections.effect-plan.v1`. The plan may contain
exactly one opaque set-or-delete operation for the invocation's container key in the ticket-bound
`vqro.collections` namespace. It must echo the ticket, idempotency key, expected store generation,
and expected namespace revision. The host rejects a mismatch before persistence.

The host does not interpret a general effect list and does not grant terminal mutation. A valid plan
only authorizes one package-state CAS. The server still owns the final two-image transaction that
persists session topology and extension state together where an operation requires both.

Schema: [`vqro-collections-effect-plan-v1.schema.json`](vqro-collections-effect-plan-v1.schema.json)

## Cancellation, replay, and creation

- Cancellation before commit revokes the ticket and commits neither image.
- A consumed or revoked ticket cannot authorize another plan, even with the same idempotency key.
- Retrying the same completed idempotency key returns the recorded outcome and performs no second
  state transaction.
- CAS or dependency failure consumes the attempt; callers must obtain a fresh snapshot, declaration,
  and ticket.
- Labeled `collection.create` reserves unpublished topology, obtains a provisional profile and
  `set_label` declaration for that reserved container, invokes the package, then atomically commits
  the session image and package-state image. Failure publishes neither image and the reservation is
  discarded.

These are server-side authority rules. A package response alone never proves ticket validity or
commits state.

## Fixtures

[`vqro-collections-v1.fixtures/`](vqro-collections-v1.fixtures/) contains the frozen valid profile,
action declarations, both typed invocations, one effect plan, the package-authority record, and the
version-6-reader rejection vector. The Rust conformance tests also
exercise invalid topology, mismatched action dependencies, and ticket replay fences. R1 must use
these same bytes or recorded digests in the external package before production read leases are
enabled.

JSON Schema is structural. The Rust validators are normative for byte budgets, canonical IDs,
topology/profile correspondence, graph shape, action-subject matching, action-specific dependency
matching, replay fields, and state-value encoded size.
