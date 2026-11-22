# Native hc terminal drift: byte framing and creation ordering (2026-11-17)

Implemented on `herdr-drift`, based on `6f28c53`. This repairs the soak's P1 drift without a periodic reconciler,
backend-specific core state rule, new provider transport or effect replay. Backend execution/lifetime stays owned
by hc. `docs/spec/core-protocol.md` and provider-v0 section 9 describe the updated mechanism.

## Cause 1: DATA frames are not event records

The pinned hc 0.1.21 schema says `SubscribeResult.channel` carries **JSON lines with credit**. The old adapter parsed
each DATA payload with one `serde_json::from_slice::<Value>` (`src/adapter/herdr.rs` at baseline 6f28c53). Under load hc
coalesces several notifications into a frame; a byte-stream record may also span frames. A multi-record or partial
payload was silently discarded, with no CREDIT returned. This loses created/finished/removed observations and can
stall delivery. It is not evidence that hc failed to emit the events.

A transparent frame tap around a **private** real hc connect reproduced it with 600 concurrent one-second shells:
- 302 DATA frames held 2–20 JSON lines and failed the old single-document decoder.
- Native inventory settled at 500 finished terminals (hc automatically prunes older ones).
- prim stayed at 600 terminals, with 543 phantom live rows and 100 pruned-but-unretired rows. The states remained
  wrong for every comparison after the programs ended. No reconnect was forced.

The shared native adapter now incrementally decodes NDJSON, with a 16 MiB unfinished-record bound matching its
native frame bound. Every complete notification in a frame is applied. It returns each frame's exact payload
charge once, after the existing FIFO consumed barrier; accepted bounded partial records also earn credit, so a
record larger than the credit window cannot deadlock. Malformed/oversized input fails the link visibly rather
than silently losing facts. No unbounded decoder or extra effect queue was added.

hc already offers `missed` with a fresh snapshot. Subscribe/`missed` `seq` establishes a cutoff: older queued events
must not undo it (explicit in the schema's `SubscribeResult.seq` description). The adapter now enforces that
cutoff too. No timer/resubscribe workaround is necessary for the observed drift.

## Cause 2: an initial spawn reply can roll a newer observation back

After decoding was fixed, 600 immediate-exit shells exposed 78 phantom live rows, with native and prim inventories
otherwise both at 500. The effect response described creation, but `resource.spawn` unconditionally upserted those
initial facts after an independently published finished event. The asynchronous response/notification paths can
complete in either order.

`Core::record_spawn` now reuses an already-published exact `(provider, instance, ref, incarnation)` and its current
facts; only an unknown identity follows ordinary durable admission. This is generic creation-result dedup, not
"finished wins" policy: the wire regression also preserves newer metadata on a still-live resource. Different
incarnations do not match. The wire adapter's current/partial-sync cache observes the same rule, so a late creation
reply cannot replay stale metadata at the final chunk. A deterministic interleaved-sync regression catches that
second cache path too. Normal resource updates, kill observations and effect outcome semantics are unchanged.

## Red/green tests and target load

- Strengthened `tests/fixtures/event_credit_backend.py` sends coalesced/split records under a finite byte window,
  followed by `missed` and a stale created event. The old decoder times out. The fixed
  `tests/herdr_event_credit.rs` waits for **all exact bytes** to be credited, then asserts the finished snapshot
  survives. Holding the core lock still prevents premature credit. This proves decoder/credit/sequence behavior.
- Unit tests exercise every possible split (including inside UTF-8), malformed and oversized partial records,
  plus snapshot/old-event sequence fencing.
- `spawn_completion_does_not_roll_back_newer_published_facts` in `tests/provider_wire.rs` publishes newer facts
  before answering spawn with creation facts. Baseline returns live instead of finished; fixed behavior preserves
  finished state and newer live metadata, including after an interleaved final sync chunk.
- `tests/e2e_herdr_drift.py` is a permanent **full gate** against real private hc and the actual prim-herdr binary.
  Three waves of 600 terminals (`sleep 1`, `sleep .01`, immediate exit) compare hc and prim inventory/facts, exercise
  native automatic pruning, verify 1800 distinct prim ids, then explicitly forget three finished resources.
  All waves converge to 500/500, then 497/497. The provider generation stays 2: **zero reconnects**. Focused timings
  were 3.917 s /7.028 s /7.488 s. This is an 18-second controlled load, not a claim of repeating the two-hour soak.
- Targeted Rust validation: 129 library, 16 core, 7 providers, 6 wire (native tmux separately opt-in), 3 native input and
  the consumed-credit integration test passed. The release authority is `scripts/check.sh full`, including the new
  `e2e-herdr-drift` step; use its final receipt, not focused results, for merge acceptance.

Local evidence under `<private-notes>`:
`before/{frames.jsonl,result.json}`, `fixture-before.log`, `spawn-before.log`, `unit-green.log`,
`focused-final.log`, `instant/result.json`, `instant-after/result.json`, `load-final.{json,log}` and the final full
receipt/logs. The tap/probe are scratch diagnostics; the permanent tests do not depend on them.

## Isolation and limits

Real probes use env-clear private HOME/XDG/HC/PRIM endpoints and only the herdr provider declaration; no retention/UI
extension can hide native pruning drift. hc 0.1.21's default finished retention is exercised, not reimplemented.
Each fixture stops its own prim and hc and removes its temporary root; no human sessions, install, push or model
calls. Loads run at nice 10 on the first 8 available CPUs. The tests poll only to **observe convergence**; product
code has no new inventory timer. Existing resource/admission/stream bounds and native pending limits remain.
