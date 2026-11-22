# Bridge startup above the provider snapshot envelope (2026-11-17)

Branch `bridge-snapshot` from `28042db`; tested implementation `9bffb37`.

## P0 cause and scoped fix

The Pi bridge used `workspace.snapshot` after registration/sync to discover one resource id and inspect one
view declaration. A provider connection has an intentional **1 MiB** record cap; a target-scale workspace can
exceed it. Retrying rebuilt/refused that same aggregate snapshot repeatedly.

The approved generic primitives are now:

- `resource.list {provider?, backend_ref?}`: exact AND filters, applied before record materialization. With no
  filters the old complete-list behavior remains. The bridge requests only its own provider and `agent` ref.
- `workspace.get {key}`: the single `{version,value}` entry or null. The bridge reads only `view/agent-rN`.
  Normal entry/reply caps still apply; this is not a raised provider record cap or a partial snapshot pretending
  to be complete. Any client/extension/provider can use both reads; protocol documentation records them.

Matching resource clones and the entry Arc are captured under the core lock, then JSON is built outside it.
The resource clone naturally becomes cheap when the separately owned Arc<ResourceFacts> contention work merges.
No provider register/sync/disconnect logic was otherwise changed for these queries.

The bridge never requests the aggregate workspace. It checks its owned marker, declares with expect:0 when
absent, and resolves a declaration CAS race by reading just that entry again. Foreign/colliding views are never
overwritten. An oversized/unreadable view reports bounded `view_error` while the agent controls/terminal remain
available; it does not cause another register/sync loop.

## Bounded retries and the safe-mode failure found by the new test

Reconnect uses exponential equal-jitter windows: initially 0.125–0.25s, growing to a strict 15–30s maximum window.
A briefly successful/flapping link does not reset it; at least 30s of healthy connection is required. No native
input/effect is replayed. Tests cover the jitter floor/cap/spread and three accepted-but-flapping connections.

The first large-workspace full gate exposed a real refusal-classification bug: a retry during safe mode saw
`forbidden` and the definitive-refusal policy stopped its owned Pi (private terminal exited1). Larger ordinary
client snapshots made the safe-mode interval long enough to expose it; this was not hidden with a timeout.

The approved fix adds only `data.reason: safe_mode` to the two existing safe-mode admission guards, retaining
`forbidden`/`outcome: rejected`. The SDK preserves structured error data; the bridge backs off and keeps its Pi
only for this explicit temporary reason. Other registration/auth/registry refusals still stop the unreachable
runtime. No message-text parsing or permission bypass was introduced. Safe mode still starts/adopts nothing
without permission, and an owned Pi's native work/input is not restarted or replayed.

## Red/green evidence

- Rust public-wire regression builds a >1.3MiB workspace: aggregate provider snapshot still returns `too_large`
  under the unchanged1,048,576-byte cap, while scoped resource/entry responses remain <4KiB. Exact matching,
  missing entries and malformed query types are tested.
- Private real prim + herdr-core + fake Pi, with1,304,188-byte initial snapshot: staging the old bridge reproduces
  lost startup availability; the new bridge passes startup, physical TUI prompt/error/retry flow, safe mode,
  core crash/restart, owned view reuse, same Pi PID and no input replay. Final-gate snapshot size was1,304,174
  bytes (private path lengths differ). Padding remains real in assertions and is explicitly summarized only in
  polling receipts, not silently stripped from actual replies.
- Unit regressions cover >1MiB workspace without any snapshot call, oversized foreign views without repeated
  registration/sync, declaration races, explicit transient safe-mode refusal, and all permanent-refusal cases.

Final `scripts/check.sh full` at `9bffb37`: **23/23 PASS**, zero-warning build, cargo tests, **169 Python tests**,
provider opt-ins, both Pi CLI/TUI gates now run above1MiB, sidebar/history/scale/TUI checks, scoped no leftovers.
All tests used clean private environments, fake Pi only, nice10 and eight-core build/gate affinity.

- Receipt: `<private-notes>`
- Final logs: `<private-notes>`
- Old bridge red receipt: `<private-notes>`
- Final large-workspace TUI receipt/screens: `<private-notes>`
- Targeted red/green logs: `<private-notes>`

No install or human session/process was changed. Existing running bridges retain their loaded code; shipping
new core/extensions does not hot-rewrite a backend-owned bridge. New agents use the fix. Initial estimate was30
agent minutes (20 work,10 tests); actual about67 (45 work,22 builds/tests), including the approved safe-mode
lifetime fix and repeated full gate. All owned test processes were stopped; small evidence remains for review.
