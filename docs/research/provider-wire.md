# Core provider wire evidence — 2026-11-17

Implementation: `src/provider_wire.rs`, `bridge_tokens.rs`, `resource_admission.rs`; thin server/native-adapter hooks.
Native herdr remains the default. No live installation, essential-extension policy, relay, or safe-mode change.

## Checked

- Red baseline: `wire_registration_chunks_and_generation_fences` fails on main `dcbae2f` with unknown
  `provider.register`; same public socket test passes after implementation.
- `cargo test`: 125 passed, zero failures/warnings, 15 optional tests ignored.
- `tests/provider_wire.rs`: ordered chunked inventory; disconnect during partial sync retains stale facts;
  same-instance identity restoration; duplicate registration and stale-generation fences; dispatched/rejected/
  provider-error/deadline outcomes; late original reply ignored; stream limits, decoded data and ordered gap;
  oversized whole-facts rejection before publication; real Python SDK/fake supervised extension spawn/echo/kill.
- Runtime token socket test: pre-host registration waits, durable host assignment, agent host link, bootstrap
  retry after lost first reply, bounded current+previous rotation, old credential rejection, persisted core-restart
  resumption with the same agent id, host removal invalidation. No Pi/model process is started by this test.
- `rpc::tests::tightening_a_record_limit_also_checks_already_queued_records`: registration's stricter record
  limit also covers records queued under the old limit; original bytes are retained so whitespace cannot evade it.
- `resource_admission` unit test: per-provider counts, whole record size, global bytes (including pending facts),
  diagnostic provider name, and capacity reclaimed after removal. Native providers use the same admission.
- Explicit optional `prim_tmux_extension_interoperates_with_core_wire`: **passed** against `/usr/bin/tmux`,
  private provider-owned server only; supervised extension, real spawn/repaint/input/echo/kill/shutdown.

## External-provider scale

`PRIM_SCALE_EXTERNAL_PROVIDERS=1 cargo test --release --test scale -- --ignored --exact scale_load --nocapture`
uses the unchanged default profile: 600 terminals, 200 agent status producers, eight 1 MB/s floods, two external
Python providers, responsive/stalled/slow viewers, 20 transactions/s and extension restart churn, for 30 seconds.
All budgets pass:

| Measure | Observed |
|---|---:|
| Core RSS peak / growth | 49.88 / 35.28 MiB |
| Responsive echo p99 | 3.06 ms |
| Slow-reader echo p99 / max | 84.11 / 151.62 ms |
| Transaction p50 / p99 | 2.73 / 4.15 ms |
| Status delivered | 99.12% |
| Flood observer share | 99.33% |
| Missed events / restart failures / leaked processes | 0 / 0 / 0 |

Receipts and complete logs: `<private-notes>,tests-final,
 tmux-final,scale-external-final}.log`, `wire-scale-external-final.json`.

## Boundaries

The 32 MiB budget accounts canonical published and pending-IO JSON, not all heap/RSS or derived inventory copies.
Wire streams admit at most 512/provider, effect/open work 128/provider, effect ids 65536/connection. A full ledger
rejects new calls, never evicts and replays. Provider errors remain caller-indeterminate; only explicit rejected
results or core pre-dispatch rejection guarantee no effect. Stream offsets/credit and generation checks are core
mechanism; backend/runtime knowledge stays in providers.

Not checked here: real Pi bridge against this core, real external `prim-herdr` against hc, Windows transport,
remote relay, absolute filesystem/crash durability beyond the existing sync/rename mechanism, and loads above the
measured profile. `prim-herdr` still declares spawn env unsupported; native in-process herdr now passes env to hc.
SDK resume-result retention was confirmed separately by the bridge owner (`e8ce7b3`, `8fc91d4`), not cherry-picked.
