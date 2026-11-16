# Roadmap

**prim is a working prototype, and fluid.** This page says where we are and what we think comes next; it is
revised as we learn, not a commitment. Detail of what was built and when: `build-plan.md`.

## Where we are (2026-11-16)

Working and used daily over herdr-core:
- Core: workspace store (transactions, history, revert), resources from a herdr-core adapter (in-process), event
  log, commands routed to owners, extension supervisor (crash budget, safe mode), live view content.
- TUI client: layouts (tabs, splits, zoom), terminal views, the panel catalogue (pages, trees, toolbars, chips,
  badges, icons, filters), menus, popups, prompts, mouse, themes, configurable prefix and keymaps, self-restart.
- Extensions: tabs, panes, gestures, workspaces, sidebar, themes, keys, settings (and soon icons).

Not built yet: headless agent bridge (agents run in terminals today, status via OSC 7501), out-of-process backend
adapters, any second backend, Windows.

## Next (in this order, revisable)

| | Step | Why now | Done when | Kill / rethink question |
|---|---|---|---|---|
| 1 | **Headless agent bridge** (pi RPC first, then Claude Code) with agent views and input delivery by native capability (D4) | Biggest gap against the vision: prim owning the agent UI and routing input | An agent runs headless, its turns render in prim, and steer / queue / interrupt work through capabilities | Do runtimes fit one fact/effect shape without a core-side queue? (`experiments/e1-agent-input.md`) |
| 2 | **Out-of-process adapter protocol + tmux adapter** | Proves backends are swappable extensions; exposes leaks in the primitives | The same extensions run unchanged on herdr-core and tmux; adapters restart without losing resources | Does any primitive assume herdr-core? |
| 3 | **Undo for extension-made changes the user asked for** (e.g. theme, keymap picks) | Today the default revert skips extension writes | A user-initiated extension change can be undone from the prefix | |
| 4 | **Windows validation** of contracts (D12) | Before anything freezes | Core and TUI run natively, or the gaps are written down | |
| 5 | **Freeze candidates** | Only after 1–2 | Protocol pieces that survived two backends and two runtimes are marked stable | |

Rules: one trunk, short branches or worktree streams; small verification per change; record surprises in
`research/` and binding choices in `decisions.md`.
