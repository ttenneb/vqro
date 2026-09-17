# `host.document.v2`

`host.document.v2` is the canonical, feature-neutral producer document contract. This release slice publishes only the DTO, strict validator, schema, fixtures, and a generic process-local audited cache. It does **not** enable provider discovery or invocation, host calls, endpoints, client rendering, actions, effects, or production activation. Existing `host.document.v1` and recovery behavior are unchanged.

The structural JSON Schema is [`host-document-v2.schema.json`](host-document-v2.schema.json). The separate strict, host-authored off-render-loop request DTO is documented as [`host.document.render.v2`](host-document-render-v2.md). Golden examples are in [`host-document-v2.fixtures/`](host-document-v2.fixtures/). The schema rejects structurally invalid JSON and expresses field forms, integer ranges, per-field limits, and array limits. The Rust validator is normative for constraints JSON Schema cannot express here: encoded and aggregate text budgets, strictly sorted dependencies, graph reachability/acyclicity/single-parent ownership/depth, terminal uniqueness and caller-authorized scope coverage.

## Data model

A document identifies its exact package/service producer and binds the artifact digest, runtime generation, provider generation, and scope generation. Its named scope has a contract and opaque scope ID. Its sorted, unique dependency vector binds each named contract and scope to a nonzero revision and generation. A nonzero document revision identifies the producer result under that fence.

Content is an ordered forest made only from:

- `group`: ordered child node IDs;
- `text`: semantic text;
- `terminal_slot`: one stable `TerminalId` string.

`roots` orders the trees and each group orders its children. An empty forest (`roots` and `nodes` both empty) is valid. Every nonempty node has an opaque document-local ID and an explicit accessibility name, with an optional accessibility description. Terminal slots never carry pane, workspace, tab, layout, or geometry identity.

The contract has no styling, geometry, input, action, effect, capability, or host-call declarations. The initial action/effect budget is exactly zero: those fields are absent from the DTO and rejected as unknown.

## Validation and budgets

Validation denies unknown fields and requires the exact `host.document.v2` tag. Producer JSON is limited to 128 KiB encoded. A document may contain at most 512 nodes and 512 roots, each group may contain at most 512 child references, forest depth is at most 16, and the dependency vector is limited to 64 entries. Per-field limits are 4,096 text characters, 256 accessibility-name characters, and 1,024 accessibility-description characters; all text and accessibility strings together are limited to 65,536 characters. Control characters, including terminal escapes, are rejected.

Node IDs are unique. Every root and child reference must resolve, every node must be reachable exactly once, and cycles are rejected. Terminal slots must be unique and resolve against the immutable `TerminalId` set supplied by the caller. Callers choose either subset validation or exact scope coverage.

## Quarantined audit cache

The generic audit cache is isolated server/runtime contract infrastructure, not activated runtime or render-owned state. Entries are keyed by producer plus named scope and are never persisted. Acceptance succeeds atomically only while the producer fence and complete dependency vector captured by the audit ticket still equal current authority. Tickets are move-only and one-shot: a current attempt is consumed before serialization, capacity, or sequence checks, so rejected attempts cannot replay. A newer attempt fences late success and failure.

The default registry is bounded to 64 authority records, 64 cached entries, and 4 MiB of encoded values. New authority is rejected at capacity; authority is never implicitly evicted. Producer revocation or scope retirement explicitly frees authority and cached entries. Cached-value eviction is deterministic oldest-accepted-first, with the key as a tie-breaker. Encoded size is measured through a capped counting writer without allocating a payload-sized staging buffer. The normative terminal dependency convention uses contract `host.terminals.v1`, scope ID `<tab_id>/<full lowercase fingerprint_sha256>`, revision `1`, and generation `snapshot.lease_fence.provider_generation`. The helper rejects a noncanonical tab ID, a digest that is not exactly 64 lowercase hexadecimal characters, a scope over the identifier bound, or a zero generation. The render request cannot carry this dependency; audit code derives it only from a successful trusted host snapshot observation.

Production runtime wiring and producer invocation remain follow-up work. Test-only composite process and component harnesses record trusted terminal observations for parity, but do not activate the document contract or claim atomic audit acceptance.
