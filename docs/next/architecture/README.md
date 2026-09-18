# Architecture decisions

This directory holds tracked cross-worktree architecture proposals and accepted decisions for unreleased Vqro work.

Each document must declare one status:

- **proposed** — under owner review; not yet an implementation mandate;
- **accepted** — the durable implementation boundary;
- **superseded** — retained for history with a link to its replacement;
- **withdrawn** — considered but not adopted.

Architecture documents describe stable ownership, invariants, compatibility obligations, and acceptance or removal gates. They do not contain temporary owners, pane IDs, schedules, branch-local test receipts, or active-task bookkeeping; those remain in ignored `.local/prd/` ledgers.

Normative wire/data semantics remain under `docs/next/api/`. Public user guidance remains under `docs/next/website/`. A decision that changes either must update those sources in the same implementation change that crosses the boundary.
