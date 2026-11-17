# Test plan: assumptions to clear up now

> **Status (2026-11-16):** **Superseded by D16** (build prim directly; answer questions inside the build). Kept as the record of what we meant to test. Still open: S1, agent input routes, now `experiments/e1-agent-input.md` and roadmap step 1. prim is a working prototype, and fluid.

Revised 2026-11-16 around D14. Evidence: `research/` (agents, backends, prior-art-tools, protocols-and-ui,
workspace-extension-models). Every test is throwaway code or no code, ends with a written result in `research/`, and
answers a question that changes the primitives.

## Assumptions at risk

| # | Assumption | Why it's at risk | Affects |
|---|---|---|---|
| A1 | Agent runtimes' native input paths fit one capability model | Claude Code has three routes (inbox socket, stream-json, PTY); pi steers at boundaries; semantics differ | agent capabilities, messages (D4, D10) |
| A2 | Backends can be adapted thinly through declared capabilities, without leaking into the primitives | Backends differ widely in events, metadata, layout and status (`spec/backend-adapter-v0.md`) | the adapter contract, lowest-common-denominator risk |
| A3 | Extension views can be expressed in a way agents can author and any client can render | No declarative standard renders in a terminal; Zellij and Neovim use hybrids | `spec/workspace-v0.md` views |
| A4 | Extensions can be replaced and rolled back live | Nobody does automatic rollback; rollback restores code, not state | extension lifecycle |
| A5 | The owner's daily work needs what prim offers across tools | Only measured by use | priorities |

## Now: no code

**P1 Usage log** (A5): for a week of real multi-agent work, note every time you relay a message between agents, wish
the workspace looked or behaved differently, or lose or mistime input, and which tools and multiplexers were involved.
This sets priorities for which primitives and adapters matter first.

## Spikes (independent, each ≤ 2 days)

**S1 Agent input routes** (A1), `experiments/e1-agent-input.md`
- Claude Code through its inbox socket, stream-json, and PTY typing; pi RPC.
- A mock model provider, so the request trace shows exactly when input reached the model.
- 100 messages per route: loss, landing point mid-turn, behaviour when busy, mapping onto D10's reporting states.

**S2 Two thin backend adapters** (A2)
- Write capability manifests and minimal adapters for herdr-core and tmux (reusing `spikes/`), exposing terminal
  spawn, input, raw output, exit and OSC 7501 status as prim events.
- One tiny extension (list terminals with status, send input to one) runs unchanged on both.
- Measure: adapter size, what had to be declared `lossy` or absent, and any place the extension needed to know which
  backend it was on. Any such place is a leak in the primitives.

**S3 View models** (A3)
- One view (the pending-requests queue) built three ways: declarative catalogue doc, ratatui program in a terminal,
  terminal program emitting widget escapes (Zellij-style).
- Two clients at 80×24 and 200×50; a scripted agent adds a button to each; Claude Code authors five small views in
  the declarative and program forms.
- Score: code size, resize, focus and key routing, whether edits need restarts, agent success rate and tokens,
  portability to a second client.

## Then: extension lifecycle (A4), `experiments/e0-workspace.md`

On the winner of S3: kill -9 loop (placeholder < 100 ms, other views and terminals unaffected, manifest bindings still
listed); state-schema rollback; late crash 30 s after load (crash budget); 5 s stall (last good frame kept).

## Scale harness (run after every foundation slice)

`tests/scale.rs`, ignored by default. It starts a real core in its own process (fake backend, two adapter
instances standing in for two hosts), loads it over the socket and writes a JSON receipt:

```sh
env -i HOME=$HOME PATH=$PATH cargo test --release --test scale -- --ignored --exact scale_load --nocapture
```

- Default load (the target floor): 600 terminals, 200 agent-like (status change every 2 s), 8 streams at 1 MB/s,
  20 transactions/s, an extension restart every 5 s, 30 s, three clients: one observer draining events and
  streams, one writer, one stalled (subscribes and attaches, never reads).
- Change it with `PRIM_SCALE_*` variables: `TERMINALS`, `AGENTS`, `FLOODS`, `FLOOD_BYTES_PER_SEC`, `STATUS_EVERY_MS`,
  `TXN_PER_SEC`, `STALLED_CLIENTS`, `EXT_RESTART_EVERY_S`, `ADAPTERS`, `DURATION_S`, `ECHO_EVERY_MS`;
  `RSS_ABORT_MB` (default 4096) stops the load early to protect the machine.
- Measures core RSS over time, spawn and transaction latency (p50/p99), input→echo p99 on an idle stream sharing
  the observer's connection with the floods, status event lag (backend emit → client), `missed` counts and
  delivered share, extension restart failures, and a final snapshot.
- Budgets are the `BUDGET_*` constants at the top of the file (placeholders until set); the test fails on any
  exceeded one. Receipt: `$PRIM_SCALE_RECEIPT`, default `<target>/tmp/scale-receipt.json`.
- The load shapes come from the fake backend's load profile (`src/adapter/fake.rs`: `--status-every-ms`,
  `--output-bytes-per-sec` in a spawned terminal's argv), so a real backend can replace it later.

Baseline on `main` 13c5561 (2026-11-16): fails on core RSS only. The stalled client's unbounded queues grow
the core from 9 MB to about 520 MB in 30 s; the same run without the stalled client peaks at 31 MB and passes.
Echo p99 3 ms, transaction p99 6 ms, event lag p99 2 ms, no `missed`.

## Deferred

Concurrent layout edits (versioned docs vs Loro); protocol compatibility gate; multi-client convergence after `missed`.

## Candidate decisions waiting on these tests

- Core protocol rules (`research/protocols-and-ui.md`).
- Commands and bindings declared in extension manifests; context keys for contextual bindings.
- Crash budget; namespace schema versions and a snapshot at `replace`.
- A size rule for one layout shown on clients of different sizes.
- A small prim widget catalogue borrowing A2UI's shape, if S3 favours declarative or hybrid.
