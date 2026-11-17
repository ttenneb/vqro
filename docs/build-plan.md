# Build plan

D16: build prim itself, smallest complete and usable first. The north star: an agent can substantially modify its own
workspace through reliable public primitives, without changing the host or endangering underlying processes.

## Slice 1: first working prim

| Component | Scope |
|---|---|
| `prim serve` (core) | JSON-RPC 2.0 lines over a local socket. Workspace store: versioned entries (views, layouts, bindings, docs, extensions) changed only by transactions, persisted atomically, with history and revert. Resources from adapters. Event log with seq, cursors and `missed`. Command registry routed to owners. Extension supervisor with crash budget. Built-in recovery commands. |
| herdr-core adapter | Through `hc connect` (stdio, works locally and over ssh): list, spawn, kill, raw attach streams for input and output, resize, events (status from OSC 7501, exit). |
| Fake adapter | Deterministic test fixture only. |
| `prim tui` | Renders the active layout (splits, tabs) of terminal views (VT emulator over the raw stream) and declarative panel views; key prefix with built-in recovery menu that workspace bindings can't override; command palette. |
| `prim call` | CLI for scripts and agents: any core method from the shell. This is how agents modify the workspace. |
| First extension | A real extension, outside the core, that builds part of the workspace through public primitives only. |

Done when: the owner can open prim over herdr-core, run shells and agents in it, an extension adds a panel, command and
binding, an agent changes the layout with `prim call`, a broken extension is contained and reverted, and the core
restarts without disturbing any terminal.

## Then, driven by use

2. Agent bridges for headless runtimes (pi RPC, then Claude Code), with agent views.
3. Second backend (tmux or Zellij) through the same tests.
4. Windows validation of the contracts.
5. Whatever dogfooding shows is missing.

## Slice 1 status (2026-11-16)

Working and tested (`cargo test`: store unit tests, key encoding, end-to-end core tests over the fake backend):
- `prim serve` over herdr-core via `hc connect`: spawn, attach, immediate input, resize, OSC 7501 status, exit.
  Core restarts leave terminals running and keep resource ids.
- Workspace store: validated, all-or-nothing transactions with compare-and-set, history, revert, atomic persistence.
- Commands routed to owning connections; `prim.*` recovery commands built in.
- Extension supervisor: live replacement on spec change, crash budget, safe mode, per-extension logs.
- `prim tui`: tabs and splits from the layout, terminal views (vt100) and panel views, palette, resource picker,
  built-in prefix that bindings can't override, status bar showing safe mode and failed extensions.
- `prim call` / `prim run`: the agent path for changing the workspace.
- `extensions/sidebar.py`: a real extension using only public primitives (panel view, commands, binding, layout CAS).

Verified by hand against herdr-core: an agent-style `prim call` installed the sidebar; a broken extension
written the same way was contained by the crash budget and removed with one revert.

## Foundation hardening (2026-11-16)

- Store commits persist a candidate before publishing entries/history/counters, using unique temporary files,
  file sync, replacement and directory sync. Pre-replacement failures are effect-free; a post-replacement sync
  failure adopts and publishes the transaction but reports `durability_unconfirmed` (never replay automatically).
- Resource-id persistence failures are logged and reported as `persistence.error`, without hiding backend effects.
- Command registrations are connection-bound, so an old extension connection closing cannot disable its replacement.
- Regression coverage: failed write rollback and retry, uncertain directory sync and event publication, temporary-file
  cleanup, resource-map error reporting, and same-identity extension replacement routing/live-content cleanup.

## Bounded terminal streams and deadlines (2026-11-17)

- Terminal and herdr frame queues now have byte budgets. Backend CREDIT follows downstream handoff into the
  connection's bounded writer queue, rather than an unbounded adapter enqueue.
- Connection control/data budgets are independent. Slow streams close explicitly; their queued output is
  discarded without replay, while normal final output remains ordered before closure. Control overflow disconnects.
- Forwarded command deadlines report `timeout` with an indeterminate outcome, never retry, and clean pending calls.
- The TUI resets parser state and retries closed live terminal views with backend history, including closure racing
  the attach reply. Event-resync connection fencing is preserved.
- Release scale harness at the default 600-terminal/200-agent profile: stalled-only budgets passed; with both a
  stalled client and a 64 KiB/s reader, RSS peaked at 33.9 MiB and no slow echoes timed out, but the placeholder
  slow-echo p99 budget still failed (1.10 s versus 250 ms). Bounded memory does not imply bounded transport latency;
  existing socket bytes and status-event bursts cannot be preempted. Final teardown leaked no processes.
  Evidence and the explicit scale-test failure: `research/bounded-streams.md`.

## Known gaps found while building

- **History churn:** extensions that refresh views on every event (the sidebar on status changes) fill the
  transaction history. Consider a separate, unversioned channel for derived view content, or coalescing.
- **Resize ownership:** with several viewers of one terminal, the latest active one sets the size (herdr-core's
  rule). No prim-level rule yet.
- **Extension views** are only `panel` (declarative) or `terminal`; the hybrid option is untested.
- **Portability:** Unix socket only; Windows (named pipe) not yet validated (D12, D22 proposed). See "Windows work".
- **Agents** run PTY-backed for now; the headless agent bridge (pi RPC, Claude Code) is the next slice.
- **Adapters are in-process** Rust tasks behind a channel interface; the out-of-process adapter protocol from
  `spec/backend-adapter-v0.md` waits until a second backend shows what it needs.

## Slice 2: opinions as extensions (D17), 2026-11-16

- Core lost `prim.open`; keeps only recovery commands. New `menu/*` entries and gesture bindings (`mouse.right`,
  `mouse.middle`), contributions carry `owner` and disappear when their extension is off.
- TUI draws pane/tab menus, tab-bar and status-bar buttons from entries; prompts (`input`) and pickers (`pick`);
  passes `context` to commands; `client.menu|prefix|palette` are its only commands.
- Bundled default extensions, installed on first run from `defaults.json`: `tabs`, `panes`, `gestures`, `sidebar`,
  `settings` (helper `extensions/prim_ext.py`). Settings toggle extensions and client options (`doc/settings/tui`).
- Verified through a pty: prefix `n`, `+`, right-click pane/tab menus, rename prompt, middle-click close, status
  buttons, settings tab, and disabling the gestures extension (then `mouse`) removing right-click behaviour.
- Seen once, not reproduced: one `+` click opened two tabs.

## Slice 3: richer UI, workspaces (2026-11-16)

- Panel catalogue: pages, trees (client-side expansion), badges, scrolling with scrollbar, `/` filter, row menus,
  inline input rows; toasts (`ui.notify`); live status-bar segments. Resources gained `cwd`.
- New `workspaces` extension (manual list in `doc/workspaces/list`, git worktrees, open shell here, new worktree),
  published as live data that the sidebar consumes. Sidebar is one tree (workspaces → worktrees → terminals,
  other terminals) with presets in `doc/settings/sidebar`; OSC 7501 status as badges rolled up to parents.
- Settings popup has pages: Extensions, Interface, Sidebar, Keys (all bindings, filterable), About.

## Slice 4: polish streams (2026-11-16)

Built as parallel streams on git worktrees, reviewed and merged by a coordinator:
- **Sidebar:** one-line toolbar (`Show: … ▾`, `/`), view popup with toggles and presets, type icons coloured by state.
- **Themes (D18):** palette tokens with a built-in default; `themes` extension with 14 themes (Catppuccin with
  accent choice, Dracula, Tokyo Night, Nord, Gruvbox, Solarized, One Dark, Rosé Pine, Kanagawa, Light);
  tab bar band; safe mode ignores the theme.
- **Keys (D19):** rebindable prefix, binding priority and `client.none`, `client.focus|tab|quit|zoom`, keymap
  profiles (prim, tmux, screen) in the `keys` extension, live key feedback (kitty protocol, prefix only).
- **UI:** zoom, popups that survive their dropdowns, Nerd Font icons with fallback, self-restart on reinstall,
  client build tracking in `prim status`.

Known gaps: undo skips extension-made changes (e.g. theme picks); agents still run PTY-backed (headless agent
bridge not built); second backend (tmux) not started; Windows untested.

## Pi runtime bridge half (2026-11-17)

- `extensions/pi_bridge/`: standalone stdlib async Pi 1.5 RPC child-pipe client. Bounded LF framing,
  independent stdout/stderr drains, ID correlation, native effects with deadlines/no resend, and observed
  run/queue/dialog facts. Acceptance is not settlement; answering a dialog is dispatched, not acknowledged.
- `tests/fixtures/fake_pi.py` and `tests/test_pi_bridge.py`: deterministic tool/queue/settlement/dialog,
  crash, late-response, Unicode framing and backpressure regressions without any provider/model calls.
- `tests/pi_bridge_smoke.py --run`: opt-in real installed Pi 1.5.0 metadata-only check in a fresh network
  namespace and private environment/state/config/session roots. No real credentials or sessions used.
- Not wired to prim/provider registration yet. The future bridge must run inside a backend-owned terminal;
  this library does not turn prim into an agent process host. API and limits: `extensions/pi_bridge/README.md`.

## Extension lifetime (2026-11-17)

- Every managed extension receives a reserved stdin lifetime pipe and `PRIM_EXTENSION_LIFETIME=stdin`.
  The core retains its writer; core death closes it without requiring cleanup code. Extensions must exit on EOF.
- `prim_ext.py` watches from import on an independent thread, so startup/command code need not reach socket EOF.
  A plain non-Python pipe reader follows the same contract; no Unix-only parent-death API is required.
- Real fake-core regressions cover SIGKILL, SIGTERM and clean SIGINT shutdown, including blocked-before-connect
  helper startup. The scale cleanup check still reports zero leaked extensions under restart/flood load.
  Backend-owned terminal/agent lifetimes are not changed.

## Windows work (known, 2026-11-17)

From the static portability audit (D22 proposed; nothing compiled for Windows yet; installing a Windows target needs
the human's OK). Contracts are updated; this is implementation.
- **Compile:** a transport module with a named-pipe arm for `src/server.rs` / `src/client.rs` (tokio has
  `net::windows::named_pipe`; `src/rpc.rs` is already generic over the byte stream); `cfg(unix)` around SIGTERM in
  `src/main.rs`; a Windows branch for the TUI re-exec (today an unused variable there would break zero warnings);
  `tests/core.rs:141` and `tests/scale.rs` onto the transport or gated to Linux. Estimate about 2-3 h with the runtime
  fixes below.
- **Runtime blockers that compile but fail:**
  - `src/store.rs` opens the state directory to fsync it. On Windows `File::open(dir)` fails, so the core would not
    start. Use a write-through replace (`MoveFileExW`) and skip the directory sync there.
  - `src/paths.rs` defaults use `HOME`, usually unset on Windows, so state would land in the current directory. Use
    `%LOCALAPPDATA%\prim`.
  - `extensions/prim_ext.py` connects with `AF_UNIX`, which CPython on Windows lacks. Use the `asyncio` proactor
    pipe connection. About 1-2 h, testable only on Windows.
- **Defaults and CLI:** `defaults.json` names `python3` (use a portable interpreter choice); `resource.spawn`'s default
  shell moves to the provider (`$SHELL` / `/bin/sh` in `src/server.rs` today); `prim stop` moves to `core.shutdown`;
  the background core needs detached creation flags instead of `process_group(0)`; the herdr-core default path in
  `src/main.rs` is a macOS bundle path.
- **Optional:** Job Objects for extensions (also covers wrapper grandchildren), and the named pipe's out-buffer size as
  the counterpart of the 16 KiB send buffer (re-measure the first-echo case).
- **Validation:** `cargo check` for `x86_64-pc-windows-msvc` and `-gnu` as a CI gate once the target is approved; a real
  Windows smoke run of the provider transport before any contract is a freeze candidate (`provider-v0.md` step 6).
