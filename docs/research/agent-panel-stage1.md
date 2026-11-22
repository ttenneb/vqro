# Agent panel Stage 1 — references, split publications and measured traffic

2026-11-18. Branch/worktree `agent-panels-stage1`, based on the separately ready memory fix `79a3134`.
Tested implementation `65ed972`. **Stage 2 is held for the coordinator/human's explicit go after these numbers.**
The earlier per-subscriber delta prototype is not on this branch.

## What changed

- `extensions/agents.py` publishes a small neutral shell, not a copy of native output. Its capability-gated
  controls, host-terminal action, error/retry facts and stale/ended warnings remain extension-owned.
- Runtime-neutral optional `data.views` advertises up to eight ordered source names. Existing runtimes without
  it retain canonical `agent-rN` compatibility, including live-only native fragments.
- `src/tui/compose.rs` implements generic read-only `type:view` rows; panels declare `embedded_views`. The TUI
  resolves current source entry/live items and tracks dependencies so source updates/removal invalidate only
  affected cached compositions. No second publication/writer is introduced. Source commands/args/options/input,
  menu/chips and detail panels are not imported. Terminal sources never attach. Missing, unsupported and cyclic
  references have visible fallbacks; reference depth and expanded row count are bounded. Text controls are
  escaped. This initial capability embeds ordinary items/children, not source pages/toolbars; its limits and
  older-client fallback are documented in `docs/spec/core-protocol.md`.
- The Pi bridge publishes status/queues/dialogs to `agent-status-rN` and stable transcript rows to `agent-rN`.
  Each is deduplicated independently; a status-only queue change does not resend the transcript. No relative
  timestamps or new native effect/retry behavior were added. The128-row/16KiB transcript is unchanged.
- Scoped declaration/reuse/collision handling applies independently to both views. A foreign output declaration
  is never overwritten; an owned status view may still work. Retention prunes the extra orphan status declaration
  using the same marker/grace/reference rules as the existing output declaration.

## Before/after at30 and200 agents

`tests/agent_panel_bandwidth.py` is a private **public-wire renderer benchmark**, not a native-process soak.
It uses actual before/after bridge renderers and agents extension, synthetic native facts,128 prewarmed rows,
and the control driver's deterministic20 attempts/s, prompt:abort:steer =2:1:1. Busy prompt rejections produce no
publication. Each before/after pair has the same schedule and acceptance count; every final native and neutral
panel is acknowledged by an exact observer-content fence before counters close. No models or HC are used.

| Agents | Accepted/s | Before KiB/s | After KiB/s | Reduction | Before→after bytes/accepted invoke |
|---|---:|---:|---:|---:|---:|
|30|13.6|145.12|37.63|3.86×|10,945→2,835|
|200|10.0|112.24|19.27|5.82×|11,612→1,976|

Measured bytes/min:30agents8,916,078→2,312,122;200agents6,895,806→1,183,859. The larger population does **not**
assume a proportionally larger total invocation rate. This is a short warm-tail stress profile, not an apples-to-
apples replacement for the80-minute real200-bridge soak's60KiB/s result. At200 agents, the neutral shell
contributes only11,384 bytes over the whole sample, versus289,484 before. Native output/status contribute
61,639/25,779 bytes; the bridge-to-core full output leg is still present when the transcript actually changes.

Receipt: `<private-notes>`; raw logs and four private
run receipts are adjacent. Baseline program/agents files are generated from `79a3134`. Reproduction:

```
env -i HOME=$HOME PATH=$PATH python3 tests/agent_panel_bandwidth.py <prim> \
  --before-agents <baseline-agents.py> --before-program <baseline-program.py> \
  --scratch <private-short-root> --report <receipt.json>
```

## Verification and rollout boundary

`65ed972`: `scripts/check.sh full` **26/26 PASS**, zero-warning Rust build, **195 Python tests**, Rust composition/
dependency invalidation/cycle/terminal/read-only tests, queue-only no-transcript-publication regression,
600/200/350 sidebar/no-mirror check, physical Pi TUI/error/retry and second-runtime rendering, large-workspace
restart/resume, history, providers, drift, scale and scoped no-leftover processes. Prompt latency instrumentation
now observes the composed status source, not a nonexistent mirror containing "Running".

- Receipt: `<private-notes>`
- Logs: `<private-notes>`
- Memory fix remains a separate commit and its sustained memory gate also passed with two-view publication.

No install, live endpoint, human process or model was touched. Existing running bridges retain loaded code:
new clients/agents extension can reference their old canonical combined view without mirroring, but splitting
status/output requires a new bridge. No bridge was restarted or native input replayed for deployment.

Initial forecast34 agent minutes (22 work,12 tests); actual about36 through closeout. Full verification finished
01:03 EST, against the revised01:05 forecast (original stage deadline01:00). The before/after numbers were sent
before any Stage 2 implementation; next action belongs to the coordinator/human, not an automatic continuation.
