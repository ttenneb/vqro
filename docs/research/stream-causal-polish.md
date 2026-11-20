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

## Complete opener context, without whole-request flattening

The native wire forwards actual author, actor, effective principal, original initiator, diagnostic label and
explicit cause/correlation for both terminal and item channels. Rust OpenRequest and Python terminal Stream
expose that diagnostic context; missing legacy values stay unknown. Neither the authority proof nor an inferred
cause/current boot is exposed. A real delegated attach red on the old wire captured principal bob but omitted
all other fields; corrected terminal AND item channel regressions pass with workspace history unchanged.

Both SDKs validate known context before stream/effect id or handler admission. Rust projects only known scalar
fields and bounded causal metadata, rather than serde-cloning/flatten-buffering the whole opaque params/args.
Diagnostic label validation is shared with the core, with legacy null wire labels kept absent. Existing separate
stream/effect admission budgets/native channel support and errors are preserved. Rust SDK13/context2/causal7
focused tests pass; Python context2 +existing causal6 pass; original meaningful reds remain retained.

## Shared vectors and the measured numeric-codec edge

29 shared data vectors cover byte4095/4096/4097, container depth64/65, UTF8, finite max/subnormal/negative zero,
nonfinite overflow and lone surrogate rejection, prior-qualified links and format controls only in known strings.
The initial uniform-result fixture exposed two REAL differences: for1e-5, Python4096bytes accepts while
serde_json4098 rejects; for1e-6, Python4097 rejects while serde_json4096 accepts. Original fixture and red log
are retained (causal-vectors-common-red.json,stream-causal-rust-first.log), not disguised as a pass.

Coordinator explicitly chose the existing core serde_json bound as authoritative, SDK pre-checks advisory near
numeric representation edges. Only those two labelled vectors have per-codec measured sizes/results;27others
keep identical expected outcomes. Both suites pass29cases now. No coercion/normalization, bespoke numeric
formatter, weakened threshold, inferred provenance or changed payload. Either side refuses explicitly. The
short protocol paragraph records this interoperability limitation rather than promising identical admission.

This note does not claim a whole candidate gate or independent acceptance yet.
