# Timing-guard inventory (baseline main 7d7c382)

## Boundary

Intrinsic synchronous work must not be timed with the scheduler's stopwatch. Measure current-thread CPU time, retain the existing CPU-work budgets, retain structural bounds (one durable inventory write, bounded retained records/file bytes), and keep a generous wall-clock hang backstop. CPU time excludes descheduling **and blocking I/O**; it is not an end-to-end latency claim.

The coordinator explicitly confirmed on 2026-11-20: keep genuine end-to-end SLA probes and budgets unchanged; do not invent a separate performance-admission design here. They run in the full gate's final serial phase. Serial within one gate is not isolation from other gates or machine load.

The 2026-11-22 approved contention-grading follow-up is documented in [timing-gates.md](timing-gates.md): unchanged latency budgets may be explicitly unverified under measured contention in full gates, while timing-only/exclusive runs remain strict and correctness/capacity failures always fail.

## Changed intrinsic-work assertions

Baseline references:

| Assertion | Meaningful oracle | Existing budget retained |
| --- | --- | --- |
| `src/core.rs:831-834` inventory600 | current-thread CPU under the lock **and exactly one durable identity-map write** | 150 ms CPU |
| `tests/identity_hygiene.rs:9-12` 10,000 lifetimes | current-thread CPU per identify/retire; retained map empty and file <128 bytes every 100 lifetimes | 100 ms CPU |
| `tests/identity_hygiene.rs:31-32` public inventory600 | current-thread CPU during synchronous inventory; 600 resources | 150 ms CPU |
| `tests/history_budget.rs:125-129` 500 durable 37 KiB layouts | current-thread CPU p99 (same order statistic); history/file byte bounds unchanged | 200 ms CPU p99 |

Each operation retains a 30-second wall-clock backstop. Actual wall durations are still reported with explicit wall labels. The shared helper is test-only and cannot be sent between runtime threads. It uses the already-depended-on libc `CLOCK_THREAD_CPUTIME_ID`, verified in libc 0.2.191 for Linux and macOS; it fails on clock errors instead of silently falling back to wall time.

## Changed ordering stand-ins

| Baseline | Structural oracle |
| --- | --- |
| `src/store.rs:766-784`: sleep 300ms then assert elapsed >=250ms; held-lock return <2s | release the real held file lock only from the retry callback after an actual failed acquisition; require a retry. Advance a virtual clock for the permanently-held case: exactly 20 pauses of 10ms exhaust the configured 200ms deadline. Zero-wait probe must never pause. Real production clock/sleep and retry/deadline settings remain unchanged. |
| `tests/commit_off_lock.rs:126-148`: snapshot/echo each <200ms while writer blocked | both operations must finish before releasing the durable-write gate; admitted apply must remain unfinished and the gate unreleased. Old candidate remains invisible. Ten-second observer timeouts are hang backstops, not revised performance budgets. |

## Retained end-to-end SLA guards (not CPU-work claims)

| Baseline | Guard |
| --- | --- |
| `tests/scale.rs:33-46,599-607,699-707` | spawn p99 10ms; transaction p50/p99 10/20ms; echo p99 10ms; event lag p99 10ms; final snapshot 70ms; slow-reader startup max 4500ms and steady p99/max 50ms. Lost/incomplete/missing samples remain failures as currently specified. |
| `tests/core_storm.rs:49-50` | healthy RPC and echo max 250ms under a deliberately adversarial RPC workload |
| `tests/provider_scale.rs:35` | public snapshot RPC <500ms with 600 providers |
| `tests/e2e_tui_scale.py:22,236-238` | tmux input-to-observed-screen key p99 170ms; CPU/RSS/fps limits and minimum workload-validity rates unchanged |

Replacing these with test-thread CPU would hide server starvation, intentional queue waits, I/O stalls, slow readers and display latency. No budgets have been raised or converted to optional passes.

## Retained semantic/liveness checks

These are configured deadline/termination or no-event checks, not intrinsic-work benchmarks. Their generous observer deadlines remain wall-clock backstops; negative quiet windows test absence, not minimum throughput.

- `tests/core.rs:181-189`, `tests/providers.rs:169`, `tests/pi_native.rs:128`: configured request deadlines, indeterminate outcomes, no replay and connection usability after late replies. The 500ms observer around a 30ms deadline is not a CPU budget.
- `tests/qa_recovery.rs:49`: late abandoned stream closes without backend output (300ms observer).
- `tests/test_pi_bridge.py:291-295`, `tests/test_vqro_provider.py:569-575`: end-to-end bounded close despite inherited pipes/stalled peer (1s/2.5s); terminal facts and no surviving drain/buffer asserted separately.
- `tests/pi_pull_e2e.py:298,317`: print completion <15s and shutdown <5s while reconnecting. External runtime lifetimes cannot be measured with test-thread CPU. These are not invoked with real models for this work.
- `tests/tmux.rs:316`: flood completion within a 20-second hang backstop.
- `tests/test_check.py` held-slot grading control: private admission metadata replaces the caller's environment, and the stub remains alive until the actual sampler has retained a snapshot. Its deliberate budget-breach workload starts afterward; a 30ms process lifetime alone is not a sampler-readiness barrier.
- `tests/commit_off_lock.rs` escaped-note byte admission: hold two admitted writes and observe the third request's actual wire rejection or forbidden third admission before releasing capacity. No SDK rejection-latency deadline is asserted; the 30s arm only detects a hung response. A client observer timeout is not evidence that commit byte accounting rejected the request.
- `tests/test_coord_harness.py` silent-peer call: the configured 0.5-second socket deadline is checked with a virtual monotonic clock and a recording socket (elapsed setup leaves only 0.25 seconds for receive). The real silent peer must observe client EOF, exactly one hello and no leaked resources before its 30-second hang backstop. The removed 3-second stopwatch included `gc.collect()` and descheduling; it was not an end-to-end SLA. CPU time would also be wrong here because it excludes the blocking receive. The separate dripped-reply total-deadline guard is unchanged.
- Short no-message/barrier windows: `src/tui/mod.rs:4331`, `tests/provider_client.rs:27`, `tests/codex_requests.rs:124`, `tests/commit_off_lock.rs:256,370`, `tests/qa2_flush.rs:13,18`. Loaded scheduling cannot make a genuinely absent message appear; ordering/buffer ownership are the asserted facts.
- Lower-bound backoff checks in `tests/test_pi_provider.py:647-648,697-698` ensure retries do not run too early. Other retry/jitter/frame-budget tests use supplied/mock timestamps or sampled delay values, not execution time: `tests/test_vqro_ext.py:505`, `tests/test_pi_provider.py:809-810`, `src/tui/frame_budget.rs:20-24`, `src/tui/mod.rs:4205,4416-4417`.
- Startup/readiness, bounded socket reads, process exit, registry drainage and event receipt use wall-clock observation backstops across the Rust integration suites and Python E2Es. These remain necessary to fail hangs. Production timers, wire `deadline_ms` values, epoch timestamps, telemetry-only duration prints, memory/byte limits, and synthetic percentile arithmetic are not wall-clock performance assertions.

## Search evidence

The complete baseline candidate references (including every explicit Rust `timeout`/deadline assertion, Python timing assertion, and SLA budget) are preserved under:
`<private-notes>,assertion-candidates}.txt`.

Reviewed `src/` and `tests/` for `elapsed`, `Instant`, `Duration`, `timeout`, `deadline`, Python time getters (including event-loop time), comparison assertions, named percentile/latency budgets and shell await/check call sites. No TypeScript timing assertion was found under these directories. Telemetry-only clocks in `core_contention`, `bridge_tokens`, `latency_typing`, `e2e_scale_herdr`, sidebar/item-stream probes and prompt-latency reporting do not independently gate on a measured wall latency. Resource/workload rate checks in scale probes remain in the genuine end-to-end category.

Negative-control results and final check receipts are recorded in the task's external report, not asserted from inspection alone.
