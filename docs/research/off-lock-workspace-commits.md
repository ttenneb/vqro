# Off-lock durable workspace commits (2026-11-17)

## Mechanism and invariants

`src/commit.rs` is the single running-workspace mutation owner. It reuses the existing byte-budgeted
queue, with 128 request slots and 8 MiB encoded pending input (including the active operation). Slots
and byte permits survive receipt cancellation and remain held through publication/reconciliation.
Admission failure is `overloaded`/`outcome: rejected`, before any transaction effect.

Committed entries and history are immutable `Arc`s in `Store`. At the head of the FIFO, the worker
briefly locks `Core` to prepare any recovery-command operation and take an O(1) committed copy. It
then runs `Store::apply`/`revert` on the blocking pool: candidate cloning, validation/CAS, size admission,
JSON encoding, temporary-file write/sync, rename and directory sync do not hold the core mutex.

The worker adopts the result under the core lock and publishes its entries event before taking the
next request. A clean error leaves the old store and transaction counter untouched; a post-rename
`durability_unconfirmed` adopts/publishes the candidate exactly once. Old store destruction happens
after releasing the lock. Extension reconciliation is owned by the worker, not the awaiting caller.
An unexpected writer-task panic is indeterminate and closes the committer, rather than risking an
overwrite from stale memory. Restart is required for that exceptional failure.

There is no speculative public pending state: readers see the last adopted commit while storage is
busy. A queued CAS runs only after preceding commits are adopted, so it cannot validate against the
same old version as another pending write. Default undo and recovery-operation construction also
happen at queue head. Runtime `workspace.apply`, `workspace.revert`, undo, layout reset, and extension
enable/disable all use this path. Startup defaults and standalone `Store` tests retain synchronous
`Store::apply`; `Core.store` is crate-private to discourage bypassing the running committer.

## Deterministic regression evidence

`tests/commit_off_lock.rs` uses a private fake core on a separate two-worker runtime, so a blocked
core cannot also stop the test's deadlines. A gated writer is deliberately not released until the
unrelated snapshot/echo probes complete or time out:

- Before: snapshot and echo both timed out after 201.58 ms.
- After: both completed in 0.36–0.44 ms while the write was still gated. The snapshot contained no
  pending entry; the entry appeared only after the gate was released and persistence completed.
- Additional cases cover queued dependent CAS, one winner for the same expected version, transaction
  and event order, default undo selected after preceding commits, clean/post-rename errors, cancelled
  callers preserving extension reconciliation, and a writer crash after replacement failing closed.
- An escaped-note regression proves encoded metadata admission: three roughly 3 MiB JSON notes,
  each only 500 KiB raw text, cannot bypass the 8 MiB budget. The red test timed out because the
  third request was wrongly admitted; green rejects it before effects.
- Unit tests check both request-count and byte permits, including dequeued work and dropped receipts.

`Core::configure_workspace_writer` is an in-process harness hook, not a wire method. It can only be
configured before the first commit admission attempt. Default production behavior still uses the
real atomic writer with no injected delay.

## Storage-focused scale comparison

Release builds, real temporary files on the home filesystem, fake backends, all `HERDR_*` variables
removed. Pinned synchronous baseline: `dcbae2f`, archived into scratch with only the identical writer
hook and scale knobs added. Off-lock measurements: `259b6b7` plus the scale-only latency knobs.
The later escaped-metadata correction does not change the write algorithm.

Profile: 600 terminals, 200 agents, eight 1 MB/s floods, two adapters, 20 offered transactions/s,
status period 1000 ms, echo period 20 ms, two core workers, 10-second load. Slow/stalled clients are
explicitly disabled to isolate storage; this is not a new slow-reader stress claim. Delay is inserted
before each **real** durable workspace write only in the test child.

| Implementation | Added write delay | Txn p99 ms | Echo p99 ms | Event lag p99 ms | Txn/s achieved |
|---|---:|---:|---:|---:|---:|
| synchronous baseline | 0 ms | 3.263 | 2.060 | 1 | 20.18 |
| off-lock | 0 ms | 3.824 | 0.334 | 1 | 20.17 |
| synchronous baseline | 80 ms | 84.274 | 1231.998 | 167 | 12.09 |
| off-lock | 80 ms | 82.992 | 0.328 | 1 | 12.18 |

Both zero-delay runs passed all unchanged scale budgets. Both 80 ms runs intentionally exceeded the
existing 10/50 ms transaction p50/p99 budgets; the synchronous baseline additionally exceeded the
50 ms echo budget. No budgets were widened. Durable transaction latency/throughput still reflect
storage cost; the improvement is isolation of unrelated work, not pretending fsync became faster.
Each run had zero transaction errors, zero lost echo probes and zero leaked child processes. Peak
core RSS was 23.5–24.1 MiB in these profiles. These are individual local runs, not statistical bounds.

After rebasing onto provider-wire implementation `f2c1f21`, all 134 Rust tests and 51 strict Python
tests passed with zero warnings. Repeating the same release profiles gave:

| Integrated implementation | Added delay | Txn p99 ms | Echo p99 ms | Event lag p99 ms |
|---|---:|---:|---:|---:|
| provider wire + off-lock | 0 ms | 3.823 | 0.488 | 1 |
| provider wire + off-lock | 80 ms | 83.249 | 0.457 | 1 |

The normal run again passed every budget; the delayed run exceeded only the unchanged transaction
p50/p99 ceilings. No transaction errors, lost echoes or leaked processes; peak RSS 24.3–25.1 MiB.
The integrated queue's original-record-byte metadata required no committer adaptation.

Reproduce in an isolated environment using `cargo test --release --test scale scale_load -- --ignored
--exact --nocapture` and these `PRIM_SCALE_` settings:

```
TERMINALS=600 AGENTS=200 FLOODS=8 FLOOD_BYTES_PER_SEC=1000000 STATUS_EVERY_MS=1000
TXN_PER_SEC=20 STALLED_CLIENTS=0 SLOW_CLIENTS=0 ADAPTERS=2 DURATION_S=10 ECHO_EVERY_MS=20
CORE_WORKERS=2 WORKSPACE_WRITE_DELAY_MS=0   # change delay to80 for the slow-storage run
```

Set a private short `PRIM_SCALE_RUN_DIR`, a receipt path via `PRIM_SCALE_RECEIPT`, and the assigned
`CARGO_TARGET_DIR`; run via `env -i HOME=$HOME PATH=$PATH ...`. The knobs are read only by the scale
harness/test child, never `prim serve`.

Raw receipts/logs are under `<private-notes>`:
`commit-scale-{normal,slow,baseline-normal,baseline-slow}.{json,log}`; deterministic red/green logs
are `commit-off-lock-{red,green}.log` and `commit-metadata-red.log`. Integrated receipts/logs are
`commit-scale-integrated-{0,80}.{json,log}` and `commit-off-lock-integrated-{cargo,python}.log`.

## Remaining limits

This removes workspace persistence/encoding from the core mutex, not every possible core filesystem
operation: resource-identity persistence and extension log/process setup keep their existing paths.
Publication/event construction still does bounded CPU work under the lock; copying/serializing large
retained transaction history can be expensive off-lock. A filesystem call already in progress cannot
be safely cancelled, and an admitted caller timeout does not imply failure or permission to replay.
