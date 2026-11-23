# Contention-aware timing grading

`check.sh full` still runs its independent pool before the serial sensitive phase.
Serial within a gate is not isolation from other gates. The sampler records facts
for each of these steps, not a machine-wide load average from before/after the run:

| Step | Timing breach distinguishable from correctness |
| --- | --- |
| `e2e-tui-scale-budgets` | Only positively observed finite key-latency breaches (the unchanged p99 budget or existing 2-second observation deadline), with complete ledger/workload evidence, healthy extension, successful shutdown request and observed zero core exit. Unknown/unseen, explicitly lost or incomplete keys, shutdown error, forced kill or nonzero exit are hard failures. CPU, RSS and frame caps stay hard failures. |
| `scale-harness` | Finite spawn, transaction, echo, event-lag, snapshot and slow-reader latency breaches. Missing/incomplete samples (including required load-window and end RSS), RPC errors, lost echo, missed events, inadequate delivered share, restart failures, capacity bounds and leftovers stay hard failures. |
| `pi-bridge-memory` | Sampled, but all its allocation, journal/identity cardinality and owned-child checks stay hard failures. They are capacity/correctness claims, not scheduler latency claims. |

`latency_typing.py` is an opt-in telemetry harness, not registered in `check.sh`,
and has no performance budget. No new budget or purported pass is invented for it.
TUI key latency and the scale harness's RPC/echo latency are the latency gates.
History CPU-work tests, sidebar wire/publication bounds, item-stream integrity,
scale-herdr completeness and the remaining opt-in correctness suites retain their
existing grading and registration. No budget, retry policy or production timer changes.

## Exit and receipt contract

- `0`: met the step's budgets, PASS even if contended/unavailable.
- `78`: **only** a pure latency/throughput breach, correctness verified. The TUI
  probe emits it directly. The scale harness writes `failure_class` in its receipt;
  with `VQRO_SCALE_DEFER_TIMING_GRADE=1` only pure latency failures are deferred to
  the gate. The gate requires test-process success as well as the receipt before
  mapping that class to 78. Direct cargo runs remain strict.
- `77`: SKIP, never a passed step. Existing optional-dependency skips stay unchanged.
- Every other nonzero: FAIL, never downgraded for contention.

The TUI validates required finite metrics and explicit correctness/cleanup evidence
before either 0 or 78, deriving breaches from measured values rather than trusting
`result` or `budgets_exceeded` labels. Its bounded key ledger must contain complete
positive observations for every sample. A late observed echo is a latency breach,
not inferred loss; a missed deadline stays enforced even outside the p99 order
statistic. Unknown/unseen or explicitly lost keys, truncated/incomplete evidence,
a pass-only dict and NaN metrics are invalid. Historical stream signals are only
associations and never forwarding/loss witnesses; their unknown labels stay intact.
Scale requires nonempty, finite load-window RSS, no missing RSS attempts, and finite
start/setup/end RSS; a genuinely not-applicable slow-reader phase remains allowed.

In full mode, 78 plus observed contention is SKIP, reason `unverified (contended)`.
With no contention it is FAIL. `check.sh timing`, or any gate admitted with
`--exclusive`, is strict: 78 is always FAIL. Sampler facts and the original step
exit, grading exit and reason are embedded in each sampled step's check receipt
and retained in a `<step>-contention.json` log sibling. An unavailable sampler is
explicitly contended, never quiet evidence and never a pass for a budget breach.

## Sampling and admission

Each second while the root is observed live, `timing_contention.py` records:
other held full/quick lock files, `/proc/stat` procs_running, and the step's observed
process-tree **per-thread** schedstat runtime/runqueue wait. TID birth fences reuse;
observed cumulative maxima retain tasks after exit. Pidfd readiness ends sampling
without a post-exit lock/procs read. A row gathered across an observed exit is
discarded, including its counters/errors; such later facts cannot become measured
contention. Missing exit notification is explicitly unavailable.

These are retained snapshots bracketed by root liveness, not atomic kernel event
accounting. Final counters can precede exit and tasks living entirely between
samples are not measured. Observer elapsed time can extend beyond actual exit
while waiting to be scheduled. The JSON labels end observation, discarded rows
and coverage, and retains at most 600 sample rows. No exact end-counter or complete
CPU/IO exclusivity claim is made.

A window is contended if any other slot is held, median procs_running > cores/2,
runqueue wait > runtime, or any required source is unavailable. Linux implements
these facts; unsupported platforms report unavailable rather than guessing quiet.
The wrapper exports `VQRO_GATE_SLOT_DIR`, `VQRO_GATE_SLOT_HELD` (comma-separated
filenames) and `VQRO_GATE_EXCLUSIVE`; the repository never guesses a slot directory.
The common first `env -i` and the gate's service supervisor preserve only these
admission facts, not arbitrary caller variables or credentials. The candidate wrapper
strips conflicting caller assignments for all three facts in the common env/plain
or env-i prefix, then supplies its actual authoritative values. Sampler, grader,
scale-receipt parser and final receipt writer also run under an explicit env-i,
isolated Python boundary, including on direct/unscoped gates. Direct gate calls
without wrapper metadata correctly report slot facts unavailable.

The external slot wrapper's `--exclusive` acquires all configured full **and** quick
slots. A failed attempt releases its partial set before waiting. `--wait-seconds N`
defaults to 1800; exhaustion prints `could not get exclusive slots` and exits **75**,
not 0 or 77. Existing ordinary callers still acquire one slot. No fixture inherits
lock descriptors. Pool sizes must agree across callers.

## Strict proof before installation

Prebuild from the revision being graded, outside the timing-only run. The stdlib
helper reads Cargo's successful JSON stream and selects this manifest's `scale`
test target (including its package/target source), not an inventory of old hashed
executables. Its stdout is the path callers pass; no prebuild attestation file is
required. Optional `--record` is diagnostic/provenance output only.

```sh
VQRO_SCALE_TEST_EXE=$(
  "$GATE_SLOT" env -i HOME="$HOME" PATH="$PATH" CARGO_TARGET_DIR="$CARGO_TARGET_DIR" \
    bash -c 'set -o pipefail
      cargo test --release --test scale --no-run --message-format=json |
        python3 -I scripts/scale_artifact.py select --manifest Cargo.toml --target-dir "$CARGO_TARGET_DIR"'
) || exit 1
export VQRO_SCALE_TEST_EXE
"$GATE_SLOT" env -i HOME="$HOME" PATH="$PATH" CARGO_TARGET_DIR="$CARGO_TARGET_DIR" \
  cargo build --release --bin vqro
"$GATE_SLOT" --exclusive --wait-seconds 1800 env -i HOME="$HOME" PATH="$PATH" \
  CARGO_TARGET_DIR="$CARGO_TARGET_DIR" VQRO_SCALE_TEST_EXE="$VQRO_SCALE_TEST_EXE" scripts/check.sh timing
```

Timing mode runs only the three sampled steps above plus the usual owned-leftover
receipt/cleanup guard. It never builds or runs the unrelated quick/full suites.
It requires a release vqro and an explicit `VQRO_SCALE_TEST_EXE`. Timing only
checks that the nominated file exists, is executable and canonically inside
`CARGO_TARGET_DIR` (symlink escapes rejected). Missing/invalid paths fail with
`prebuild the release scale test and pass its path`; they are not skips. Timing
never invokes Cargo or scans the artifact directory. Stale extra binaries are
irrelevant. Full gates may prebuild with Cargo JSON and run the selected artifact.

Stable Cargo `--no-run` still compiles, and `fresh:false` is reported afterward.
Therefore no build-capable Cargo freshness query runs in timing admission. The
explicit prebuild may report non-fresh artifacts because builds are allowed there.
This is the caller-attested policy, not a new build/fingerprint framework.
Receipts label build
provenance **caller-attested, not revision-bound**, name the checkout path and full
checkout commit, and record SHA256 plus nanosecond Unix mtime of the release vqro
and the one chosen scale-test artifact, not unrelated old files. This identifies what was graded; it does not
claim that a hash or mtime binds a prebuilt artifact to the checkout revision.
Archive its strict receipt beside the full receipt; a timed-out exclusive admission
is not a grading run. Target duration is under about five minutes, excluding admission
and prebuilds. Gate-script changes require secondary review before merge.

Controls are `test_timing_contention.py`, `test_check.py` and
`test_tui_scale_budgets.py`, plus scale's
`timing_grade_never_excuses_correctness_or_missing_measurements`. External wrapper
controls run when `VQRO_TEST_GATE_SLOT` names the wrapper, always with private lock
directories. They need no real contention or backend and include partial-lock release,
strict/full grading, unavailable facts, actual held locks, command status and descriptor
non-inheritance. Existing budget constants are unchanged.
