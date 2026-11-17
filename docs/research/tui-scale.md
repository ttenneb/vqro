# TUI scale probe and measured fixes — 2026-11-17

## Reproduction

```sh
CARGO_TARGET_DIR=~/.local/share/herdr/scratch/prim-slice1-target cargo build --release --bin prim
env -i HOME="$HOME" PATH=/usr/bin:/bin python3 tests/e2e_tui_scale.py \
  ~/.local/share/herdr/scratch/prim-slice1-target --duration 30 --receipt /path/to/receipt.json
```

The opt-in probe uses a private `--fake` core, the real sidebar extension, three real TUI terminal views and an
explicit private tmux socket at 160×44. Profile: 600 terminals, 200 agents (~100 status updates/s), one 1 MB/s flood.
All children use a private HOME/state/socket/extension root without HERDR variables. `PRIM_HERDR=/bin/false` also
prevents accidental real-backend auto-start during recovery. Cleanup identifies only that exact root (including
socket inodes), checks PID birth before fallback signals, and reports leftovers. No install or live session used.

CPU/RSS come from the TUI process, not the core or harness. Latency is sending a unique short keyboard marker to
the focused idle pane and observing it in capture-pane; it includes tmux command launch and up to 20 ms polling,
so is an upper observation, not a precise display/scanout timestamp. Carriage return avoids falsely calling a
wrapped marker lost. `PRIM_TUI_PROBE` opts into bounded local frame/draw/received-byte counters, refreshed at most
once/second and on normal exit. It does not change protocol or retain an unbounded trace.

The probe is in `scripts/check.sh full`. Its performance budgets are enforced since 2026-11-17, set from measured
results (`docs/test-plan.md`, "Scale budgets"); they were non-enforced placeholders before. Correct input delivery, no missed events, a stable sidebar, actual status/flood workload and owned-process cleanup
are gates. Received decoded stream bytes must demonstrate at least 90% of the requested flood rate; otherwise the
receipt cannot claim that workload. This caught an early wrong fake-backend argument: `--output-bytes-per-sec` is
required. Earlier receipts without `-corrected` in the evidence directory do **not** establish flood performance.

## Corrected observations

| Measure | Baseline (15 s) | Fixed (30 s) |
|---|---:|---:|
| TUI CPU, % of one core | 23.18 | 17.55 |
| TUI RSS peak, MiB | 36.00 | 37.65 |
| Mean / peak total fps | 232.57 / 247 | 87.00 / 92 |
| Peak background fps | unbounded (247) | 55 |
| Marker observation p99 / max, ms | 51.03 / 51.03 | 52.07 / 53.17 |
| Observed flood bytes/s | 993955 | 975023 |
| Sidebar crashes | 3, then failed | 0, running |
| Lost markers / missed events / leftovers | 0 / 0 / 0 | 0 / 0 / 0 |

Fixed run observed ~102 status changes/s and 131 input samples. Total fps includes deliberately immediate
interactive redraws; only background invalidations have the 60 fps budget. Baseline CPU is **not** a like-for-like
healthy-sidebar comparison: after crashing, its sidebar content disappears. A separate no-flood intermediate
measurement justified panel caching: stable full-sidebar draw cost fell from ~3.0 to ~0.7 ms and CPU from28.5% to
11.85%. Those intermediate numbers are not flood claims. These are individual local observations, not a latency
promise or evidence beyond this profile/terminal size.

## Changes and checks

- Client background redraw invalidations coalesce to a frame deadline, retaining pending dirtiness. Keys win the
  select and incoming draining stops for queued keys; interactive UI changes do not wait for the deadline.
- Immutable merged panel content is reused until entry/live/snapshot invalidation. Prepared rows reuse it while
  page/filter/expansion are unchanged; selection/scroll/chip do not re-flatten. Geometry, palette and icon-dependent
  row heights still recompute, so resizing/themes cannot inherit stale hit areas. Invalidation also releases old
  prepared content, including hidden panels.
- Sidebar batches status events in its own extension worker at most10Hz. It snapshots under the SDK cache lock,
  releases it before RPC, preserves a wake arriving during publication, and records last content only after success.
  No resource facts or terminal bytes are dropped/coalesced—only derived publication and drawing.
- Rust150 tests pass (17 optional ignored); Python78 pass. New checks cover frame/key priority, panel invalidation,
  shape cache keys, coalescing bursts, publication races and failure bookkeeping. Sidebar unit tests fail on the
  baseline. Private real-hc TUI smoke passes all five checks, including30MB flood/reconnect/recovery quit/cleanup.

Evidence: `~/.local/share/herdr/scratch/prim-independent-review/tui-scale-before-corrected.json`,
`tui-scale-final-corrected.json`, `tui-sidebar-before.log`, `tui-scale-tests-final.log`,
`tui-scale-python-final.log`, `tui-scale-smoke-final.log`. The full all-suite check was not rerun by this owner;
its new probe step and the affected suites were run independently. Parent owns integration/install.
