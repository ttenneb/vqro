# `host.terminals.v1`

Status: staged, read-only package-runtime contract. Production package activation does not issue terminal leases. Legacy services, package preflight, and uninstalled runtimes receive no capability.

The sole capability is the exact string `host.terminals.read`; its sole method is `terminals.snapshot`. Calls require `{ "contract": "host.terminals.v1" }` and are bound to one stable workspace ID and opaque `TabId`, plus the package, service, artifact, catalog, provider, and runtime generations captured by the host-created invocation lease. Read-only conformance invocations may combine this scope with package-bound `state.snapshot` in one atomically staged internal context; the shared router still permits only exact depth 1, one concurrently outstanding call, and the bounded total call count.

A successful response is one coherent App-actor snapshot. It contains only workspace identity, tab identity, ordered tiled-terminal or neutral-container layout items, stable `TerminalId` values, neutral container identities, and the lease fence. `PaneId`, collection labels, archive state, agent state, status, policy, UI state, and client state are not exposed. Empty containers and ungrouped tiled terminals are representable. Every terminal in the scoped tab must occur exactly once or the call fails as malformed host state.

Freshness is represented by a SHA-256 fingerprint over the canonical captured payload and lease fence, not by a partially maintained revision. Snapshot data linearizes in the first App-actor capture turn. The handler then queues a bounded final App-actor authorization gate, so catalog, package, artifact, service, workspace, or tab revocation processed after capture but before that gate rejects the call. The gate does not recapture topology: a later topology change leaves the returned fingerprint identifying the earlier capture, which downstream consumers must treat as stale when fencing dependencies. After the gate, the handler rechecks shared revocation authority, cancellation, and deadline immediately before returning. Changes after the authorization gate are not retroactively fenced.

Snapshot construction occurs only for an admitted host call, never in render or layout computation. Current terminal/container admission bounds are checked before output allocation, and the encoded response is limited to 64 KiB.

The generated schema is [`host-terminals-v1.schema.json`](./host-terminals-v1.schema.json).
