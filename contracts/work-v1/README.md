# Work V1 contract candidate (Slice2A)

**Not frozen/activated until independent review and PM receipt. No implementation owner or2B starts before that gate.** This directory contains proposed new public contracts and inert fixtures, not a delivered actor broker/work engine. Existing generation1 codec/method fixtures and ordinary terminal input remain unchanged.

## Artifacts / checks

- `work.schema.json`: extension-owned actor/task/thread, mutation/query, immutable entry and work receipt/non-record lookup branches.
- `broker.schema.json`: neutral invoke, journal/deferred intent, opaque mailbox/claim; no task fields.
- `api-shapes.mjs` → generated `api.schema.json`: all proposed neutral methods, closed parameters/results/errors, capability and byte-limit metadata. Regenerate with `node contracts/work-v1/generate-api-schema.mjs`.
- `decision.schema.json`: public async six-point request/result ABI. Context is work-built/host-fenced, never trusted client data.
- `decision-defaults.mjs`: no-model defaults and inert shape/current-result/hard-policy application plans. Not an action committer.
- `canonical.mjs` + Python fixture: integer-JSON canonical digest/freeze; duplicate-key test uses Python stdlib, not a production wire decoder.
- `lookup-reference.mjs`: verified absent fence, no fabricated unknown attribution; absence alone never substitutes current permissions/preconditions.
- `output-route-reference.mjs`: human public route consent/CAS and exact sole-run binding; arguments marked verified are assumed fixtures, not implemented authentication.

`just work-contract-test` tests schema parsing/internal refs/generated parity, canonical cross-language cases and pure semantic fixtures. It does **not** run a full Draft2020-12 validator, authenticate a caller, fsync a store, launch Pi, prove final persisted reply or establish exactly-once model execution.

Normative rationale/authority/quota/failure matrices: `docs/next/api/v1-work-contract.md`, `v1-neutral-mechanics.md`, `v1-decision-contract.md`.

## Protected behavior and later implementation gates

| Protected behavior | Contract guard / mandatory production characterization |
| --- | --- |
| Generation1 connections/codecs/method meanings | Add separately advertised JSON methods only; existing frozen fixtures/digests/enums untouched; missing methods feature-local |
| Neutral agent/terminal facts and typing | No bundled presentation dependency or pane-input action fallback; direct terminal input unchanged |
| Same-user trusted plugins, no first-party privilege | Ordinary non-reserved service grants/action invocation/recovery leases; prove normal publisher not pinned-package route |
| Actor authority and exact execution | Peer UID/PID/birth/ancestor platform proof, operator challenge and ticket-private transfer; wrong origin/replay/reparent/replacement/restart deny/hold |
| Task/thread authority | Only work extension owns graph/transitions; host opaque journal/CAS; adversarial missing/duplicate/cyclic/foreign refs and forged actor fields |
| Durable operation and source provenance | Append audit/delta/receipt/deferred stamp same fsynced CAS; host attribution + known receipt domain; callback alone not commitment |
| Post-crash outbox | Separate current recovery-service lease, immutable original intent; ACK-loss lookup, no payload/sender substitution; independent transport pin |
| Receipt absence and retention | Integrity/current-domain absent branch has key/domain only; unknown/retired/repair never replay; entire-generation tombstones |
| Claim/run/edits | Stamped payload immutable; exact active recipient version CAS; one logical run per delivery/version; uncertain start never retry automatically |
| Final reply/human route | Notification-only settlement + exact persisted successful output; public human route before settlement, source/run/epoch/revision/cancel races; explicit tool dedupe |
| Scope/decisions | Hard actor/subtree rights independent of provider auto_apply; suggest-only; same-CAS audit, timeout/revoke/cancel/override fences |
| Quota/reset | Charged metadata/pins/receipts, no implicit expiry; export/hash/fsync assertion + explicit destructive consent; interrupted retirement repair barrier |
| Client rendering/drafts | Modal patterns, drafts local, public submissions; no I/O/process tree/model on render; fixed1/15 populated-pane profile if widening layout |
| Real installation/auth/models | Separate Slice4 human approval; private no-model roots first; preserve canonical/dogfood sessions and credentials |

2B tests must use real production admission/storage/public API in private roots.3 tests must qualify work/defaults/Pi/modal behavior and native event ordering. This candidate does not substitute for those tests or authorize real cutover.
