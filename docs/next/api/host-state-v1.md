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

The normative document dependency for a complete successful snapshot has contract `host.state.v1`, revision `snapshot.revision + 1` (so namespace revision zero remains representable by the nonzero dependency field), and generation `snapshot.store_generation`. Its scope ID is `state_<digest>`, where `<digest>` is lowercase SHA-256 over the exact domain bytes `vqro.host.state.document-dependency.v1\0`, followed by the store ID and namespace in that order. Each string is encoded as an unsigned 64-bit big-endian byte-length followed by its UTF-8 bytes. The constructor rejects a wrong contract, invalid store/namespace identity, zero generation, or revision overflow. Sequence and values are intentionally excluded from the tuple: an audit consumer must compare the complete observed snapshot with both the staged and final current snapshots.

This dependency constructor and test-only composite audit are staged only; they do not activate production document invocation or state reads.

The generated schema is [`host-state-v1.schema.json`](./host-state-v1.schema.json).
