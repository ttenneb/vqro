# Stream context and causal boundary polish (2026-11-20)

After the three explicit identity review P3 commits (assertable recorded tags, Cf principal ids, restart docs),
this group finishes the existing stream/cause and allocation follow-ups. No new permission, proof, inference,
delivery guarantee, deduplication or replay mechanism.

## Validate before lifting

`Causality::lift_args` borrows nested cause/correlation, uses the same validation as root parsing, and clones only
bounded validated fields. Equal duplicate/conflict rules and opaque business args stay unchanged. The new stage
unit fails on the old implementation returning Ok for a900k-byte nested field; the corrected implementation
returns invalid before inserting it into root params. Depth65 is refused too. Existing encoded_bytes is a counting
writer (no serialized allocation); no new allocator framework or bespoke JSON formatter. Stage red/green and
all existing causality/SDK focused results are retained under prim-slice1-context.

This note does not claim a whole candidate gate or independent acceptance yet.
