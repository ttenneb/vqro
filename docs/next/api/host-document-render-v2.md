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

The M3 structural-shadow host gate is deliberately `cfg(test)` only. `just collections-package-conformance` requires a clean checkout of `ttenneb/vqro-extension-collections` at `32b489994443e4bca96ecd844769344d225d3bc4`, builds and verifies its deterministic package, and checks the hard-pinned package (`42745bb5…c634`), manifest (`73c6accc…9295`), component (`44e48062…49e0`), and every digest in its committed `contracts/PROVENANCE.md`. The actual packaged component receives its one terminal snapshot through the staged App handler, returns a canonical document, and the host-owned coordinator serializes begin/final acceptance through mutable `App` turns into a bounded, process-local quarantine. This activates no catalog provider and exposes no output to rendering, clients, endpoints, or persistence. Process/component equivalence continues to use the existing shared host-call protocol parity fixtures; this structural-shadow milestone does not claim a new generic document producer for either runtime.
