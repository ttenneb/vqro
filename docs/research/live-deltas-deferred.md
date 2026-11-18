# Live-view deltas — deferred design and critique

## Decision (2026-11-18)

**Stage 2 is deferred by the human.** Stage 1's measured reduction is sufficient; do not add a live-delta protocol,
compact-log representation or scoped view-recovery API merely because a prototype exists. Reopening requires a
new decision supported by measured pressure after Stage 1, not the earlier linear bandwidth extrapolation.
There is no active Stage 2 checkpoint/deadline and no pending implementation authorization.

Landed on main by the coordinator:

- `e0d767f` (`79a3134` on the worker branch): bounded bridge journal object count and effect-identity history.
- `8fd5152` (`65ed972` on the worker branch): no neutral-panel mirror, generic read-only source references and
  separate Pi status/transcript publications. The full Stage 1 report is [agent-panel-stage1.md](agent-panel-stage1.md).

Stage 1 passed **26/26** full-gate steps, including 195 Python tests and physical TUI checks. Warm-tail public-wire
renderer A/B measured **145.12→37.63 KiB/s at 30 agents** and **112.24→19.27 KiB/s at 200 agents**. The deterministic
schedule was 20 attempts/s total, not per agent; accepted rates were 13.6/s and 10/s respectively. These short,
prewarmed, synthetic-native-state measurements are not a replacement for the independent 80-minute native soak,
which measured about **60 KiB/s total for 200 agents before Stage 1**, with live content plateauing near 1.1 MB.
That direct evidence was already far below the original 1 MB/s extrapolation.

## Preserved material

- [Original reviewed proposal and prototype notes](live-deltas-original-proposal.md), with an archival banner.
- [Independent design critique](live-deltas-design-critique.md), preserved verbatim from the reviewer.
- Prototype implementation `a553bdc`, branch `agent-panel-traffic`; closeout `dd9e807`. It passed a private full
  gate and demonstrated roughly 20× lower delivered bytes under a different, higher-rate synthetic workload.
  **It did not implement the critique's architecture and was not merged.** Its result is not an incremental
  Stage 2 benefit measured on top of Stage 1. The Stage 1 branch starts from the memory fix, not that prototype.

The proposal's per-subscriber caches and provisional schedules remain historical evidence, not product contracts.
`core-protocol.md` documents the landed reference capability, **not** a live patch subscription or `view.get`.

## Requirements if the work is reopened

Retain the generic, opt-in direction, full snapshots/legacy behavior and no backend-specific rules. Incorporate
**all** critique requirements rather than shipping the prototype unchanged:

1. **Compute one patch per publication** against the previous authoritative live value. Subscribers retain only
   view→last-delivered sequence, not separate content copies or N-way diff work. New, filtered, evicted and
   mismatched bases receive an honest full recovery/fallback.
2. **Compact shared event history**, not only delivered bytes. Explicitly design historical replay and legacy
   delivery: current live state must never be mislabeled with an older event sequence. If an old full state is
   unavailable, report a recovery gap or retain enough bounded information to reconstruct it; do not silently
   substitute newer content. Define and measure actual retained memory, not just encoded-log bytes.
3. Preserve Stage 1's no-mirror composition and status/transcript split. Source rows remain stable without relative
   times; generic controls, read-only sanitation, error/retry and stale/ended warnings remain intact.
4. Match arrays by unique string row `id` when all rows have one, with bounded index lookahead otherwise. A
   separate keyed-row wire API is unnecessary. Prefer add/remove/replace; any `move` extension needs an explicit
   decision and compatible consumers. Cap operations, path depth/length, encoded patch size and traversal work;
   use full fallback when complexity/savings limits are not met.
5. Add generic **`view.get {view}` → `{seq, content}`** for single-view recovery instead of requiring a whole
   workspace snapshot for one bad base. Specify ordering with concurrent publications, content removal, epoch
   changes, snapshot fences and filter/reconnect changes. The first post-snapshot patch may use an implicit
   covered base only when the fixed snapshot sequence proves it; never use a moving global cursor as proof.
6. Apply patches atomically in Rust and Python, preserving full materialized content for extension callbacks.
   Unsupported/malformed edits and mismatched bases recover rather than partially render. Older clients/cores
   retain full behavior; ownership, admission and publication lifetime rules do not change.
7. Property-test `apply(diff(a,b),a)==b` in Rust, Python and across Rust-produced/Python-applied fixtures. Cover
   escaped pointers (`~0`, `~1`), unicode keys, null/type changes, mixed arrays, keyed shifts/reorders, removals,
   malformed edits, missing bases and recovery fences.
8. Measure subscriber bytes, producer traffic, shared-log churn, CPU and retained memory together. Gate a
   declared workload at **600 terminals / 200 agents / 350 workspaces**, with an explicit target near **80 KiB/s**
   at 200 agents (the review's illustrative load: one status tick/s and one transcript row/2s per agent). State
   actual achieved rates; do not let slow elapsed time hide a failed byte budget. Full gate and independent
   implementation re-review are required before proposing release.

## Alternatives retained from the review

- **Append-only transcript stream:** defer. Appropriate if output must exceed a bounded window or needs resumable
  scrollback, but entails lifetime, cursor/gap, retention, credit and renderer contracts. Do not introduce it only
  to optimize present state panels.
- **Keyed-row protocol:** avoid a separate API; use stable IDs inside a generic diff if deltas become necessary.
- **Generic core diff:** a candidate only with once-per-publication work and compact, truthful history. It does
  not inherently remove provider-to-core full publications or prove a CPU improvement.
- **Split views and no-mirror composition:** landed in Stage 1; measure them before adding more machinery.
- **Terminal-stream credit for panels:** not the same model; terminal bytes and declarative state have different
  recovery semantics.

No live install or process restart was performed by the worker. This closeout adds documentation only; Stage 2
remains intentionally unimplemented.
