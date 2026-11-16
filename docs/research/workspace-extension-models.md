# Workspace extension models in existing systems (2026-11-16)

Source: subagent reading shallow clones in `~/Projects/ref/{nvim,vscode,zed,zellij,wezterm,kitty,wave}` (paths below
relative to that), plus `rex api` (read only) and web docs. [U] = unverified, [I] = inferred.

## Per system

| System | Primitives | How extensions draw | Isolation / crash / reload | Clients | Lessons |
|---|---|---|---|---|---|
| Neovim | buffers, windows, tabs, commands, keymaps, autocmds over msgpack-RPC (`nvim/runtime/doc/api.txt`) | cell grid + opt-in `ext_*` semantic externalisation of cmdline, popupmenu, messages, tabline (`api-ui-events.txt:25-56`) | Lua in-process; remote plugins out of process; failing `ui_attach` force-detached | many UIs; grid = smallest client; `ext_*` off unless all clients support it | remote plugins being "simplified, massively" ([#27949](https://github.com/neovim/neovim/issues/27949)) |
| VS Code | contribution points: commands, menus, keybindings with `when`, views ([docs](https://code.visualstudio.com/api/references/contribution-points)); activation events | mostly declarative; webviews heavy, lose state when hidden ([docs](https://code.visualstudio.com/api/extension-guides/webview)) | one shared extension host; restart ≤3 times in 5 min then "Extension Bisect" (`nativeExtensionService.ts:150-215`); unresponsive after 3 s | remote hosts | one crash restarts all extensions |
| Zed | languages, LSP/DAP, slash commands, context servers, themes | none (no UI in WIT, `extension.wit:4-182`) | in-process WASM, epoch interruption; capability grants | | 10 WIT versions kept; ACP/MCP extensions deprecated |
| Zellij | ~275 host fns: events, panes, layouts (KDL), pipes, `start_or_reload_plugin` | program in pane drawing ANSI; **widgets serialised as DCS escapes inside the cell stream** (`ui_components/button.rs:88-99`) | in-process WASM, 17 permissions; crash shows "Panic!"; reload loses state [I]; no rollback | server/client, web and desktop clients; per-client render | WASM runtime switched twice; 0.46 breaking changes; 0.46 added OSC 7501 and "Zellij as a library" (`CHANGELOG.md:31,58`) |
| WezTerm | Lua events, keys, mux objects | formatted text from callbacks | in-process Lua | mux PDU codec v46 | last release 2024-02 |
| Kitty | remote control as JSON in DCS | kitten = program in overlay, then `handle_result` in-process | none | | |
| Wave | typed blocks, JSON widget launchers, `wsh setmeta` | typed views or program in a term block | [U] | Go + Electron [U] | legacy features removed |
| Rex | blocks, windows, layers, ownership, actions, keymap; block creators | blocks from creators | **per-client Lua; `client.set_script` replaces atomically, failing parts "skipped"** | many clients; **block owner sets size**; `client.perform` | API churn |
| acme / 9P | windows as files: `ctl`, `addr`, `body`, `tag`, `event` ([man](https://9fans.github.io/plan9port/man/man4/acme.html)) | text; programs intercept `event` and write actions back | process per tool | 9P over network | |
| Arcan | SHMIF IPC; arcan-tui buffer, list, readline | structured cell grid replacing VT | clients survive server crash via RESET/MIGRATE ([HACKING](https://github.com/letoram/arcan/blob/master/HACKING.md)) | clients migrate between servers | docs thin |

## Synthesis

- **Recurring primitives:** command registry, input→command bindings (contextual: VS Code `when`, Rex modes, Zellij
  input modes), named views/panes, layout tree, event subscription, a manifest declared up front. prim's seven match.
- **Rendering:** declarative wins for chrome (lists, menus, status, tab bar); program-in-pane wins for content and fast
  prototyping; freeform web UI costs most. A hybrid exists: a program that emits semantic widgets inside its byte
  stream (Zellij), or a grid with externalised semantic elements (Neovim).
- **Copy:** capability negotiation (off unless all clients support it); commands and bindings declared in the manifest so
  they stay listed while the extension is down; crash budget (3 in 5 min) then stop; owner-sets-size; RESET/MIGRATE-style
  client reconnect; acme's interceptable event stream for wrapping views.
- **Adopt outright?** No. Neovim's protocol is editor-shaped; Zellij's API is WASM-bound, large and unstable. Zellij as
  a library with OSC 7501 is worth a look as an execution/terminal backend, separately.

## Problems found in `spec/workspace-v0.md`

1. Declarative vs in-terminal is a false binary; add a hybrid (program emitting widget escapes, with a cell fallback).
2. If an extension view is a terminal node, replacing the extension ends a process, contradicting "must not touch
   nodes". Need a rule for extension-owned nodes.
3. No surveyed system rolls back automatically. Rollback restores code, not state: v2 may write a namespace schema v1
   can't read. Needs namespace schema versions, a snapshot at replace time, and a crash budget (a load-time health check
   misses late crashes).
4. One layout document vs clients of different sizes: needs a rule (smallest client, owner sets size, or per-client render).
5. Process per extension: measure the cost; VS Code chose one shared host to save resources.
6. Contextual bindings: use context keys (VS Code `when`).

## Tests proposed

1. The same view built three ways (declarative doc, ratatui program in PTY, PTY program + widget escapes), on two
   clients of different sizes; a scripted agent adds a button to each.
2. kill -9 an extension 100 times while rendering: placeholder < 100 ms, terminal bytes continuous, bindings still listed.
3. State-schema rollback: v2 migrates the namespace, roll back to v1.
4. Late crash 30 s after a passing health check: crash budget vs health window.
5. Stalled extension (blocks 5 s): last good frame kept, other views responsive, flagged unresponsive at 3 s.
