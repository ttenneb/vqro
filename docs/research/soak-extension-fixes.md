# Soak extension fixes (2026-11-17)

Baseline `f018512`; tested code `9b609b1` on `soak-ext-fixes`. Addresses review-soak P1-1, P1-3 and P2-1.
No live sessions/install or provider registry/reclamation code changed.

## Fixes and red/green evidence

- **Refused bridges stop their Pi.** An explicit registration refusal (including overloaded or unavailable error
  replies) reports its reason in the bounded diagnostic terminal and exits through owned-child cleanup. An
  accepted registration without a usable resume credential also exits. Transport gaps remain distinct: bounded
  exponential backoff and last-observed credential recovery still handle a lost first registration reply.
  Effects are never replayed. Choosing exit rather than retrying a known refusal is deliberate: no unreachable
  runtime is kept indefinitely and no consumed-token assumption is needed. The user can relaunch after recovery.
- **Subscriptions select their live data.** Generic `events.subscribe.view_content` supports exact names and
  literal prefixes, with the same filtering for replay/new content/removal. Agents requests `agent-` output;
  Settings requests themes/icons/keys; Sidebar requests workspaces. Other bundled UI extensions drop unused
  view-content subscriptions. The core change is only event shaping, not provider wire/identity/ownership.
- **Overflow resyncs instead of exhausting the crash budget.** Extension notifications reserve a small local gap
  marker within the existing 4 MiB cap. Overflow discards only undispatched notifications, keeps the active
  callback charged and queues one explicit missed/snapshot fence. Events arriving during snapshot acquisition
  are buffered and fenced by sequence; another overflow schedules another fence. RPC replies and command
  requests are untouched, and no effects are replayed. A failed recovery snapshot closes the transport rather
  than continuing with a divergent cache. Raw SDK users retain disconnect behavior unless explicitly opting in
  with a snapshot-capable handler.
- **Agent updates are targeted.** A resource event updates only that agent or agents hosted by that terminal;
  unchanged panel RPCs are skipped. The old path republished every agent panel for every terminal status change.
- **Sidebar output is bounded.** Shared 64-logical-row tree pages retain ancestors and grouped bridge children;
  every resource remains reachable, with previous/next/direct page controls and a full-inventory chooser.
  Filtering is explicitly page-local. Global blocked counts still include off-page resources. Publications
  coalesce for 500 ms and unchanged visible pages are not republished; page choices are derived extension state,
  not history. This intentionally reduces data delivery, not resource retention or program lifetime.

Red failures and green logs are under `<private-notes>`:
`bridge-red.log`, `filter-red.log`, `queue-red.log`, `sidebar-red.log`, `agents-red.log`, and corresponding green
logs. Tests cover explicit refusals, missing resume credentials, transport/lost-reply recovery, view filters on
live/replayed/removal events, in-flight byte charging, snapshot fencing, failed recovery, targeted publications,
all-page reachability, and off-page global blocked counts.

## 600-terminal / 200-agent / 350-workspace measurement

`tests/e2e_sidebar_scale.py` drives a private real prim core and production Python extensions through public
provider/resource/view protocols. A single synthetic backend supplies 600 terminal and 200 agent facts plus 350
workspace facts, without terminal/model execution. This isolates extension traffic from the separately assigned
provider-registry cap. Agent hosts are not fabricated; the ungrouped synthetic inventory is conservative for
sidebar size. Actual fake-Pi process/lifetime tests remain in the full gate.

The A/B changes only the sidebar script (`f018512` vs current); other extensions and the core are the same fixed
versions in both arms. At nice 10, each arm runs 20 resource updates/s plus native agent content, with 30-second
off-page and on-page phases (600 resource updates each), then drains the coalescing window:

| Metric | Old sidebar | Bounded sidebar |
|---|---:|---:|
| Initial sidebar JSON bytes | 612,019 | 32,585 |
| Off-page phase sidebar bytes | 26,099,561 | 0 |
| On-page phase sidebar bytes | 26,103,674 | 1,540,759 |
| On-page bandwidth, KiB/s (including 0.8s drain) | 827.7 | 48.9 |
| On-page publications | 47 | 52 |

Visible-change bandwidth fell **16.9x** despite slightly more frequent, much smaller updates. Off-page detail
produced no sidebar publications. The inventory used 18 pages; switching pages was verified. Agents, Settings,
Sidebar and Tabs stayed running with zero crashes in both arms. A filtered subscriber received zero sidebar
bytes while receiving matching native-agent content. This is a focused two-minute A/B, not a claim of a new
multi-hour execution soak; contention can affect frequency.

Receipts: `<private-notes>,after}-30s.json`.
Reproduce with clean environment and the private binary:

```
env -i HOME="$HOME" PATH=/usr/bin:/bin python3 tests/e2e_sidebar_scale.py ABS_PRIM --seconds 30
```

Use `--baseline-sidebar PATH` for the old script arm; `--receipt PATH` retains an external receipt.

## Release gate

`scripts/check.sh full` at `9b609b1`: **20/20 PASS**, zero-warning build, cargo tests, **152 Python units**, provider
opt-ins, private fake-Pi CLI/TUI, new sidebar-scale regression, scale/TUI budgets and scoped no leftover processes.
The first run exposed a pre-existing assumption in the UI harness that any sidebar snapshot already contained
its newly created agent. Waiting for the actual coalesced agent row corrected that race; the physical mouse and
keyboard-focus assertions remain intact and pass.

- Receipt: `<private-notes>`
- Logs: `<private-notes>`
- Extra private TUI receipt: `<private-notes>`

All test processes were stopped. Small receipts/logs remain for review. Estimated 60 agent minutes (40 work,
20 tests/wait); actual about 39 minutes (24 work, 15 builds/tests). No registry cap, terminal retention or real
model/provider calls were changed or exercised by the new scale fixture.
