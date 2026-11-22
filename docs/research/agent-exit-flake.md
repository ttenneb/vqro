# Agent exit/gone flake: product bug, not a longer-wait issue (2026-11-17)

## Diagnosis

Main `587fa3b` failed the full gate once at `Pi exit -> resource.gone` in the agents UI E2E. The preserved
`ep-s85z9aq3` receipt shows the actual Pi/bridge terminal exited23, the provider disconnected, but r2 remained
unavailable/stale with pre-exit run facts. A longer wait cannot make a terminated producer send another retirement.

Two core tasks race after EOF. Peer has already accepted complete final notifications into bounded ingress, but
WireProvider::run observes closed first and marks routes disconnected. Connection::validate then incorrectly
rejects the queued resource.gone as stale because it also tests connected health. The application queue drains
without retiring the resource. A second deterministic regression exposed that retirement publication did not
recheck replacement ownership atomically after prior validation.

## Small fix

`8526a10` accepts a resource.gone tail from its registered generation even after EOF has closed execution routes.
At the actual retirement mutation, it checks the current provider source AND generation under the core lock.
It cannot retire a replacement. Other notifications (including upserts), requests and effects keep their existing
connected checks; EOF cannot revive resource availability. Invalid/unregistered generations still fail. No new
protocol, retry, resource timeout, synthetic completion or replay. No E2E deadline was increased.

The server uses the notification-specific validator rather than pre-rejecting that final retirement through a
second generic health check. Producer/bridge code and exit status behavior are unchanged.

## Red/green evidence

- Deterministic `provider_wire::lifecycle_tests`: current-owner gone after EOF failed stale_generation before the
  fix; a previously validated old owner could wrongly retire a replacement. Both fail before and pass after.
  A third test confirms upserts remain rejected and unavailable after EOF.
- Exact E2E contention: only the test wrapper/children and 3 owned busy Python workers pinned to CPU31. Before:
  3/6 failed with the exact gone timeout; after: 6/6 pass. Affinity of no existing/human process was changed.
  Workers have a bounded lifetime and were stopped/waited in finally. Tests remain network/state/config/socket
  isolated and use fake Pi; the installed/live prim was never invoked or changed.
- Before matrix: `<private-notes>`.
- After matrix: `<private-notes>`.
- Reproduction controller retained at `<private-notes>`;
  it runs tests/pi_provider_e2e.py --agents-ui against the private build with explicit environment allowlists.
- Exact per-trial scratch roots, timings, stderr and return codes are retained in those pressure matrices.

## Full release gate

`scripts/check.sh full` at `8526a10`: **18/18 PASS**, zero warnings, no leftover processes. Includes all unit tests,
provider/transport opt-ins, provider/agent TUI E2Es and both scale suites. Receipt:
`<private-notes>`.
Logs: `<private-notes>`.

No cutover/install, live-session effects, real model/provider calls or credentials copied. This fixes an accepted
retirement record racing transport cleanup; it does not invent retirement for a producer that never sent one.
