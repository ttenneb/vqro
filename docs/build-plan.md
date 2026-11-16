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

## Known gaps found while building

- **History churn:** extensions that refresh views on every event (the sidebar on status changes) fill the
  transaction history. Consider a separate, unversioned channel for derived view content, or coalescing.
- **Resize ownership:** with several viewers of one terminal, the latest active one sets the size (herdr-core's
  rule). No prim-level rule yet.
- **Extension views** are only `panel` (declarative) or `terminal`; the hybrid option is untested.
- **Portability:** Unix socket only; Windows (named pipe) not yet validated (D15 says before locking contracts).
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
