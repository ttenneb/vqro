# Generic explicit causality (2026-11-19)

Separate from C2 connection identity. Agent-v0 §2.5/§9 item4 requires generic effect metadata, not an agent policy
or a new event store. Reuse the existing request attribution carrier, serialized commit lanes and native effect
context. Links are data; they confer no authority and do not change native delivery/outcome claims.

## Wire and bounds

`resource.invoke` accepts explicit `cause` / `correlation` in root params or common op args. Equal duplicate
locations are allowed; disagreement is invalid before dispatch. Args remain unchanged. Root metadata is also
recorded by workspace apply/revert and operational register/apply, using their existing audits/events. Prepare
revalidation retains explicit links, but a callback proof does not invent or automatically forward a cause.

Cause is an object: required non-empty kind/id/provenance strings, optional endpoint/epoch/boot strings. Entity
and request references require endpoint+epoch; effect references require boot; delivery ids are global. These are
structural checks only, never a resource/epoch/boot existence or ownership lookup. Prior epochs/boots remain
unchanged. Unknown kind/provenance values and extra bounded JSON fields survive. Known strings are 1..256 UTF-8
bytes, correlation 1..256 UTF-8 bytes, encoded cause at most 4096 bytes and container depth at most 64. Per the main
spec pin, invalid shapes/missing qualifiers/oversize causal fields are `invalid`; outer wire admission is separate.

Absence stays absent/unknown. Core never fills a current boot, delivery id, parent effect or provenance from
text/timing, and never uses links as effect ids, deduplication keys or replay instructions. Native effects keep
their own boot-scoped ids and D10 outcomes. Declared provenance is passed exactly, not upgraded or certified.
Adapters may label only links their native evidence supports.

## Implementation seam

- `src/causality.rs`: bounded DTO/parser, explicit args lifting/conflict check, optional annotation. No policy.
- `principal::Attribution`: flattened optional fields survive existing candidate-first persistence and proof
  revalidation; private authority remains ephemeral and unrelated to the caller's reference data.
- `server::effect` / provider wire: unchanged fields go to `effect.invoke` and the existing effect audit event.
  Cached forget uses its existing audit; nothing becomes a native-delivery assertion.
- `Core::emit_txn` / `finish_operational`: links join the existing event. Workspace history persists them;
  operational audits remain separate and never pollute global undo.
- Rust SDK validates before effect-id admission/native dispatch and exposes cause()/correlation() plus the DTO;
  Python SDK validates before handler dispatch and exposes Effect.cause/correlation. No extra dependency/module
  is required by the standalone Python SDK. Ordinary client parameter dictionaries already pass fields unchanged.

## Evidence

- `tests/causality.rs`: the exact C9 trace failed on 44d8ce8 after the native receipt echoed its input, because
  generic core audit cause was Null. The unchanged regression is green. A second real Rust SDK/native ledger
  verifies root-only and matching duplicate input, unchanged args, two prior boots with the same local id,
  no inferred fields when absent, and no native dispatch on conflicting/unqualified/invalid metadata.
- `tests/causality_bounds.rs`: open enums/extra fields, required qualification, exact 4096/4097 encoded bytes,
  exact 64/65 container depth and 256/258 UTF-8 bytes; workspace event/history/restart and rejected-write atomicity.
- `tests/principal_operational.rs`: a genuinely supervised callback revalidates its proof and keeps explicit
  registration/apply links in its operational audit/event while global history and opaque-proof privacy
  remain intact.
- `tests/test_causality.py`: paired shape bounds and an actual SDK handler ledger: invalid metadata never calls
  the handler or consumes its effect id; absence remains unknown. The full Python suite passes 316 cases.
- Independent `spike-c/k-z25l153p/results.json`: C9 passes on the exact experimental core image (per-run
  `core_image_sha256` 42446b30...), with unchanged fixed A1 adapter 1cc9247. Native receipt+audit, actual core restart
  with distinct boots and reused asserted local effect id, and request identities from two private endpoint epochs
  are exercised. This is C9-only, not a full-gate or whole-adapter qualification claim.

Experimental snapshot `experimental-6f1f168` is immutable and explicitly not full-gate accepted: base+complete
patch+binary hashes. Its positive behavior passed 6 Rust and 3 Python cases. Before canonical acceptance, causal
byte/depth refusal kinds were normalized from too_large to the spec-pinned invalid; no valid-link behavior
changed. Independent K1 qualified-reference checks and the canonical full gate are reported separately.

No install, merge, push, paid model call or real backend/session mutation is part of this work.
