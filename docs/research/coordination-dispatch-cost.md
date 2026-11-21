# Coordination dispatch cost: eliminate repeated work inside one scan

## Existing observation, not a new runtime guarantee

Prim-scale's read-only ac140ac7 RCA found native-invocation gaps growing with retained history in both91ab50f and
ac140ac7: about0.04→0.32s near1000 records at low load, with9.1s gaps under full-run host load. Low-load I10 passed
1280 invokes in236s. Source: shared `program/impl/rca-part-b-regrade-ac140ac7.md` / private runs under
`<private-notes>`. This was not a batch1/2 regression or an unconditional latency SLO.

## Local cause and small fix

At mainfc916ddd, `claim_one` deep-copied all message history once, then `_eligibility` repeated that full copy and
recomputed unknown-lane heads for every pending candidate. It also rescanned the domain for frozen archive keys
per candidate. Pull `take` repeated the same work. Thus one scan had avoidable O(pending×retained) work on top of
its normal bounded-domain cost.

The fix computes heads and frozen keys once per existing locked claim/take snapshot and discards them after the
scan. It adds no cross-call cache, index, persistent pool, authorization shortcut or quota-accounting approximation.
Attention-derived unknown ordering and archive selection rebuild on each scan. Resource facts, rights, claim CAS,
step durability and pre-native authorization checks remain on their existing current paths.

## Adapter-only measurement

Private stdlib driver: `<private-notes>` (name is historical; this is not
an I21 rerun). OperationalStore with a private MemoryStore RPC fixture, synthetic unknown evidence on another
endpoint,1 or8 pending candidates,3 unprofiled `claim_one` samples plus a cProfile sample. CPU16–31/nice10, cleared
environment, no core/runtime/native execution. Raw files are
`<private-notes>,after}.json`.

| Retained history | Pending | Before median ms | After median ms | Before full-history copies | After copies |
|---:|---:|---:|---:|---:|---:|
|0|1|0.27|0.26|2|1|
|0|8|1.23|0.70|9|1|
|256|1|7.10|5.90|2|1|
|256|8|15.34|6.50|9|1|
|768|1|21.18|17.33|2|1|
|768|8|42.77|18.49|9|1|

At768/8, the instrumented baseline profile spent0.126s of0.163s in the9 `records()` calls (profiling overhead means
these are not wall-clock medians). Store view calls fell20→4. Deterministic unit tests assert one history copy per
native/pull scan, not host-sensitive time thresholds. Further tests cover attention changes, archive abort and
rights revoked after claim before native effect.

## Remaining cost and limits

Per-delivery work is STILL O(retained history/encoded domain): exact reservation/byte accounting, bounded history
processing and the owning backend's durable transaction work remain. At768/1 the adapter-only gain is modest;
this does not establish elimination of prim-scale's long-gap/load behaviour. The original prim-scale evidence,
this tiny synthetic3-sample CPU benchmark and real-core public correctness tests are distinct populations.
No new p99/runtime/control-latency declaration or whole I10 regrade follows from this change.

Changing persistence, caching quota totals across mutations or dropping retained evidence to promise O(1) would
be a larger design/qualification decision, not part of this small release. Current bounds, unknown-ordering,
archive exactness and no-replay semantics are unchanged.
