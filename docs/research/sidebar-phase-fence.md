# Sidebar load gate phase fence (2026-11-17)

Fix `453fe23` on `sidebar-phase-fence`, based on main `518217d`; parent has already integrated it as `8618048`.
Only the measurement fixture/tests changed, not product behavior or the zero-off-page requirement.

## Cause

The old readiness predicate accepted any nonempty sidebar once a different connection saw 800 resources and
200 agent panels. Neither that snapshot nor `observer.stats()` acknowledged the sidebar extension's coalesced
startup work or the observer's notification delivery. Private traces show partial 975/1038-logical-row sidebar
frames followed by the complete 1151-row frame. A late final startup frame could therefore be charged to the
first off-page phase. A deterministic delayed-delivery regression reproduces the old one-frame misattribution.

A fixed private run also observed an actual 30,136-byte natural startup publication after its initial stats
sample, before the explicit fence. It is now recorded as startup, not excused as an off-page change. The byte
size differs from the parent's 29,560-byte frame because private workspace paths are embedded in the content.
No product off-page invalidation was identified.

## Fix and safeguards

The fixture changes one visible workspace label via public `view.publish`, waits for the observer to see that
derived sidebar frame, restores the label, and waits for the later restoration sequence before opening the
measurement window. This acknowledges both derivation and ordered observer delivery, rather than sleeping
longer or resetting an arbitrary counter. The initial fenced page must contain all 1151 logical rows.

Every phase records settling separately. Only the exact two marker-only frames may be excluded. A real detail
change coalesced with a marker is still counted (its entire wire frame, conservatively); trailing natural
publications are charged to the preceding phase. Off-page publications/bytes must still be zero, and the
restored visible content must equal the phase's starting content. Four deterministic regressions cover late
partial startup, marker-only exclusion, coalesced real changes and previous-phase trailing frames.

Receipts include startup/settling marker and restoration sequences, natural publication/byte counts and whether
marker/restoration frames also carried real detail. Phase `seconds` covers workload plus sample drain;
`settling.seconds` is additional and explicitly recorded. No natural traffic is discarded across boundaries.

## Verification

- Six private 600-terminal/200-agent/350-workspace runs passed: five at the default five-second duration per
  phase and one at two seconds. All had zero off-page publications/bytes and zero extension crashes.
- `scripts/check.sh full` at `453fe23`: **22/23 steps passed**, including the sidebar scale gate, zero-warning
  build, **162 Python tests**, all provider/TUI/fake-Pi/history/scale suites and scoped no leftover processes.
- The sole failure was the already-known main integration test
  `commit_admission_counts_escaped_author_bytes_not_only_raw_text` (timeout vs overloaded). Its targeted rerun
  reproduced it. Parent confirmed the independent test-only fix `5a9bbbf`, requested no further rerun here,
  integrated this sidebar fix, and owns the combined main gate. No unrelated admission code/test was edited.

Evidence: `~/.local/share/herdr/scratch/prim-agent-ux/sidebar-fence/` (`green-1.json` records the actual pending
startup frame; `green-{2..6}.json`, `unit.log`, `full-gate.log`, `admission-rerun.log`, `check-receipt.json`).
Full logs: `~/.local/share/herdr/scratch/prim-agent-ux-target/check-logs/20261117-202216/`.
All owned test processes were stopped; no install/live session changes. Estimate 22 agent minutes (12 work,
10 tests); actual about 30 (19 work, 11 builds/tests), including diagnosis of the unrelated integration failure.
