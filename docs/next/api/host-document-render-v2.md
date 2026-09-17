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
