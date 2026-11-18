# Failing-provider contention and stale agent retention (2026-11-17)

Baseline `28042db`; private release tests, eight-core affinity, nice10. No live backend, model calls or user sessions.

## Attribution

`tests/core_contention.rs` profiles 400 runtime providers, 600 terminal facts plus 400 agent facts, and about2MiB
of workspace entries. Repeating100 reconnect/sync/snapshot/disconnect cycles measured:

| Core mutex work | Before p99 | After p99 |
|---|---:|---:|
| Registration lifecycle |1.882ms|0.102ms|
| Authoritative unchanged sync |1.343ms|0.086ms|
| Snapshot construction / immutable capture |23.579ms|0.634ms|
| Disconnect lifecycle |0.241ms|0.113ms|

After capture, snapshot materialization is outside the mutex (p99=12.164ms in this fixture). This is a lock-hold
comparison, not a claim that capture alone performs all snapshot work. Large entry/fact/live JSON is shared until
materialization; captured availability, provider metadata and epoch/sequence remain immutable. Provider-local
identity/reference indexes remove global resource scans; unchanged registrations/inventories avoid cloning the
entire identity map. Actual identity changes still use one durable map replacement.

Credential rotation was separately measured at400 hosts/800 credentials: p99=0.281ms, max0.450ms. It was not the
dominant measured cost, so this fix does not introduce another persistence actor. Credential and identity IO
remain synchronous; slow storage is a remaining independent concern, not disproved by this profile.

## Real RPC storm regression

`tests/core_storm.rs` starts a private fake core with a >1MiB workspace,600 reported resources and400 registered
providers. They repeatedly request snapshots their1MiB envelopes cannot carry while an ordinary client probes
control and terminal echo. Baseline binary: healthy `hello` exceeds its5s deadline. With immutable capture,
limited preflight and separate bounded construction pools:613 requests rejected, healthy control max0.176ms,
echo max0.074ms in the first release execution. Both250ms guards remain unchanged. Shared-host timing is
observational; the frozen baseline binary and logs are retained under the task scratch root.

Two construction slots serve ordinary recovery clients and two serve smaller provider envelopes; admission waits
at most250ms. Capture follows admission, so queued requests retain no large snapshot. Slots remain with blocking
work even if the request is cancelled. Preflight stops before constructing oversized JSON. Eight registration
slots bound pending host claims/registration work without consuming healthy control or stream slots.

## Why stale objects accumulated

Existing retention selected only available finished resources; disconnected live agents were never candidates.
Native `resource.forget` also refused live/unavailable resources, so identities stayed active and runtime provider
reclamation could never become eligible. Reconnects were not proof of resource disappearance.

The separate generic `resource.forget_cached` primitive now explicitly retires unavailable cached observations;
it never calls a provider, kills a terminal, or claims a native forget. Normal `resource.forget` is unchanged.
Available resources reject it, protecting reconnect races. The retention extension supplies the60-second continuous
unavailability policy for agents; reconnect cancels the clock. No expiry policy is added to core. Mechanical count
and byte bounds remain when retention is disabled. The assigned60-second rule is not attributed to D23, whose
current text concerns multi-machine contracts.

Retention prunes only unreferenced bridge-marked orphan fallback panels. Agents prunes its own unreferenced
panels/actions and clears their live content; referenced views and user panels remain. Tests cover60-second expiry,
reconnect cancellation, explicit finished-only command safety, ownership/reference-safe cleanup, immutable snapshot
fencing, bounded registration admission,200 cached retire/reap cycles and monotonic resource identities.

Evidence: `~/.local/share/herdr/scratch/prim-core-contention/` (`profile-before.log`, `profile-after.log`,
`credential-before.log`, frozen `baseline-prim`, `storm-red.log`, `storm-green.log`, `retention-red.log`).
Protocol: `docs/spec/core-protocol.md`. Full gate includes `core_storm`; the standalone profile is opt-in release.
