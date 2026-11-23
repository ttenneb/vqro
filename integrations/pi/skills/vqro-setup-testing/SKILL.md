---
name: vqro-setup-testing
description: Finish a vqro setup change with current Unit evidence and an Integration receipt or explicit skip reason.
---

# Finish a vqro setup change

Changes to installed extensions, settings, bindings, themes, launchers or tests need evidence.
The terminal (tier1) Pi testing plugin runs Unit deterministically after observed setup changes; no AI call is needed.
RPC Pi bridges do not support this plugin; their launch notice must not be described as testing-hook coverage.

1. Use `vqro_setup_finish` before claiming a setup change finished. Attach its Unit receipt reference and verdict.
2. If Unit failed, fix it or ask the human to accept the specific current failure. Never self-approve an exception.
   Recorded completion evidence must enable installed tests; `--record --no-tests` is refused.
3. Attach a completed Integration receipt, or explicitly record why skipped with
   `vqro run test.setup.skip '{"suite":"integration","reason":"<specific reason>"}'` on the selected setup.
   Pending, failed, skipped, unverified and can't-tell results must be prominent, not hidden in a green summary.
4. Recommend Performance only for rendering, input, streams, publishing or startup changes; run it in downtime/background.
   It never blocks completion. Never launch paid/model calls to satisfy this policy.

`test.setup.status` reports the current setup identity/generation, evidence, and whether finishing is allowed.
The Unit requirement ships OFF until the human enables `doc/settings/testing.require_unit` after a clean setup check.
OFF is not a test pass. The tool may permit finishing while OFF; still disclose actual missing/failed coverage.
Coverage gaps are shown prominently but do not independently block finishing; never describe them as tested.
A lost response or checker error is NOT VERIFIED: inspect evidence; do not blindly replay a test command.
Integration commands return a future receipt path before completion; a filename alone is not a result.

This is cooperative policy, not a security boundary. The plugin cannot stop prose claims of “done,”
and it never requests another provider turn. Missing plugin/unsupported runtime/checker failure is not success.
Do not modify the separate inbox-pull plugin or its acknowledgements to implement this rule.
