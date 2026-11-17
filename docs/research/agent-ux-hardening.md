# Agent UX hardening (2026-11-17)

Tested implementation: `agent-ux` at `67e63dc`, based on `25cb901`. No install or live-session changes.

## Findings and changes

- `tabs.show` previously made a terminal view for every resource id. `panes.show` likewise rebound a terminal
  view to any picked id. Both now use runtime-neutral resource-kind routing (`extensions/prim_views.py`): agents
  go through `agents.show`, terminals through terminal views, unknown kinds are refused. Only unavailable/missing
  agent commands permit host-terminal fallback; timeouts and uncertain effects are not replayed as navigation.
- One New Pi agent opens one agent tab. The bridge is an explicit **Bridge terminal** action and a collapsed
  sidebar child, titled `Pi rN bridge` in workspace UI. Spawn names are unique short `pi-bridge-<suffix>` strings.
  A private test caught that a display-style name with spaces is rejected by the terminal host; it is not used.
- Pi panels show readable activity, nonzero queue/request counts and the bounded transcript. The diagnostic
  terminal redraws its sanitized, bounded screen rather than appending repeated transcript snapshots. The
  duplicate request/failure sections in that screen are removed.
- Finished sidebar items have an **ended** badge; wholly ended/gone resource tabs get title markers. Status-only
  updates do not write transactions. Close-ended preserves mixed/live tabs and the last tab. Sidebar cleanup
  hides ended items with a reversible preference, never backend deletion or program termination. Disconnection
  remains **unavailable**, not **gone**. Missing agents retain an explanatory panel and close instructions.
- A real private mouse test exposed a missing generic client primitive: activating a tab from a sidebar leaves
  keyboard focus in the sidebar. The approved `client.focus {view}` command-reply hint reuses TUI pending-focus,
  affects only the invoking client, and ignores hidden/missing views. Tabs/agents/Pi forward it; no resource-kind
  policy was added to Rust, and server/view ownership code was untouched. Protocol and TUI unit test cover it.

## Fake Pi latency

Seven sequential prompt/abort cycles per run, private herdr-core and prim, no model/network calls. A persistent
public RPC connection invokes the same `agents.prompt` command used after prefix-menu text submission. Timestamps
record acceptance, first active resource facts and first Running panel event; the fake separately records native
command receipt/response emission. Human typing, CLI startup and TUI capture polling are excluded. This is a
local-path measurement, not a real Pi/model latency claim.

Median milliseconds:

| Measurement | Before urgent publication | After (standalone) | Final full gate (8-core affinity, nice 10) |
|---|---:|---:|---:|
| Command acceptance | 33.148 | 26.869 | 37.759 |
| Active resource facts | 123.322 | 26.095 | 35.861 |
| Running agent panel | 125.216 | 28.713 | 39.456 |
| Fake runtime receive → response | 0.095 | 0.099 | 0.104 |

Before panel samples ranged 106.6–136.6 ms; standalone after 23.3–33.7 ms. Final-gate after ranged 26.5–48.3 ms.
The avoidable prim-side delay was the bridge's 200 ms publication sampling timer. Explicit user effects now wake
that publisher immediately; ordinary streamed transcript updates remain sampled/bounded. Acceptance-minus-native
processing was 26.764 ms standalone after (37.658 ms in the throttled full gate). Scheduling/contention affects
these small local timings; no guarantee or model-time inference is made.

## Verification and retained evidence

`CARGO_TARGET_DIR=~/.local/share/herdr/scratch/prim-agent-ux-target`, clean environment, nice 10 and eight-core
CPU affinity. `scripts/check.sh full` passed **19/19**, including zero-warning build, cargo tests, **142 Python
units**, all opt-in providers, fake Pi CLI/TUI, scale/TUI budgets and scoped no-leftover-processes check.

- Full receipt: `~/.local/share/herdr/scratch/prim-agent-ux-target/check-receipt.json`
- Full logs: `~/.local/share/herdr/scratch/prim-agent-ux-target/check-logs/20261117-183035/`
- Baseline timing receipt: `~/.local/share/herdr/scratch/ep-tfal8twt/receipt.json` (run intentionally exposed an
  incorrect test expectation that provider disconnection means gone; corrected to unavailable).
- Standalone after receipt: `~/.local/share/herdr/scratch/ep-dyiu5161/receipt.json`
- Final full-gate UI receipt/screens: `~/.local/share/herdr/scratch/ep-nz0ahoyr/`
- Extra mouse-focus regression receipt/screens: `~/.local/share/herdr/scratch/prim-agent-ux/ep-oog3m8wk/`

TUI checks physically launch via prefix, activate Bridge terminal, click the sidebar agent, then verify the
focused pane's keyboard menu contains Prompt/Abort. They assert one agent tab and no terminal view of an agent,
exercise non-Pi capability-driven controls and agents-off terminal fallback, show gone/ended UI, run explicit
cleanup, and prove bridge/runtime lifetime and no effect replay across core restart/safe mode. Every private
suite cleans up its owned processes; small receipts/screens remain for review.

Estimated 70 agent minutes (45 work, 25 tests/wait); actual about 32 minutes (about 21 work, 11 builds/tests).
