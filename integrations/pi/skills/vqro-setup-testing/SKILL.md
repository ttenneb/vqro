---
name: vqro-setup-testing
description: Apply the current vqro testing policy and attach honest setup-check evidence.
---

# Evidence for a vqro setup change

First inspect the current `test.setup.status` assessment (or use `vqro_setup_finish` to inspect it).
The rule ships OFF until the human enables `doc/settings/testing.require_unit` after a clean setup check.
This skill does not impose an always-on completion requirement.

## Current policy ON, with a fresh assessment

- The `vqro_setup_finish` step is required before reporting a setup change finished. Attach the current Unit receipt and verdict.
- If Unit failed, fix it or request the human's acceptance of the specific current failure. Never self-approve an exception.
- Attach completed Integration evidence, or record a specific skip explanation with
  `vqro run test.setup.skip '{"suite":"integration","reason":"<specific reason>"}'` on the selected setup.
- Recorded completion evidence enables installed tests; `--record --no-tests` is refused.

## Policy OFF, or assessment unknown, stale or unavailable

Unit and Integration evidence are recommended, not unconditionally required by this skill.
Recommended reporting: attach available receipts and explain omitted/unavailable checks honestly. The finish tool is an optional way
to inspect the assessment, not a skill-imposed completion prerequisite in these states.
Refresh unknown/stale status before describing a result as verified; OFF and unavailable are not test passes.
The backend's actual finish assessment remains authoritative; this wording does not fabricate approval.

## In every state

- Show failed, missing, skipped, pending, unverified and can't-tell results prominently. A future receipt filename alone is not a completed result.
- Coverage gaps are disclosed but do not independently block finishing. Never describe missing coverage as tested.
- Performance is recommended only for rendering, input, streams, publishing or startup changes; use downtime/background. It never blocks completion.
- Do not launch paid/model calls or blindly replay a check after a lost response. Inspect existing evidence first.

The terminal (tier1) Pi plugin observes setup changes and can run Unit deterministically without an AI call.
RPC Pi bridges do not support this first slice; RPC support is a required follow-up, not a dropped requirement.
While an RPC Pi launcher is enabled, show: NOT VERIFIED: rule not active for vqro-launched Pi agents yet.
An unsupported notice is not testing-hook coverage.
This is cooperative policy, not a security boundary or a physical veto on prose claims of “done.”
Do not alter the separate inbox-pull plugin or its acknowledgements to implement this rule.
