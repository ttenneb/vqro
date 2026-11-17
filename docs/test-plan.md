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

## Release gate: `scripts/check.sh`

`scripts/check.sh quick` builds every target with zero warnings and runs `cargo test` and the Python unittests
(`unittest discover -s tests`). `scripts/check.sh full` adds:
- every opt-in suite, including ignored tests: `tmux`, `tmux_timeout`, `tmux_provider`, `herdr_provider`,
  `provider_wire`, `provider_errors`;
- the end-to-end scripts: `e2e_providers.py`, `e2e_tui_providers.py`, `e2e_scale_herdr.py`, and the TUI smoke
  `e2e_tui_smoke.sh` (private herdr-core and prim core; 30 MB flood, core restart reattach, quit while the core
  cannot start);
- the default scale profile;
- a leftover-process check: a before/after diff of processes tied to test roots by environment or by unix socket
  path. tmux renames its server to `tmux: server`, so `pgrep -x tmux` misses it.

Every step runs under `env -i` with private paths and never touches a live prim, hc or tmux. It prints a PASS/FAIL
table and writes a JSON receipt to `$CHECK_RECEIPT` (default `<target>/check-receipt.json`), with step logs under
`<target>/check-logs/`. `full` needs `/usr/bin/tmux` 3.7b and `~/.local/bin/herdr-core`.

## Scale harness (run after every foundation slice)

`tests/scale.rs`, ignored by default. It starts a real core in its own process (fake backend, two adapter
instances standing in for two hosts), loads it over the socket and writes a JSON receipt:

```sh
env -i HOME=$HOME PATH=$PATH cargo test --release --test scale -- --ignored --exact scale_load --nocapture
```

- Default load (the target floor): 600 terminals, 200 agent-like (status change every 2 s), 8 streams at 1 MB/s,
  20 transactions/s, an extension restart every 5 s, 30 s, four clients: one observer draining events and
  streams, one writer, one stalled (subscribes and attaches, never reads) and one slow (the same, but reads
  64 KiB/s and probes input→echo on its own idle stream, behind the floods unless `SLOW_ATTACH_FLOODS=0`; probes time out after 10 s and count as 10 s).
- Change it with `PRIM_SCALE_*` variables: `TERMINALS`, `AGENTS`, `FLOODS`, `FLOOD_BYTES_PER_SEC`, `STATUS_EVERY_MS`,
  `TXN_PER_SEC`, `STALLED_CLIENTS`, `SLOW_CLIENTS`, `SLOW_READ_BYTES_PER_SEC`, `SLOW_ECHO_TIMEOUT_S`, `EXT_RESTART_EVERY_S`, `ADAPTERS`, `DURATION_S`, `ECHO_EVERY_MS`, `PARTIAL_RESOURCES` (default 1: clients subscribe as the TUI does), `STATUS_PHASE` (`random`, the default: each
  agent's first status change at a reproducible pseudo-random offset within the period; `spread`; `lockstep`, the
  worst case where all agents change together);
  `RSS_ABORT_MB` (default 4096) stops the load early to protect the machine.
- Measures core RSS over time, spawn and transaction latency (p50/p99), input→echo p99 on an idle stream sharing
  the observer's connection with the floods, status event lag (backend emit → client), `missed` counts and
  delivered share, extension restart failures, and a final snapshot.
- Budgets are the `BUDGET_*` constants at the top of the file (placeholders until set); the test fails on any
  exceeded one. Receipt: `$PRIM_SCALE_RECEIPT`, default `<target>/tmp/scale-receipt.json`.
- Cleanup: the core is stopped as `prim stop` does (SIGTERM, so it stops its extensions). Any process still carrying
  the run's `PRIM_SOCKET` is then killed, listed in the receipt (`cleanup`), and fails the test (`leaked_processes`,
  a real budget of 0). Socket and state live in a run directory under `$PRIM_SCALE_RUN_DIR` (default
  `~/.local/share/herdr/scratch/prim-scale`), which is removed afterwards.
- Abrupt core death previously orphaned extensions. Managed extensions now receive a reserved stdin lifetime
  pipe (`spec/core-protocol.md`, Extensions); EOF must exit the extension, independently of socket handling.
  `tests/ext_lifetime.rs` kills a real private `prim serve --fake` with SIGKILL/SIGTERM and verifies a connected
  helper, a helper blocked before connecting, and a non-Python stdin reader exit within three seconds. SIGINT
  covers clean shutdown. These process-observation tests run on Linux; the pipe/EOF contract is platform-neutral.
  Programs that ignore the channel remain noncompliant and may still outlive a crash; no external process
  supervisor or Unix parent-death signal is part of the contract.
- The load shapes come from the fake backend's load profile (`src/adapter/fake.rs`: `--status-every-ms`,
  `--output-bytes-per-sec` in a spawned terminal's argv), so a real backend can replace it later.

Baseline on `main` 13c5561 (2026-11-16): fails on core RSS only. The stalled client's unbounded queues grow
the core from 9 MB to about 520 MB in 30 s; the same run without the stalled client peaks at 31 MB and passes.
Echo p99 3 ms, transaction p99 6 ms, event lag p99 2 ms, no `missed`.

Follow-ups (same core):
- Without stalled or slow clients, over 5 minutes, RSS climbs about 0.8 MB/s until about 85 s, which is when the
  10k-event log fills at about 120 events/s. After that it slows to under 0.05 MB/s and is still easing at 300 s
  (51 → 60 MB). The tail is probably allocator settling, but that hasn't been checked.
- A slow client (64 KiB/s) attached to the floods: its idle-stream echo never returns within 10 s, because the
  echo queues behind about 8 MB/s of output. Core RSS grows like the stalled case: about 520 MB with the slow
  client alone and about 950 MB with stalled plus slow.
- The same slow client attached only to its idle stream: echo p50 is 6 ms and p99 is about 610 ms. Event traffic
  alone (about 40 KB/s) nearly fills its read rate. Core RSS stays flat.

Event volume (bounded streams merged, ed7599c, then partial resource events):
- The receipt's `events.wire_by_type` gives wire bytes by event type. At the default load, `resource` (status)
  events were 358 B each and 34.5 of 42.6 KB/s; `entries` 8 KB/s (the harness pads transaction values).
- With `partial_resources`, a status event is 218 B, and the total is 29.1 KB/s. The slow client's echo p99 falls
  from about 1100 ms to about 570 ms, which still exceeds the 250 ms placeholder.
- What's left is burst-shaped: the 200 fake agents tick in phase (spawned within 0.5 s, all every 2 s), so about
  40 KB arrives at once every 2 s. Per-resource coalescing would not help this load: each resource changes only
  once per period.
- Real agents are not phase-locked, so the default is now `STATUS_PHASE=random`. The slow client's echo p99 is then
  67-92 ms (spread: 38 ms), within the 250 ms placeholder, and the whole default profile passes. `lockstep` still gives
  about 570 ms. In every run with floods attached, the slow client's first probe takes about 5.6-6 s. It waits behind
  flood output already written to its socket before the core closed those streams as slow consumers (bounded
  streams). With `SLOW_ATTACH_FLOODS=0` the worst probe is 24 ms. p99 hides that single outlier at about 115 probes.
- A configured slow client must complete at least one probe (`slow_echo_samples`, `BUDGET_SLOW_ECHO_MIN_SAMPLES`):
  zero samples fails instead of passing on an empty measurement, and a probe still unfinished at the end counts as a
  lower bound on `slow_echo_max_ms` (receipt `echo_unfinished_ms`). With no slow clients configured these budgets do
  not apply.
- That first probe now has its own budget, `slow_echo_max_ms` (placeholder 1000 ms). It was 5.6 s because flood bytes
  were already in the OS send buffer. With a 16 KiB send buffer per connection it is about 0.58 s, and the default
  profile passes (`docs/research/bounded-streams.md`).

## Deferred

Concurrent layout edits (versioned docs vs Loro); protocol compatibility gate; multi-client convergence after `missed`.

## Candidate decisions waiting on these tests

- Core protocol rules (`research/protocols-and-ui.md`).
- Commands and bindings declared in extension manifests; context keys for contextual bindings.
- Crash budget; namespace schema versions and a snapshot at `replace`.
- A size rule for one layout shown on clients of different sizes.
- A small prim widget catalogue borrowing A2UI's shape, if S3 favours declarative or hybrid.
