# Slow-client startup and steady-state budgets (2026-11-17)

## Decision and reason

The scale fixture now separates the **first three seconds after a slow client's successful socket
connection** from steady state. A probe is assigned by **when its input starts**, not when the echo
arrives. A probe starting before3000ms remains startup even if it completes later; one starting at
or after3000ms is steady. Waiting longer for an attach reply does not start a fresh startup window.

Approved budgets for the default64KiB/s reader:

- `slow_echo_startup_max_ms`:3000ms for startup probes.
- `slow_echo_p99_ms`:250ms for steady probes (unchanged threshold).
- `slow_echo_max_ms`:1000ms for steady probes, including unfinished lower bounds (unchanged threshold).

This is a benchmark calibration, **not a core latency fix**. Initial state/base establishment (first
full resource facts), attach replies and queued control traffic share the throttled byte stream with
echo. That initial snapshot-like/control backlog is inherent: roughly100KB takes about1.5s at64KiB/s,
regardless of how quickly the core handles input. The fixture does not issue `workspace.snapshot`;
its initial state arrives through first/full resource events after subscription.

In the attribution matrix, startup tails occurred on current, pre-off-lock and pre-provider-wire
revisions.36 interleaved default release runs found maxima up to2253ms; every maximum was the first
probe. No statistically confirmed off-lock shift was found (not proof of equivalence). A traced
pre-off-lock probe took1551ms to read101,335 bytes, mostly resource/entries control traffic plus
13,498 flood bytes:101,335/65,536 is about1.546s. After a conservative3s warm-up, observed worst probes
were30ms current,25ms pre-off-lock and24ms pre-wire. Budgets were split by explicit coordinator
decision; the steady thresholds were not loosened to hide those startup samples.

## Receipt and guards

All samples remain visible. Per slow client:

- Existing `echo` statistics and rounded `echo_series_ms` still include **all** completed and timeout
  probes, including startup. They are informational, not the new steady-only budget population.
- `echo_startup` and `echo_steady` contain the phase-specific statistics.
- `echo_samples` retains exact latency, `started_after_connect_ms`, phase and outcome for each probe.
- Existing `echo_unfinished_ms` remains; `echo_unfinished` adds its start timestamp and phase.
  Unfinished probes are lower bounds, not invented completed/percentile samples. Each phase's max
  includes its unfinished lower bounds, even if the run stops in another phase.
- Root `slow_echo_phases` records the connection anchor,3000ms window and start-time classification.

An empty phase is explicitly not applicable (null measurement), which permits short startup-only
profiles and clients whose first probe is already steady. This never bypasses the existing minimum
sample guard **for each configured slow client**. A missing client/sample still fails
`slow_echo_samples`; no-client profiles remain valid. Timeouts remain in the receipt/sample population
and an explicit `slow_echo_timeouts=0` guard prevents a short configured deadline from being accepted
merely because its numeric duration fits the larger startup allowance.

Tests in `tests/scale.rs::slow_phase_tests` cover exact boundary/cross-boundary assignment, late first
probes, both phase limits, all-sample retention, missing/no-client and startup-only runs, unfinished
bounds and short timeouts. The old all-probes-as-steady behavior fails the startup/boundary regressions.

Raw attribution evidence is preserved at
`<private-notes>` and `distribution.json`.
This startup allowance is an empirical guard for this fixed profile, not a universal transport bound.
