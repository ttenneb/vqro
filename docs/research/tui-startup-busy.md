# TUI snapshot admission recovery (2026-11-18)

Baseline a5ec030; final re-review P2-1. Snapshot construction already had two ordinary slots and a250ms admission
wait. Startup propagated overloaded out of run(); resync closed an otherwise healthy peer on any non-too_large
error. The same startup path runs after a private/live install's reconnect/re-exec.

## Lean fix

Reuse the built-in recovery snapshot and existing App retry/resync state, not another connection manager or daemon.
Overloaded/unavailable/timeout snapshots start an interactive recovery UI, schedule jittered exponential backoff
(0.5–3s), and show a persistent core-busy badge. Existing UIs keep their last cache and streams; read-only resync
retries on the same peer. Every snapshot attempt is bounded to5s. Retries continue while the user keeps the UI
open: a finite attempt budget would merely defer the original unwanted exit. Commands/input are never replayed.

During a scheduled wait, the next snapshot subsumes skipped events. In-flight deltas remain generation/epoch/seq
fenced. A fixed relative idle tick could itself starve under continuous events: snapshot retry uses an absolute
deadline before incoming messages, with user keys still first. Successful recovery clears the busy notice.

CLI diagnostic requests instead report promptly:2s connection/hello,2s optional status list,5s snapshot call.
Optional status-list failures are visible. Arbitrary CLI calls keep their60s request deadline to avoid cutting
legitimate long commands. No CLI replay was added. Too-large and nontransient failures keep separate handling.

## Evidence

- Red/green committed tests: overloaded/unavailable/timeout startup reaches recovery rather than returning Err;
  resync preserves the peer and old cache; scheduled-wait deltas remain fenced; event-ready queues cannot starve
  a due retry; keys/prefix/quit remain available; delay stays capped even at u32::MAX.
- CLI red/green: optional client-list refusal was swallowed; old diagnostics waited60s. Tests now cover immediate
  typed refusal without retry, short status/snapshot deadlines, and existing closed-stdout plumbing.
- Exact reviewer-shaped private pressure:3MB entries,6 concurrent snapshot callers,5 sequential TUI starts.
  All5 remained alive and quit normally;2 visibly showed core busy;122 caller snapshots included10 overloaded.
- Physical private mock endpoint/TUI:17/17 checks, startup busy→ready, resync without a second hello, usable prefix,
  and atomic replacement of a disposable binary→reconnect→actual re-exec→busy→ready with the same UI pid.
  `/proc` confirms the running image is the replacement inode. Quitting while admission remains refused is responsive. Only read/subscription requests repeated.

Receipts, raw/captured screens, scripts and red/green logs:
`<private-notes>`. Full-gate outcome is in the task closeout report/receipt.
All tests use private state/endpoints, env-i,8CPU/nice10/offline-j8. No live install/session/backend effects,
credentials or model calls; physical copy/endpoint fixtures were removed after cleanup.
