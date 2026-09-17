# `host.document.render.v2`

Status: public, generic request contract for staged audit/conformance work only. Production provider discovery, activation, endpoints, clients, persistence, and rendering are not enabled.

The host creates every request and sends it only off the render loop. A request contains exactly:

- the contract tag `host.document.render.v2`;
- the complete host-authored package/service/artifact/runtime/provider/scope producer fence;
- the named output scope; and
- a nonzero requested document revision.

The strict DTO denies unknown fields and is limited to 2 KiB encoded. It cannot carry authorized terminal IDs, dependency claims, actions, effects, capabilities, presentation state, or geometry. In particular, a producer cannot use request parameters to authorize its own terminal references or declare which terminal snapshot the host should trust.

A successful result is expected to be a strict [`host.document.v2`](host-document-v2.md) value. The request schema does not activate invocation or acceptance. Audit code must independently observe successful trusted host reads, validate the returned document with exact terminal coverage, compare the complete producer fence, output scope, requested revision, and dependencies, then revalidate authority before one-shot cache acceptance.

The generated structural schema is [`host-document-render-v2.schema.json`](host-document-render-v2.schema.json). Rust validation remains normative for the encoded-size bound and semantic field validation.

Audit topology semantics are value-current, not mutation-history fencing: A→B is rejected when B is current at finalization, while an exact canonical A→B→A may pass. Full opaque `TabId` identity still rejects tab replacement ABA. Pending audits are globally bounded to the quarantine authority capacity and only one attempt may remain pending per producer/scope key; replacement explicitly fails the older ticket.

Both runtime transports enforce the method-specific 128 KiB response limit before allocating a JSON result: components bound guest response bytes before canonical decoding, and process services bound the complete response frame before envelope deserialization. The normal runtime frame ceiling remains an independent upper bound.

The M3 semantic-shadow host gate is deliberately `cfg(test)` only. `just collections-package-conformance` requires a clean checkout of `ttenneb/vqro-extension-collections` at exact candidate commit `b590657b23866e36b07e3bbbaed5554e1be5a4fc`, builds and verifies its deterministic package, and checks the hard-pinned package (`36230691…2980`), manifest (`4d270e26…034`), component (`819963c8…1bc`), and all 13 committed contract/provenance digests. The actual packaged component reads seeded `vqro.collections` policy state and mixed terminal topology through exactly one `state.snapshot` followed by one `terminals.snapshot`; the latter uses separate App turns for capture and final validation. Its canonical document must carry both exact dependencies and the expected label/archive semantics before the existing sole host-owned composite audit coordinator can finalize it into a bounded, process-local quarantine. Real-component actor-boundary races, authority revocation, and a persistence block consume their tickets without caching. This activates no catalog provider and mutates no legacy state, render path, client/API surface, endpoint, or persistence authority. Process/component equivalence continues to use the existing shared host-call protocol parity fixtures; this semantic-shadow milestone does not claim a new generic document producer for either runtime.
