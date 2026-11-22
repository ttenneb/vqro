# Final fuzz P2 fixes and scoped launcher reads (2026-11-18)

Implemented on `final-fuzz-p2`, based on `a5ec030`. This covers the assigned half-close, command lifetime/bounds,
explicit undo target, scoped Pi launch and spec-drift items. Depth/accept-limit and TUI key/snapshot findings are
separate streams; this document does not claim those fixed.

## Read EOF is not response shutdown (D10)

The old RPC reader set the common shutdown flag on EOF, aborting the writer even for requests already read or
admitted to durable IO. The baseline pipeline got zero replies; an injected durable-write gate proved adoption
could finish after the reply channel was closed locally.

The core's serving transport now distinguishes clean read EOF from write shutdown. It drains already-read valid
requests under the existing per-connection/admission limits, waits for their handlers, flushes queued replies,
then closes and runs connection cleanup. Handler tasks are owned/reaped in a JoinSet, not retained forever on a
long-lived connection. Once input ends, new outbound callbacks fail without sending: that peer cannot answer.
Ordinary client-side Peer EOF behavior is unchanged; the serving application explicitly owns the drain lifetime.

**This is not a delivery acknowledgement.** A client must keep reading after its write half closes. Genuine reset,
write failure, framing/queue failure or core shutdown can still make effects indeterminate; inspect state/history,
never replay automatically. No effect is undone to compensate for lost transport. Subscriptions, interactive streams
and command owners retain full-duplex connections, rather than requesting one-shot EOF cleanup.

`tests/half_close.rs` fails both cases before the fix and passes after: 100 pipelined distinct-key writes get all
replies exactly once; EOF while a durable writer is deliberately blocked still gets that admitted transaction's
reply/history once. An RPC unit check proves the serving writer remains usable but new callbacks are rejected.
Existing commit-off-lock and shutdown-drain tests pass.

## Connection-owned commands cannot become lifetime history

Disconnect removes only commands still owned by that connection; it cannot remove a replacement connection's
registration. The existing `command {available:false,...}` event withdraws the row. The TUI removes that cached
name too, rather than retaining unavailable historical names. Built-in recovery commands are unchanged.

Live command JSON (names/titles/owners, brackets/separators, including built-ins) is bounded to 1 MiB. A single row
that cannot fit is `too_large`; aggregate pressure is `overloaded` with no map change. Replacing a name releases
its previous charge in the same locked admission. Registration is infrequent: counting the already-bounded map
avoids another mutable accounting index. This protects command snapshot metadata, not every other metadata source.

`tests/command_hygiene.rs` fails on the old retained names/unbounded metadata and passes after. It checks 100 owner
lifetimes, encoded NUL expansion, rejected third registration, replacement credit, single-row rejection and a
bounded successful snapshot. Existing same-author replacement/old-disconnect tests and a TUI cache test pass.

## Explicit undo targets are not defaults on parse failure

`prim.revert` permits an omitted target for default/latest selection. A supplied `txn` must be an unsigned JSON
integer; strings, floats, negative values, booleans, objects and explicit null are invalid before undo selection.
`tests/revert_params.rs` reproduces the old string target reverting latest; fixed behavior leaves both transactions
unchanged for every bad type and still honors a valid explicit older target. D24 attribution/default undo is intact.

## Pi launch uses scoped reads; spawn is never retried

`pi.new` reads its exact settings entry with `workspace.get`, source cwd via `resource.list {id}`, and the newly
created host's agents via `resource.list {host}`. Optional generic id/host filters intersect the existing
provider/backend_ref filters before cloning/serialization; malformed types reject, unfiltered callers stay the
same. A genuine token-bound runtime test verifies host scoping; no test-only authentication bypass was introduced.

The old full-cache refresh/poll at 10 Hz is gone. Only scoped status reads retry after transient errors, inside the
existing nominal wait window (normal RPC deadlines still apply). Expiry/permanent read failure returns the already
observed host and terminal fallback with a do-not-blindly-retry warning. A failed agents UI call also preserves
the observed host/agent result. The one spawn effect is never resent. Python regressions fail before/pass after,
including source cwd, no fresh(), transient lookup, deadline and post-spawn UI failure.

## Spec corrections, not native input emulation

The living specs no longer promise all providers deliver queued input after an output gap. The core's ordered
input binding and `dispatched` result are not native-consumption acknowledgement. prim-tmux drains until ordered
close (bounded 30 s); prim-herdr's pump can close its native attach on gap/end, and Python SDK input can end then too.
That is documented honestly, without hidden reattach/raw-input emulation or replay. Evidence is the existing
`src/bin/prim-herdr.rs::pump`/adapter close paths and final re-review's private gap probes (no new native guarantee).
The stale `prim.shutdown` implementation-pending text is removed: it is the implemented ordered core shutdown.

## Verification / isolation

Focused complete cargo tests and 208 Python tests pass; the authoritative release gate is `scripts/check.sh full`.
Local red/green evidence is under `<private-notes>`: `half-before.log`, `half-green.log`,
`commands-revert-before.log`, `revert-before.log`, `pi-before.log`, `scoped-before.log`, `rust-focused.log`,
`scoped-green.log`, `python-focused.log`, `cargo-first.log`, and the final full receipt. Tests use private fake
backends/cores/state or the established private native fixtures, env-clear processes and no model calls. No live
sessions, installation, push, ownership grants, new backend/runtime transport or effect replay.
