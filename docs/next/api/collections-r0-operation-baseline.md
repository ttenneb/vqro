# Collections R0 operation baseline

Status: **frozen pre-R1 baseline with an instrumented post-R4 comparison** at runtime revision `099239da`.

Machine-readable counts: [`collections-r0-operation-baseline.json`](collections-r0-operation-baseline.json)

This is an instrumented ownership/dispatch comparison, not a wall-clock benchmark. The test creates
verified component-package factories against separate Wasmtime Engines, starts real Stores and
Instances, invokes changed scopes, and repeatedly consumes cached projections for the configured
pane/client/frame dimensions. Counts follow the current implementation boundaries:

- every installed `ComponentRuntimeFactory` owns one Wasmtime `Engine` and one compiled `Component`;
- every active component generation owns one `Store` and one `Instance`;
- provider work is scheduled for changed provider scopes, not attached clients or composed frames;
- frame composition consumes App-owned cached projections and has no extension-service call path;
- client multiplication changes only cached projection fanout;
- one cold-capacity scenario constructs 100 factory slots without active generations;
- stale or canceled completion may attempt reconciliation once but publishes zero results.

The scenario set intentionally is not a Cartesian benchmark matrix. It covers 1/15 services and
scopes, 1/15 panes, 1/3 clients, one unrelated-scope invalidation, and the 100-service cold smoke.
The component-runtime probe reports the same named counters for every scenario, including the
100-service cold-capacity smoke. R5 may add timing, memory, lazy activation, Engine sharing, or
compiled-component cache benchmarks.

## Acceptance interpretation

The baseline is healthy only where cardinality is independent of panes, clients, and frames. It also
records the current expensive dimension honestly: factories own separate Engines and compiled
Components, and active services own separate Stores and Instances. R1 must not increase those counts
per tab, view, render pass, or client.

Engine sharing is not implied by this baseline. The current engine-wide `increment_epoch()`
interruption design must be replaced by safe domain ticking and per-Store deadlines before Engines
can be shared.

## Evidence commands

The contract test checks the frozen cardinality invariants. The runtime test compares those counts
with observed factory, Engine, compiled Component, active Store/Instance, invocation, and cached
fanout counts. Existing stale/cancel tests retain the one-attempt publication bound. Run both with:

```bash
ZIG=$HOME/.local/opt/zig-0.16.0/zig cargo test --locked \
  collections_extension_contract::tests::r0_operation_baseline_has_bounded_cardinality \
  -- --exact
ZIG=$HOME/.local/opt/zig-0.16.0/zig cargo test --locked \
  extension_runtime::component::tests::collections_operation_baseline_matches_instrumented_component_topology \
  -- --exact
```
