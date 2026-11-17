# Shutdown and native herdr reconciliation — 2026-11-17

`core.shutdown`, built-in `prim.shutdown`, and Unix signals share ordered cleanup. The accepted reply uses a local
write/flush barrier; no remote receipt is promised. New dispatch/restarts stop, extension lifetimes close with
500 ms grace before kill/reap, provider transport workers stop, clients close and endpoint unlinks before serve
returns. Backend-owned terminals are not killed. Cleanup timeouts are logged as unconfirmed. `prim stop` uses RPC
without needing a pidfile; only no-answer falls back to a validated positive pid. A protocol refusal never signals.

Native `terminal.kill` replies are authoritative finished facts when the incarnation still matches the adapter's
record. This fixes the missing-finished-event path with an attachment open; unknown numeric exit codes stay null.
Native event DATA credit follows a FIFO consumed barrier, off the session loop. `prim-herdr` acknowledges after
bounded publication; native core acknowledges after facts adoption. This is not a real-backend stall claim.

Evidence in `~/.local/share/herdr/scratch/prim-independent-review/`:
- `shutdown-before.log`: baseline f069d0c fails unknown `core.shutdown`.
- `kill-before.log`: baseline returns accepted rather than observed with open attach and omitted finished event.
- `event-credit-before.log`: disabling credit prevents the finite-window fixture reaching finished (timeout).
- `shutdown-final.log`: final full suite; `tests/shutdown.rs` covers RPC, recovery/safe mode, EOF cleanup before
  client disconnect, actual serve exit, no-pidfile CLI, refusal/no-signal, private-pid fallback and SIGTERM.
- `tests/herdr_kill.rs`, `tests/herdr_event_credit.rs`: fixture-backed reconciliation and consumption credit.

No real hc, live prim or model process was used. Off-lock transaction work and terminal.input invoke follow-up are
separate assignments. Existing workspace apply paths are unchanged.
