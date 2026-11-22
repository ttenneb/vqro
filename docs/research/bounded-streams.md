# Bounded stream validation (2026-11-17)

Scope: in-process adapters and existing JSON/base64 connections, based on `80253d3`. No new provider wire
protocol, viewer credit, event coalescing, or socket tuning.

## Mechanism

- `src/queue.rs`: semaphore-backed byte accounting, a 64-byte minimum charge for small items, and a bounded
  channel. A consumer retains the permit while handling the item. Dropping the receiver wakes blocked senders.
- Stream input and output each have a 256 KiB payload budget. Decoded input messages are limited to 64 KiB.
- Each peer has independent 4 MiB control and 1 MiB data queues, with control selected first. Accounting includes
  the current socket write. A slow stream's queued data is invalidated and an explicit `slow_consumer` closure
  uses the control queue. A partially written JSON line must finish; it is never truncated or replayed.
- Normal closure remains behind that stream's final accepted output; failure to queue the closing marker is
  reported as a slow-consumer gap instead. Control overflow disconnects rather than silently dropping control.
- Herdr frame queues are bounded at 1 MiB outbound / 32 MiB inbound, retaining the existing 16 MiB frame limit.
  Backend CREDIT is generated only after core acceptance into the bounded connection writer queue. This is not
  a claim that a viewer has parsed the bytes. The stdio credit fixture checks both sides of this handoff.
- Forwarded commands default to 30 seconds; `deadline_ms` allows 1–300000 ms. Timeout reports an indeterminate
  outcome, removes the pending call, ignores late replies, and never resends. Cancellation also removes pending
  entries. The generic Rust peer helper independently defaults to a 60-second deadline.

These are queue/payload bounds, not global RSS or transport-latency guarantees. Allocation overhead, one frame
being read before queue admission, OS socket buffers, event history, and workspace history are separate costs.

## Tests

Two public-API regressions were copied into a scratch checkout of `74823b6` (which already has the shared fake
load profile and event-resync work). Both failed before the bounded changes:

- Hung command: a 30 ms requested deadline did not answer within the test's 500 ms outer deadline.
- Stalled socket at 32 MB/s fake output: no explicit overflow closure arrived within three seconds.

After the changes, the normal suite passed with zero warnings: 44 unit tests, 13 core integration tests and one
herdr CREDIT integration test. Coverage includes byte charging while a consumer stalls, queue closure waking
waiters, reserved control capacity, final-output ordering, timeout/cancellation cleanup, late replies, explicit
slow-stream closure, unrelated viewer usability, TUI parser reset/retry without a resource event, attach/closure
races, and preservation of the existing connection-generation fence.

## Release scale result

Command (with all `HERDR_*` absent via `env -i HOME=... PATH=...`):

```
CARGO_TARGET_DIR=<private-notes> \
  cargo test --release --test scale -- --ignored --exact scale_load --nocapture
```

Default profile: 600 terminals across two fake adapters, 200 agent-like status publishers, eight 1 MB/s output
streams, 20 transactions/s, extension restarts, one draining observer, one stalled viewer, and one 64 KiB/s
slow reader. Thirty seconds of load, plus setup/teardown.

Final measurements:

| Measurement | Observed | Placeholder budget |
|---|---:|---:|
| Core RSS sampled peak | 33.86 MiB | 512 MiB |
| Core RSS growth after setup | 25.25 MiB | 128 MiB |
| Normal echo p99 | 1.95 ms | 50 ms |
| Slow-reader echo p99 | **1102.86 ms** | **250 ms — failed** |
| Slow-reader echo timeouts | 0 | — |
| Status event lag p99 | 1 ms | 250 ms |
| Final snapshot | 4.80 ms | 1000 ms |
| Leaked processes | 0 | 0 |

All other scale budgets passed. **The scale command exits unsuccessfully because the slow-echo placeholder
fails; this is not an all-green scale result.** The parent approved shipping that limitation explicitly and
tracking per-client event subscription filters and/or status coalescing separately. Status volume is roughly
40 KiB/s against a 64 KiB/s reader; queued socket bytes and synchronized event bursts cannot be preempted by
priority among application queues. The prior FIFO-only bounded candidate still timed out slow echoes; separate
queues plus invalidation of closed-stream data removed the unbounded buildup and the timeouts.

The earlier stalled-only profile passed every budget at 33.86 MiB peak. The parent recorded the unbounded
baseline at about 520 MiB after 30 seconds. The final harness shut its core down gracefully and reported no
leaked processes; earlier harness teardown leaked two noop sleeps, which the parent removed before the fixed
teardown was rebased.

Detailed receipts/logs live under `<private-notes>`:
`slice2a-scale-final.json`, `slice2a-scale-final.log`, `slice2a-tests.log`,
`slice2a-before-timeout.log`, and `slice2a-before-flood.log`.

## OS send buffer: bytes the core cannot revoke (2026-11-17)

The bounded queues above cannot take back bytes already handed to the OS. A 64 KiB/s viewer that attaches to eight
1 MB/s floods had about 170 KB of flood payload in its connection's kernel buffer (Linux default `wmem` 208 KiB;
`ss -x` showed Send-Q about 205 KB on the core side, Recv-Q about 171 KB on the viewer side) when the core closed
those streams as slow consumers. Its first echo then waited about 5.6 s. The backlog drains at the viewer's read rate
minus the event traffic (about 37 KB/s), which the writer always sends ahead of stream data.

Bounded outbound: the core now requests a 16 KiB OS send buffer per client connection (`CONNECTION_SEND_BUFFER`,
best effort through `socket2`; Linux doubles it). The default scale profile, first echo of that viewer:

| Requested send buffer | First echo | Note |
|---|---:|---|
| OS default (208 KiB) | 5.6 s | |
| 64 KiB | 3.8 s | |
| 32 KiB | 1.8 s (3 runs) | |
| 16 KiB | 0.50-0.65 s (4 runs) | chosen |
| 8 KiB | 0.71-0.75 s (3 runs) | |
| 4 KiB | 1.8-1.9 s (3 runs) | the viewer reads only about 53 KB/s: small buffers cost it wake-ups |

Draining viewers are unaffected: 99.9% of flood bytes delivered, observer echo p99 about 2 ms. What remains, about
0.5 s, is one flood line already being written plus the events queued ahead of the echo. Getting below that would
need stream data to share priority with events, which is a different trade-off. Receipts: `first-echo-before-3697c40.json`,
`first-echo-after-35dd1b3.json` under `<private-notes>`.
