# Soak P3 closeout (2026-11-17)

Branch `soak-p3` from main `6f28c53`; tested implementation `d0782ae`.

## Orphaned tab views (`7b4f813`)

`tabs.close` now collects every view in the closed subtree and deletes only those no resulting layout uses.
`tabs.close_ended` performs the same check across all changed and unchanged layouts, deduplicating shared
candidates. Layout writes and version-guarded view deletions are one transaction. Shared views, the last/mixed
retained tab, and unrelated non-layout popup/declaration views remain. This is not an indiscriminate sweep of
old unused declarations. Speculative tab edits use a copied layout, so rejected commits do not mutate the SDK
cache. No resource kill/forget is performed by the tab action.

Retention continues to watch layout references, not the existence of a view declaration. The private native-fake
integration uses real Tabs and Retention extensions: a mirror layout keeps a finished resource/view pinned;
closing its final tab removes the view; undo restores view and layout atomically and cancels pending forgetting;
closing it again allows the existing grace to expire. A live terminal remains untouched. Python regressions
also cover nested subtrees, cross-layout sharing, unrelated popups, last/mixed tabs and a rejected commit.

## Crash counts (`d0782ae`)

Chosen policy: expose both the current crash-budget count and the separate supervision total, rather than
throwing away historical failures after a healthy run.

- `crashes`: unexpected exits still in the 300-second budget window, excluding a budget explicitly cleared by
  restart/spec replacement. Computed at fact serialization, so an idle healthy run does not keep expired exits
  merely because no new crash pruned the deque.
- `crashes_total`: unexpected exits since this declaration runtime was created in this core. Budget resets
  preserve it; declaration removal or core restart resets it. It is not persisted lifetime history.
- `crash_window_seconds`: the window used for the recent count. Settings labels both counts and the total's
  scope explicitly; an older core without the total field displays unavailable rather than inventing a total.
- Historical `last_exit` remains. Intentional stop/replacement is not a crash. The failed latch and restart
  budget policy are unchanged: aging to zero does not silently restart a failed extension.

Deterministic clock tests cover expiration, exact window boundary, reset/total separation and failed-latch
preservation. A real supervised-process integration verifies three failures, spec replacement and intentional
safe-mode/disable stops. Both `extension.list`/snapshots and Settings use the same facts. Normal Settings refresh
obtains freshly serialized counts; no new periodic polling mechanism was added.

## Verification

`scripts/check.sh full` at `d0782ae`: **23/23 PASS**, zero-warning build, cargo tests, **168 Python tests**, provider
opt-ins, private retention/undo integration, fake Pi CLI/TUI, target-scale/sidebar/history probes and scoped no
leftover processes. Builds used the existing private per-agent target, clean environments, nice 10 and eight-core
affinity. No live install, sessions, core or backend was touched.

- Receipt: `~/.local/share/herdr/scratch/prim-agent-ux/soak-p3/check-receipt.json`
- Logs: `~/.local/share/herdr/scratch/prim-agent-ux-target/check-logs/20261117-210333/`
- Red/green evidence: `~/.local/share/herdr/scratch/prim-agent-ux/soak-p3/`
  (`tabs-{red,green}.log`, `crashes-{red,green}.log`, `settings-{red,green}.log`, `retention-green.log`,
  `crashes-integration.log`, `full-gate.log`).

Estimated 25 agent minutes (15 work, 10 tests); actual about 27 (19 work, 8 builds/tests). All owned test
processes were stopped; small receipts/logs remain for review.
