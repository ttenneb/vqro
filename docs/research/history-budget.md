# Bounded compact history (2026-11-17)

## Measurement and choice

The durable Store probe uses 600 view entries, 350 additional layouts and 500 updates to a
37 KiB/600-tab layout; a second case writes sixty 900 KiB values followed by a tiny write.
Baseline: `5458d2c`. Final implementation measured at `2c561a1`, based on `421024c`.

| Workload | Before | After |
|---|---:|---:|
| 500 layout updates: workspace file | 107,223,477 B | 8,479,223 B |
| Layout commit p50 / p99 / max | 458.39 / 1026.67 / 1050.46 ms | 43.06 / 62.37 / 65.96 ms |
| Near-limit entries: workspace file | 110,613,079 B | 9,217,510 B |
| Near-limit commit p99 | 231.18 ms | 16.50 ms |
| Following tiny commit | 147.83 ms | 9.61 ms |
| Retained records after layout / near-limit case | 500 / 61 | 220 / 10 |

Both runs use the same Store-only probe and Cargo release profile, including durable replacement.
They are not a controlled latency-equivalence experiment: the shared host was not quiescent;
the baseline used default scheduling, while the final run used nice10 and an eight-core affinity.
No sample was removed. File bounds and shared-pointer regressions are deterministic evidence;
the timing improvement is observational, not a universal latency promise.

Choose **8 MiB of compact serialized history plus the existing 500-record ceiling**. The resulting
8.09 MiB target-scale file includes current entries separately. Keeping full after values or cloning
all preimages is unnecessary: undo needs the before value and the after version. Transactions and
entry values are immutable shared references in memory. Whole-file durable replacement remains;
these measurements do not justify an append log or another persistence subsystem.

## Explicit trade-offs

A single undo snapshot above the byte ceiling becomes an audited `undoable:false` record with
keys/after versions but no preimages. Its state change still commits; default undo stops at that
eligible record with an explicit error, rather than undoing older unrelated work. Small later
changes remain reversible. Metadata that cannot fit is rejected atomically. Notes have a separate
4096 UTF-8-byte ceiling; old longer notes receive a visible truncation suffix during loading.

The 15×1000 KiB delete/revert workload previously reached 122,900,154 B after five successful
reverts. Now the delete records the explicit non-undoable boundary: its first attempted revert
is rejected and the workspace file is 2,564 B. This is intentionally not equivalent undo retention.
The regression also repeats rejected oversized reverts and verifies no history/state growth.

Undo work is reserved by its cached serialized operation size, not the small revert request.
Default undo is still resolved after preceding queued commits. A changed target must reserve its
expanded size before copying values or dispatching storage; it rejects without effect if the
bounded queue cannot grow its lease immediately. It never waits behind later queued leases.

## Verification

`tests/history_budget.rs` covers pointer sharing, exact byte accounting, near-limit eviction,
500-count eviction, explicit oversized boundaries, bounded notes, and old pretty/full-after
500-record migration followed by undo and restart. `tests/commit_off_lock.rs` checks real revert
charging, ordered target expansion and default undo stopping at an oversized marker.

The full gate includes the release history suite: its 500×37 KiB regression measured p99=55.21 ms
against a 200 ms guard, with an 8,403,967 B file. This smaller fixture has one panel view rather
than the probe's full 600-view/350-layout seed. All 21 full steps passed at `2c561a1`: 236 default
Rust tests (25 ignored), 142 Python tests, native suites, UI/scale gates, and scoped no-leftovers.

Local evidence: `<private-notes>`
(source, frozen before/final binaries, build and run logs), `history-check-full.json`, and
`history-check-full.log`. Protocol semantics: `docs/spec/core-protocol.md`, Undo horizon and history storage.
