# TUI keys: preserve evidence before diagnosing a stale label screen

The concurrent-gate incident at `be1f7eb1` reported an unknown-label fallback failure, then
ENOENT on the next core snapshot connection. Its root `/tmp/pke3jh20ehq` and original core/provider
logs were subsequently deleted. **The initial core/socket-loss cause is unresolved.** This is not
an established label-rendering defect, timeout-budget defect, or OOM diagnosis.

The separate indeterminate-kill race was fixed by `fb256965` (integrated as `f69a4911`): a kill's
own native retirement must not invalidate its successful receipt or revive the retired resource.
That fix does not establish a cause for the core/socket-loss incident.

The retained leftovers contain a later `vqro serve` without `--fake`; TUI reconnect calls
`client::ensure_core`, which can start such a replacement. However, the test's `finally` terminates
the original `serve --fake` child **before** scanning leftovers. Its absence afterward therefore
cannot establish when it exited, or exclude socket unlink while it was alive.

The keys harness now prints its root before startup, checks original-child/socket liveness,
retains failed roots, and captures diagnostics before cleanup. It retains the original log inode
so unlinking the pathname cannot erase that evidence; `pread` leaves the child's output offset
unchanged. The core logs signal versus protocol/listener shutdown. No wait budget or replay policy
changes. Controls live in `tests/test_tui_keys_lifetime.py`.

Qualification: code commit `20620b36`, clean full gate **48/49 PASS, one optional OpenTUI SKIP**,
scoped cleanup PASS, 2026-11-21 05:44 EST. An explicit private-shutdown/stale-UI fault control
produces the old label-FAIL/ENOENT sequence and confirms failed-root retention afterward; it is
**not a spontaneous reproduction or explanation of the original incident**.

Detailed logs, original errors, mutations and immutable receipt:
`<private-notes>`.
