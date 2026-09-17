# `host.state.v1` (staged)

`host.state.v1` is staged infrastructure for package-runtime conformance. It is **not generally available**: production package activation does not yet issue host-state leases. Legacy services and package preflight receive no host calls. Read-only conformance invocations may receive `state.snapshot` together with `terminals.snapshot` through one atomically staged package-bound context. That composite context never grants `state.transact`; write conformance remains on a separate staged path.

The contract wraps the existing `DurableExtensionStateV1` authority; it does not create another store.

## Methods and exact grants

| Method | Required manifest-v2 capability |
| --- | --- |
| `state.snapshot` | `host.state.read` |
| `state.transact` | `host.state.write` |

Write does not imply read. Calls are bound to verified package/catalog namespace ownership and to a revocable invocation lease. A transaction uses store-generation and namespace-revision CAS plus an idempotency key. `set` and `delete` are distinct operations, so a `set` value may be JSON `null`.

Transactions are acknowledged only after durable persistence. If a caller cancels or disappears after persistence begins, persistence still decides authority; an idempotent retry reconciles the result. A failure after replacement blocks subsequent reads and writes until restart or repair.

The generated schema is [`host-state-v1.schema.json`](./host-state-v1.schema.json).
