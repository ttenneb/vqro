# Protected operational persistence: C1

The P1 prototype (`proto/inbox-seam` atb26cf0a) established the safety seam and full-file writer cost. C1 implements a generic, separately persisted operational domain on the SAME committer: no inbox schema, dispatch policy or execution ownership moves into core. The API and exact limits are in `../spec/core-protocol.md`.

## Mechanism and invariants

- One extension-chosen domain per verified supervised owner, with the existing generation fence at dispatch and irreversible writer admission. Already dispatched work adopts/reports after retirement; queued stale work rejects. Readers do not need the owner to be running.
- Domains are outside workspace history. Ordinary apply/revert rejects protected keys before CAS/mutation, including old deletes with absent after_version and a mixed revert that has already prepared an ordinary edit. There is no raw history:false escape hatch or implicit ordinary-doc import into authority.
- Files use the existing atomic durable writer. Clean failure adopts nothing; durability_unconfirmed adopts/publishes the actual candidate and carries domain/txn/epoch/seq. Neither uncertainty nor successful metadata publication authorizes replaying a native effect.
- Cached immutable serialized records give exact delta accounting and avoid encoding unchanged large values. They do not make writes incremental: every commit still replaces a complete domain file. One worker serializes both files.
- Normal admission retains128 requests/8MiB; operational-normal is additionally80/4MiB. A separate16/1MiB control lane permits at most four ready controls before ready normal work. These bounds protect headroom and finite service, not latency guarantees or filesystem-call preemption.
- Both operational list and snapshot select provider versus ordinary recovery construction pools by the authenticated connection type, just as workspace.snapshot does. Public provider reads must not consume ordinary recovery slots.
- Registry metadata holds only a weak process-lifetime lease. Active candidates hold the strong lease through writing; idle/adopted domains cannot pin a stopped core's workspace lock.

A domain's charged bytes are **encoded records +64KiB fixed header/audit reservation**, inside its declared budget. Normal growth also preserves at least10% byte/record headroom. Any coordination capacity example must subtract BOTH charges. At2MiB the coordination example therefore has only about10 maximum/92 small open messages, not12/108; these are upper-bound examples, not admission guarantees. K2 must preflight against returned measured limits and reserve its own late outcomes/tombstones.

## Permanent evidence

`tests/operational_domain.rs` exercises real supervised authentication, replacement/stale ownership, separate files/history, mixed/protected mutations and snapshot/page/event fences. `tests/operational_aba.rs` retains the reviewer's original legacy-delete ABA/mixed-revert fixture and verifies entries/history/persisted bytes remain exact. `src/operational_read_tests.rs` deterministically checks both directions of pool isolation for list/snapshot. Operational and committer unit tests cover exact encoding/accounting, count/byte reserves, aggregate capacity, growth/no shrink, corruption retention, lifetime locks, dual-lane headroom/fairness and queued versus dispatched retirement.

The full gate's `operational_crash` suite runs the real product API plus explicit test-only durable-write fault gates (`examples/operational-test-core.rs`), a real supervised owner and a fake native ledger recording EVERY invocation. Its cases include:

- Kill before claim: pending held,0 native calls; durable claim before receipt: unknown,0; after receipt before invoke: unknown,0; after native before result: unknown,1. No restart replay.
- Pre-rename/error/unconfirmed claim receipts never invoke; settled retention/high-water fencing survives restart and cannot recreate retired dispatch ids.
-60 edit/withdraw/claim CAS races have one winner/no duplicate native effect;520 operational writes preserve ordinary history.

The independent review found one P1 pool-routing defect, repaired at6cfd87c; the reviewer accepted the delta and independently passed171 library +1 ABA +3 RPC tests. No guard or deadline was relaxed. Full scoped gate at6cfd87c passed35/35 with zero compiler warnings/scoped leftovers (341 default Rust tests passed/37 ignored, plus explicit full-gate opt-in suites;267 Python tests). Existing Python ResourceWarnings remain visible in the retained gate log. Earlier receipts remain:33/34 on pre-fix main (agents UI);34/35 on updated main (one immediate shutdown-lock reacquisition assertion). That unchanged assertion subsequently passed25 complete library repeats and the final gate; root cause of the isolated failure is unproven.

## Capacity observations, November19

One Core/committer,8MB seeded layout workspace,100 open-loop scheduled txns/s,90% operational and10% actual layouts,30s×3/configuration, affinity0–7. All latency/error/unknown samples retained. Shared-host timing is observational, not equivalence or a universal throughput promise.

| Domain and traffic | Accepted/offered | Operational p99 ms | Layout p99 ms |
|---|---:|---:|---:|
|2MiB normal,90% normal storage ceiling|9000/9000|28.51–59.78|42.22–73.15|
|4MiB normal,90% normal storage ceiling|9000/9000|31.62–35.50|41.82–48.32|
|8MiB normal,90% ceiling|8927/9000|29.55–1259.66|37.77–1271.44|
|8MiB mixed80% normal/10% control/10% layout,90% ceiling|8963/9000|210.12–1274.10|224.24–1281.41|
|8MiB all90% operational marked control,near total byte limit|8859/9000|154.92–340.95|46.80–92.12|

No unknowns, history pollution or layout admission failures in these15 runs. Mixed8MiB controls alone had p99~42.71–55.87ms despite normal queue tails; marking ALL progress as control overloads the deliberately smaller reserved lane.8MiB is supported storage capacity, **not certified100tps** here. The requested4MiB case passes9000/9000 with layout p99 in the2MiB range, supporting an explicit K2 declaration of4MiB for this measured workload. Core default remains2MiB.

2/8MiB used pre-rebase helper SHA256 `c0b13ade4e9ec7350022697a4c528fa94ea34b13c0ceb6b4733a243cc523f28c`;4MiB used rebased6cfd87c helper `e08c939b815eb48fe85ee5c22711c20a0baabe2d8f01c3ea67d9146343dab89b`. The differing base is explicit; no controlled equivalence is claimed. The ordinary writer and C1 storage mechanism are unchanged by that rebase. Driver: `scripts/probes/operational_bench.py`; replay the adversarial full/control case with `--traffic control --fill full`.

Detailed receipts, source pins, all samples and frozen pre-rebase binary live under `<private-notes>`; final handoff is `../prim-independent-review/agent-spike/program/impl/c1.md` under the scratch root. SIGKILL/fault tests are not power-loss certification. This does not implement K2 schemas/grants/scheduling, cross-domain atomic transactions, delta persistence/batching, or hierarchical resume.
