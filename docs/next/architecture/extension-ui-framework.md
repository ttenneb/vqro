# Extension UI framework

Status: proposed

Vqro draws all non-terminal UI from views that extensions publish. Extensions
describe meaning. Vqro owns drawing, theme, focus, input, layout and
performance, and keeps a built-in fallback so terminals stay reachable when
every extension is broken. This note records the owner's decisions as they are
made. It becomes an implementation mandate only when accepted.

The decisions below describe the target framework. The
[first version](#first-version-scope) is a deliberately cut-down subset of it.

## First version scope

Accepted by the owner on 2026-10-03 (relayed by the pilot coordinator).

**In v1:**

- the new view format and pipeline ([Pipeline](#pipeline));
- pane **split** panels and **popovers**; the task and messages panels are the
  first users;
- **badges** on the pane border, the tab and the sidebar row;
- **v1 building blocks** (final, 2026-10-03; panel look revised by the owner
  on 2026-10-03): section with an optional summary in its header; row with a
  state glyph from the fixed Pi set (toned with the existing `ViewTone` set),
  a count, an age, a detail line (for example owed/since), a terminal link with
  live agent state, and children that expand and collapse; **one line per row**,
  truncated with an ellipsis; actions; a 2–4 option choice; a one-line text
  input through the rename dialog. Vqro also draws the loading, empty,
  unavailable and "unsupported" states (pipeline behavior, not blocks);
- fixed panel sizes, scrolling, persisted display state, the
  `vqro pane panel` command, the "show when an agent starts" rule, in-view
  action errors, and the reference extension with render tests.

**Deferred:**

| Deferred item | Comes with |
|---|---|
| Extension- or user-defined layouts, layout selection, exclusive-region owner config. v1 has **one built-in layout**. | A later layout step |
| The sidebar (and its tabs) and the terminal tab bar as extension views | After the panels |
| Overlay placement | Later; v1 is split plus popover |
| Drag-to-reorder, rename as an action (the rename dialog's text field is still used for text input), extension context-menu items, selection | The sidebar step |
| Text, key-value and table blocks | The Terminal Inspector migration |

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
| 2026-10-03 | **Pipeline: one new, separately named, versioned view format** built on the existing live host-document path and absorbing `ViewDocument`'s blocks. `host.document.v1`/`v2` and `ViewDocument` stay frozen until their consumers migrate. See [Pipeline](#pipeline). |
| 2026-10-03 | **Panel size comes from the layout or the user, never from content.** Content scrolls. The terminal resizes only on open, close or user resize (once, on drag release). A pane too small for a panel keeps it closed and shows a border badge that opens it as a popover; the current fallback to a full-pane view (`src/client/shell/contributions.rs`) is retired. |
| 2026-10-03 | **Scrolling** is Vqro-owned in every panel and sidebar tab (wheel and scrollbars, reusing existing scrollbar code), anchored on an item ID across updates, per client. |
| 2026-10-03 | **Display state persists** in the client preferences file (`src/client/shell/preferences.rs`), keyed by terminal ID: open panels, collapsed rows, sizes and the active sidebar tab. Reattach restores it. |
| 2026-10-03 | **Show/hide command**: `vqro pane panel show\|hide\|toggle <pane> [<view>]`, passed by the server to attached clients as an optional, advertised display request that older clients ignore. The "show when an agent starts" rule is a client setting that runs in the client. |
| 2026-10-03 | **Action errors** appear in the originating view as a one-line message at the bottom of the panel or popover. Repeated failures go to Vqro's existing notices. Never in the terminal. |
| 2026-10-03 | **Badges are in v1** on the pane border, the tab and the sidebar row; clicking one opens the panel or popover. This formalizes the existing tab badges (`src/client/shell/tabs.rs`). |
| 2026-10-03 | **Testing**: grow `extensions/reference-host-document` into a reference extension exercising every block, placement and action, plus render tests per block without real terminals and a test for the "unsupported" fallback. |
| 2026-10-03 | **Text input for decision answers** uses the same one-line field as the rename dialog. |
| 2026-10-03 | **Filters and notifications are not in v1.** |
| 2026-10-03 | **Cut-down first version accepted** (relayed by the pilot coordinator): one built-in layout; sidebar and tab bar views after the panels; no overlay in v1; reorder, rename as an action, extension context-menu items and selection deferred; text, key-value and table blocks deferred to the Terminal Inspector migration. See [First version scope](#first-version-scope). |
| 2026-10-03 | **A pane's split panel is its bottom edge slot**, full pane width, and reads like the Pi todo widget above the editor. A right-side column is not v1. Fixed height; the terminal resizes once, on open, close or user resize. |
| 2026-10-03 | **Panels follow the Pi widget's look** (owner review of the first spike): header line `▾ ● Title ⊟ · summary`, one line per row, Pi-style tree connectors (`├─`, `└─`, `│` lanes), per-row `▾`/`▸` for children, the fixed state-glyph set (○ open, ◐ in progress, ⊘ blocked, ◎ reported, ● done) toned with the existing tone set, the title truncated with an ellipsis, and the assignee with its live state, the owed text and the age right-aligned and dim. Clicking the header collapses the panel, a row's marker collapses that row, and clicking a row's assignee jumps to that pane. |
| 2026-10-03 | **The panel's own summary is the extension's**, drawn after the header title (done/total and the non-zero states). Vqro draws the glyphs, connectors, tones and the age. |
| 2026-10-03 | **v1 building blocks fixed without a separate mockup pass** (relayed by the pilot coordinator): the cut-down set in [First version scope](#first-version-scope). The visual review is the building owner's first spike screenshot, which the owner must approve before more is built. |

## Pipeline

- **Format:** one new, separately named, versioned format. It keeps the live
  path, the tree, terminal references and the action envelope of
  `HostDocumentV1`, and adds `ViewDocument`'s content blocks. Old formats stay
  frozen until their consumers migrate (compatibility rules in `AGENTS.md`).
- **Updates:** the extension signals "changed"; Vqro fetches the latest
  version at its own pace.
- **Actions on a changed view:** each action already carries the view
  generation, revision, item and action ID (`HostDocumentExtensionActionV1`).
  Vqro drops the click if the item no longer exists; otherwise it sends the
  revision the user saw, and the extension decides whether it still applies.
- **Compatibility:** an unknown block type shows "unsupported" in place; new
  fields are optional and older clients ignore them.

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

## Building blocks (target set; v1 subset in [First version scope](#first-version-scope))

- **Section:** titled and collapsible, with an extension-authored summary.
- **Row:** a label on one line, with a state glyph from the fixed set (○ open,
  ◐ in progress, ⊘ blocked, ◎ reported, ● done), an optional count or age, a
  detail line, a link, actions and children (a tree, expanded and collapsed by
  Vqro, drawn with `├─`/`└─`/`│` lanes and a `▾`/`▸` marker per row).
- **Terminal link:** the extension names a terminal; Vqro supplies the live
  name and agent state, and a click focuses it.
- **Age:** the extension sends an instant and Vqro displays the elapsed time.
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

None for v1. The visual review is the first spike screenshot (see Decisions).
Later steps (sidebar, layouts, overlay, Terminal Inspector migration) reopen
their own items.
