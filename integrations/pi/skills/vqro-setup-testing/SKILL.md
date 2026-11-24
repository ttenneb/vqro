---
name: vqro-setup-testing
description: Apply the current vqro testing policy and attach honest setup-check evidence.
---

# Evidence for a vqro setup change

First inspect the current `test.setup.status` assessment (or use `vqro_setup_finish` to inspect it).
The rule ships OFF until the human enables `doc/settings/testing.require_unit` after a clean setup check.
This skill does not impose an always-on completion requirement.

## Current authoritative policy ON

- The `vqro_setup_finish` step is required before reporting a setup change finished, even when existing receipts are stale or NOT VERIFIED. Attach the current Unit receipt and verdict.
- A run-start policy label is only a sample. Follow the latest authoritative policy and the plugin's public policy-context updates if it changes during a tool step.
- If Unit failed, fix it or request the human's acceptance of the specific current failure. Never self-approve an exception.
- Attach completed Integration evidence, or record a specific skip explanation with
  `vqro run test.setup.skip '{"suite":"integration","reason":"<specific reason>"}'` on the selected setup.
- Recorded completion evidence enables installed tests; `--record --no-tests` is refused.

## Current policy OFF, or policy genuinely unknown/unavailable

Unit and Integration evidence are recommended, not unconditionally required by this skill.
Recommended reporting: attach available receipts and explain omitted/unavailable checks honestly. The finish tool is an optional way
to inspect the assessment, not a skill-imposed completion prerequisite in these states.
Refresh unknown policy before describing a result as verified; do not infer OFF from uncertainty.
A stale receipt does not make an authoritative ON policy optional. OFF and unavailable are not test passes.
The backend's actual finish assessment remains authoritative; this wording does not fabricate approval.

## In every state

- Show failed, missing, skipped, pending, unverified and can't-tell results prominently. A future receipt filename alone is not a completed result.
- Coverage gaps are disclosed but do not independently block finishing. Never describe missing coverage as tested.
- Performance is recommended only for rendering, input, streams, publishing or startup changes; use downtime/background. It never blocks completion.
- Do not launch paid/model calls or blindly replay a check after a lost response. Inspect existing evidence first.

The terminal (tier1) Pi plugin and a negotiated RPC launch can observe setup changes and run Unit without an AI call.
RPC support requires that launch's bridge-confirmed plugin/skill registration paths, not argv or environment alone.
Only that confirmed launch's scoped assessment can remove its RPC caveat; older, unsupported, unavailable or
unconfirmed launches stay NOT VERIFIED. Global/unscoped status retains the caveat and individual launch states:
NOT VERIFIED: rule not active for vqro-launched Pi agents yet.
Do not turn one confirmed launch into a global coverage claim. Registration is not loaded-byte or skill-body
attestation, and an unsupported notice is not testing-hook coverage.
This is cooperative policy, not a security boundary or a physical veto on prose claims of “done.”
Do not alter the separate inbox-pull plugin or its acknowledgements to implement this rule.
