# Extension UI framework

Status: proposed

Vqro draws all non-terminal UI from views that extensions publish. Extensions
describe meaning. Vqro owns drawing, theme, focus, input, layout and
performance, and keeps a built-in fallback so terminals stay reachable when
every extension is broken. This note records the owner's decisions as they are
made. It becomes an implementation mandate only when accepted.

## Vocabulary

- **View:** what an extension publishes: bounded data describing content and
  actions for one scope (session, workspace, tab or pane). Each version
  replaces the previous one. The code calls these "documents"
  (`src/extension_contract.rs` `HostDocumentV1`, `src/view_document.rs`
  `ViewDocument`); "document" stays an internal term.
- **Region:** a slot in a frame that views fill.
- **Placement:** which region, or floating surface, a view is drawn in.
- **Layout:** a static declaration of which regions exist and their sizes.

## Decisions

| Date | Decision |
|---|---|
| 2026-10-03 | Vocabulary: "view" and "placement"; "document" is internal only. |
| 2026-10-03 | Three basic drawing methods: **split** (default; terminal resizes), **overlay** (covers terminal rows; short or urgent content), **popover** (anchored, dismissible). Most UI uses the split. |
| 2026-10-03 | Foundational scope: the **sidebar and its tabs** and the **terminal tab bar** are extension-defined views, not hard-coded UI. |
| 2026-10-03 | Hard constraint: low overhead in a TUI, a thin stack, and extensions that scale to many parallel agents. |
| 2026-10-03 | Layout model **B, declared layout**: layouts come from a small fixed grammar, extensions fill the regions, and Vqro resolves geometry. Free-form layout (extensions positioning or drawing anything) is rejected. |
| 2026-10-03 | **Layouts are static declarations**, not live views. An extension ships one or the user writes one in config, and the user selects which is active. |
| 2026-10-03 | **Edge docking**: at most one slot per edge (left, right, top, bottom), with terminals filling the centre. No general split tree in v1. |
| 2026-10-03 | **A pane is a small frame with the same model**: its split panel is its right or bottom edge slot; overlays and popovers float above it. |
| 2026-10-03 | **Reuse existing UI**: rename and one-line text input use the existing rename dialog (`render_rename_overlay`, `src/client/shell/overlays.rs`); context menus use the existing menu (`src/client/shell/context_menu.rs`) with extension items listed below Vqro's own; drag-to-reorder is drawn by Vqro, sent as a "move X before Y" action, and shown as pending until the extension republishes. |
| 2026-10-03 | **Region ownership**: shared regions (sidebar tabs, a pane's panel stack) hold many views; exclusive regions (tab bar, a top bar) have one owner chosen in user config, with Vqro's plain fallback if none is set. |
| 2026-10-03 | **Only the user opens views**, directly or through a rule the user enabled (for example "show the task panel when an agent starts"). An extension may raise attention through a badge but cannot open itself. |
| 2026-10-03 | **Keyboard**: one Vqro key moves focus between the terminal and views; inside views Vqro owns arrows, Enter and Esc. Extensions never bind keys. |
| 2026-10-03 | **Vqro actions available to views** are a small fixed, capability-gated set: focus a terminal or tab; open, close or toggle a view; open a popover; new tab, split and close. Everything else is routed to the publishing extension. |
| 2026-10-03 | **Display state is per client**: collapsed rows, selection, region sizes and the active tab. Only extension data is shared. |
| 2026-10-03 | **Migration order**: task and messages panels; then the sidebar (agents, attention list, tab switcher); then the tab bar; then Collections chrome and Terminal Inspector. Retire the full-tab Tasks view once the panels work. |
| 2026-10-03 | Scaling rules are **on hold**; this round covers UI design and architecture only. |

## Layout

```
 default layout                    same grammar, rearranged
┌──────┬─────────────────────┐    ┌─────────────────────┬──────┐
│ left │ top: tab bar        │    │ top: tab bar        │      │
│ side │─────────────────────│    │─────────────────────│right │
│      │ terminals           │    │ terminals           │ side │
│      │                     │    │                     │      │
│      │                     │    │─────────────────────│      │
│      │                     │    │ bottom: status bar  │      │
└──────┴─────────────────────┘    └─────────────────────┴──────┘
```

- Slot kinds: `side` (a column of tabbed views) and `bar` (one line). The
  `terminals` centre is always present and has a minimum size.
- Each slot declares its default, minimum and maximum size, whether it can
  collapse, and its default views. The user resizes and collapses; Vqro keeps
  that state per client.
- Geometry is resolved only when the layout or the terminal size changes.
  An invalid or missing layout falls back to the built-in layout plus a plain
  workspace/tab/pane navigator.

A pane uses the same model:

```
┌ pi · build ──────────────────────────┬ Tasks ─────────── 2 active ─ ▾ ┐
│ ⚑ 1 decision waiting      [open]     │ ▾ Pane panels        working  │
│ $ agent output…                      │   ▸ Spike fix  ● pilot-owner  │
│                                      │     owes report · 12m         │
│        ┌ Decision · Tasks ────────┐  │ ▸ CPU loop     ○ owner-fixes  │
│        │ Review spike now?        │  ├ Messages ──────── 1 waiting ─┤
│        │ [ Yes ] [ No ] [ Later ] │  │ coordinator · 3m · editable  │
│        │ Note: _________  [Send]  │  │ no reply to pilot-owner · 20m│
│        └──────────────────────────┘  │                              │
└──────────────────────────────────────┴──────────────────────────────┘
  overlay (top row) · popover (centre) · right edge slot with a panel stack
  ● = live agent state joined by Vqro
```

## Building blocks (proposed; to be finalized)

- **Section:** titled and collapsible.
- **Row:** a label with an optional state chip, count or age, detail line,
  link, actions and children (a tree, expanded and collapsed by Vqro).
- **Terminal link:** the extension names a terminal; Vqro supplies the live
  name and agent state, and a click focuses it.
- **Age:** the extension sends a timestamp and Vqro displays the elapsed time.
- **Text, key-value list, table:** for migrating Terminal Inspector.
- **Tones:** default, muted, accent, success, warning, danger. No custom colors.
- **Interaction:** a primary click; row and section actions with Vqro-drawn
  confirmation for risky ones; a 2–4 option choice; one-line text input in
  popovers; context-menu items; rename; reorder; selection.
- **Vqro-drawn states:** loading, empty, and unavailable (extension failed or
  disabled).
- **Not supported:** custom drawing, colors or sizes; raw terminal output or
  ANSI; extension keybindings; hover tooltips; animations; multi-line editors
  or multi-field forms; full-tab or full-pane views drawn by Vqro (full apps
  remain terminal programs); composing messages in a view.

## On hold: scaling rules (not decided)

1. Live agent and terminal facts never go through extensions; Vqro joins them
   when drawing.
2. Items have stable IDs and updates are incremental.
3. Vqro asks only for views that are on screen.
4. Updates are coalesced, latest wins, at most one per frame tick; extensions
   are never called per frame, per byte or per client.
5. Drawing cost scales with visible rows.
6. Hard bounds on items, depth and text.
7. Thin stack: extension → validated view store → Vqro renderer.
8. Benchmark at 1, 15 and 100 agents.

## Open questions

- Finalize the building-block set.
- Filters and notifications (proposed: not in v1).
- Text input: approved in principle through the rename dialog; confirm it also
  serves decision answers.
