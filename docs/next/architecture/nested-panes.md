# Nested panes

Status: accepted (human approval, 2026-10-04)

A pane can contain panes. Widgets such as the Tasks and Messages views become
ordinary panes that show an extension view instead of a terminal, so the one
layout engine handles sizing, PTY resize, drag and move. This replaces the
placement machinery of panel v1 (see
[Extension UI framework](extension-ui-framework.md)) and answers which agents a
view covers when a Collection holds several.

This note records the owner's decisions as they are made. It becomes an
implementation mandate only when accepted.

## What exists

- A tab's layout is a binary split tree (`src/layout.rs`, `TypedNode`).
- A Collection is already a container leaf in that tree
  (`LayoutLeaf::Collection`, `src/terminal_container.rs`): several terminals,
  one shown at a time. Nesting stops there, and only terminals can be members.

## Decisions

| Date | Decision |
|---|---|
| 2026-10-04 | **Tree model** (revised the same day from a four-part model): two container kinds, **split** (side by side or top and bottom, resizable) and **stack** (one child shown at a time with an `n/m` switcher), and **one leaf kind, the pane**, whose content is a **terminal** (a PTY) or a **view** (an extension view). Any container may hold containers or panes, at any depth, in either split direction. Layout, frame, focus, drag, move, zoom and close treat every pane the same; content matters only for input, resize, close, restart and scope (see [Pane content](#pane-content)). A Collection is a named split or stack managed by the Collections extension. |
| 2026-10-04 | **The layout is shared by all attached clients, including view panes**, as panes are today. Scroll position and expanded or collapsed rows inside a view stay per client. This supersedes panel v1's per-client open/hidden state. |
| 2026-10-04 | **A view pane may fill a tab or be zoomed**, like any pane. No feature opens a view that way by default. This supersedes the panel-v1 exclusion of full-tab or full-pane views drawn by Vqro. |
| 2026-10-04 | **"Add Tasks beside an agent" is on by default**: when an agent starts in a pane, Vqro adds a Tasks view pane beside it. The pane menu and the `vqro` CLI also offer "add Tasks beside". Details in [Agent start rule](#agent-start-rule). |
| 2026-10-04 | **Closing the last pane in a box**: an unnamed box disappears and its space returns to its neighbors; a named Collection stays, shows "empty", and closes only when the user closes it. |
| 2026-10-04 | **Nesting depth is capped at four levels of boxes** inside a tab; the cap may be raised later. |
| 2026-10-04 | **Agent start rule details** accepted as written in [Agent start rule](#agent-start-rule). |
| 2026-10-04 | **Interaction model accepted as provisional**: good for the first build, explicitly open to iteration. See [Interaction](#interaction-provisional). |
| 2026-11-02 | **Frame style (supersedes the 2026-10-04 light frame)**: one outer perimeter encloses the agent's box, including both its terminal pane and view. Each child, including the view, has its own complete, visible one-cell perimeter inside it, side by side or with the view below. No view is a header-only strip outside its agent's box. See [Frame style](#frame-style). |
| 2026-10-04 | **Vqro core owns boxes completely, including their names.** The Collections extension and archiving are dropped. Core absorbs the base features (named boxes, stacks with the `n/m` switcher and list view, adding and reordering members, moving a member out, closing a box). Archiving may return later as a separate design. This supersedes the package authority in [Collections authority transition](collections-authority-transition.md) once accepted. See [Collections become boxes](#collections-become-boxes). |
| 2026-10-04 | **No compatibility floor while Vqro is in development (option B).** This design may change the client protocol and frozen generation-1 formats and fixtures freely; older clients are not supported. The frozen-floor rule is meant for when community and ecosystem building starts. This requires changing the "Stable client endpoint contract" section of `AGENTS.md` and replacing the frozen fixtures. `PROTOCOL_VERSION` is still bumped so mismatched client and server refuse cleanly. |
| 2026-10-04 | **Identity and persistence** as in [Identity and persistence](#identity-and-persistence): stable box IDs; view panes keep their place and show "unavailable" when their extension is missing, recovering by themselves; live server upgrade keeps running terminals by converting the old layout on the way in; downgrade is unsupported; the old session file is backed up before conversion. |
| 2026-10-04 | **Migration from panel v1** and **out of scope** as in [Migration from panel v1](#migration-from-panel-v1) and [Out of scope](#out-of-scope). |
| 2026-10-04 | **Build stages approved** as in [Build stages](#build-stages). |
| 2026-10-04 | **Views are kept as a pane content kind**, alongside terminals, for the reasons in [Why views](#why-views). Terminal programs remain the escape hatch when an extension needs full control. |
| 2026-10-04 | **View scope rule**: a view covers every terminal inside its parent container. Placement is the only configuration. The view header always shows its scope ("Tasks · agent1", "Tasks · 3 terminals"). Vqro supplies the covered terminals; the extension decides what they mean. An explicit "pin" override may come later; scoping by the nearest named Collection was rejected. |

## Tree model

```
container = split(direction: side-by-side | top-bottom, ratio, children)
          | stack(selected, children)
pane      = terminal(terminal id) | view(extension view)
```

### Pane content

| | Terminal | View |
|---|---|---|
| Input | Keystrokes go to the program | Vqro handles arrows, Enter, Esc and clicks |
| Resize | The program is told its new size (PTY resize) | Vqro redraws |
| Close | Ends the process | Drops the view |
| Restart or reattach | The process keeps running and is reattached | Vqro fetches a fresh view |
| Scope | None | The terminals in its parent container |

### Why views

A terminal shows characters drawn by a program, and Vqro cannot know what they
mean. A view is data that an extension describes and Vqro draws.

| | Views (Vqro draws) | Everything is a terminal |
|---|---|---|
| Feels like Vqro | Same theme, mouse and focus rules | Each app has its own look (the old Tasks tab felt foreign) |
| Live agent state, click to jump | Vqro adds them | The app polls Vqro's API and requests jumps |
| Many agents | One extension feeds every view; no process per view | One process per view |
| Same data elsewhere | One view can feed a pane, a badge or a future sidebar | Characters can't become a badge |
| Other clients, tests, agents | Can read and draw the same data | Only a screen dump |
| Expressiveness | Limited to the building blocks | Unlimited |
| Cost to Vqro | A renderer and a format (built in v1) | Nothing new |

The limited building blocks are the real cost; terminal programs remain the
escape hatch.

Example 1, one Tasks view for the whole collection:

```
┌ collection: pilot ─────────────────────────────────────────┐
│ Tasks · 3 terminals    │ [agent1] agent2  server       1/3 │
│ ▾ Spike fix  ● agent1  │ $ pi …                            │
│   owes report · 12m    │                                   │
│ ▸ CPU loop   ○ agent2  │                                   │
└────────────────────────┴───────────────────────────────────┘
 collection = split[ view:tasks , stack[agent1, agent2, server] ]
 tasks covers agent1, agent2, server
```

Example 2, one Tasks view per agent, mixing both split directions:

```
┌ collection: pilot ─────────────────────────────────────────┐
│ Tasks · agent1  │ agent1 $ pi …                            │
│ ▸ Spike fix     │                                          │
├─────────────────┼──────────────────────────────────────────┤
│ Tasks · agent2  │ agent2 $ pi …                            │
│ ▸ CPU loop      │                                          │
└─────────────────┴──────────────────────────────────────────┘
 collection = top-bottom[ side-by-side[tasks1, agent1],
                          side-by-side[tasks2, agent2] ]
 tasks1 covers agent1 · tasks2 covers agent2
```

Example 3, a view below its agent (a top-bottom split):

```
┌ agent1 $ pi … ─────────────────────────────────────────────┐
│                                                            │
├ Tasks · agent1 ─────────────────────────── 2 active ───────┤
│ ▸ Spike fix   owes report · 12m                            │
└────────────────────────────────────────────────────────────┘
 top-bottom[ agent1, tasks ]
```

## Agent start rule

Accepted 2026-10-04:

- Because the layout is shared, the rule changes the layout, so it is a user
  setting applied by the server, not a client setting.
- It applies once per terminal. If the user closes that Tasks pane, a later
  agent start in the same terminal does not add it again.
- It does nothing if a Tasks view already covers that terminal (for example a
  collection-wide Tasks pane).
- The new pane goes beside the agent when the pane is wide, and below it when
  the pane is narrow, at a fixed default size.

## Interaction (provisional)

Accepted for the first build on 2026-10-04 and expected to change with use.
The aim is that nesting adds almost nothing to learn: existing keys keep
working, and one new idea, selecting the box around a pane, is added.

**Focus and keyboard**

- Directional focus (`prefix+h/j/k/l`) is geometric: it goes to the nearest
  visible pane in that direction, whatever box it is in. Entering a stack lands
  on its shown member.
- A focused view takes arrows, Enter and Esc; `prefix` keys always work.
- **Select box** (proposed `prefix+a`) highlights the box around the focused
  pane; pressing it again goes up a level. With a box selected, zoom, close and
  swap act on the whole box. Esc or any focus key returns to a single pane.

```
prefix+a once: the box around agent1 is selected

┏ box ━━━━━━━━━━━━━━━━━━━━━━━━━━┓┌ agent2 ────────────┐
┃ Tasks · agent1  │ agent1 $ pi ┃│ $ pi …             │
┃ ▸ Spike fix     │             ┃│                    │
┗━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━┛└────────────────────┘
 prefix+z zooms both · prefix+x closes both (one confirmation)
```

**Mouse**

- Drag any divider to resize at its own level.
- Drag a pane by its title onto another pane. The drop zone decides the result:

```
dropping onto agent2:
┌───────────── agent2 ─────────────┐
│          ▲ split above           │
│ ◀ split   ┌──────────┐  split ▶  │
│   left    │ stack    │   right   │
│           │ together │           │
│           └──────────┘           │
│          ▼ split below           │
└──────────────────────────────────┘
 edge = new split with agent2 · centre = stacked with agent2
```

- Dropping onto a box's title puts the pane inside that box; dragging a pane
  elsewhere moves it out of its box.
- On the keyboard, swap (`prefix+shift+h/j/k/l`) moves a pane within its box;
  at the box edge it moves the pane out one level.

**Zoom and close**

- `prefix+z` zooms the focused pane or the selected box.
- `prefix+x` closes the pane. A terminal with a running process asks first, as
  today; a view closes without asking. A selected box closes everything in it
  after one confirmation that states the pane count.

## Frame style

The human required a nested box on 2026-11-02, superseding the light frame.
One outer perimeter encloses both panes; the view and terminal each retain a
complete inner perimeter. The same rule applies with the view below:

```
┌ agent1 ──────────────────────────────────────┐
│┌ pi … ─────────────────┐┌ Tasks · agent1 ───┐│
││ $ pi …                ││ ▸ Spike fix      ││
│└───────────────────────┘└───────────────────┘│
└──────────────────────────────────────────────┘

┌ agent1 ──────────────────────────────────────┐
│┌ pi … ──────────────────────────────────────┐│
││ $ pi …                                     ││
│└────────────────────────────────────────────┘│
│┌ Tasks · agent1 ────────────────────────────┐│
││ ▸ Spike fix                                ││
│└────────────────────────────────────────────┘│
└──────────────────────────────────────────────┘
```

Every border is one cell thick, with no additional blank gutter. The outer
frame costs two columns and two rows. Children are split inside that inset,
then each child's complete border is subtracted for its terminal or view
content. Adjacent child perimeters are independent, not a title-only divider.
`pane view add --ratio R` gives the **view** share of the parent's available
split area, before child borders, in either direction. Integer cell allocation
quantizes this ratio; tiny panes can have zero content after their borders.
The accepted range is 0.1 through 0.9, default 0.5.

**Depth economy is a prototype for human visual review, not a final decision.**
The coordinator authorized one policy function that frames a box directly
holding a view, or an already named box; ordinary unnamed terminal-only splits
retain their existing rendering. Existing legacy names are read, not moved into
the canonical tree or given new authority. Screenshots include an all-box nested
comparison so the human can choose the final rule before stage 4.

A standalone or zoomed view keeps its complete pane frame, without a redundant
outer group frame. Zoom does not change placement-derived scope. The earlier
schematic examples above describe placement/scope only; their light-divider
appearance is superseded by these perimeters.

## Collections become boxes

Decided 2026-10-04. A Collection is now simply a named box owned by Vqro core.

- Core keeps: box names, split and stack kinds, the stack switcher and list
  view, adding and reordering members, moving a member out (drag out or the
  existing promote), and closing a box (all members, or members moved out).
- New terminals added to a box join the stack holding the box's focused
  terminal; if the box has no stack, they are stacked with the focused member.
- Dropped: the `vqro.collections` package, its package state, document,
  actions and exact-pin admission, and archiving.
- On upgrade, existing Collection names become box names, and archived
  terminals are restored into their stack so nothing stays hidden.
- `collections-authority-transition.md` becomes superseded when this is
  accepted. The Wasm package path loses its only production user; the extension
  platform remains for other packages.

## Identity and persistence

Decided 2026-10-04.

- **Boxes** have a stable ID that survives renames and moves. Today's
  container ID (`TerminalContainerId`, `src/terminal_container.rs`) becomes the
  general box ID; a tab's top level is a box.
- **Panes** keep their pane ID; terminals keep their terminal ID. A view pane
  records its extension and view. Its scope is derived from its position and
  never stored.
- **Session file**: the whole tree is saved, as a new session format version
  (8; today's is 7 in `src/persist/snapshot.rs`). Per-client view state
  (scroll, expanded rows) stays in the client preferences file, keyed by pane
  ID.
- **Agent start rule**: a per-terminal "applied" marker is saved so the rule
  runs only once per terminal.
- **Missing extension at restore**: the view pane keeps its place, shows
  "unavailable", and recovers by itself when the extension returns.
- **Upgrade**: live server replacement keeps running terminals by converting the
  version-7 layout (split tree plus collection leaves) into the new tree on the
  way in, importing Collection names as box names and restoring archived
  terminals into their stacks. Downgrade is unsupported. The old session file
  is backed up before conversion.

## Migration from panel v1

Decided 2026-10-04. References: `docs/next/api/host-view-v1.md`,
[Extension UI framework](extension-ui-framework.md).

**Kept:** the `host.view.v1` item format (sections, rows, state glyphs,
"waiting on" wording, ages, the expandable tree), validation, the
"unsupported" and "unavailable" states, badges, and the Tasks and Messages
extensions' logic.

**Changed:**

- A view names a **scope**, not a pane: Vqro asks the extension for a view
  (extension, view ID) over a set of covered terminals, instead of the view root
  naming `pane_id`. View panes with the same scope share one copy, so cost
  grows with distinct scopes, not with screen size or client count.
- `vqro pane panel show|hide|toggle` becomes `vqro pane view add|close <pane>
  <view>`.
- `ui.show_panels_on_agent_start` becomes the server-side agent start rule, on
  by default.
- A badge click focuses the view pane covering that pane, or adds one.

**Deleted:** in-pane panel geometry (the band and size claims), the
smallest-client arbitration, patch translation around panels, and the
`pane_panels_v1` capability.

**Docs:** `host-view-v1.md` loses "Several clients at one pane"; the
extension UI framework marks the split panel as replaced by nested panes.
Popovers are unchanged; they float above panes.

### Stage 3 implementation notes (2026-10-07)

Recorded deviations and deferrals from this section, so the design and the code
agree:

- **Scope composition reuses the per-pane evaluator.** The host derives the
  scope from placement and composes the existing per-pane `host.view.v1` roots
  whose `pane_id` is covered; it does not yet ask the extension for a view over
  a scope. Re-keying that request in the host<->extension contract is a named
  follow-up, not a second layout store: scope is still derived and never stored.
- **The agent-start setting is one switch.** `ui.show_panels_on_agent_start` is
  retired in favour of `ui.agent_start_view` (`<extension>:<view>`): setting the
  identity turns the server-side rule on, and unset adds nothing. The rule
  itself ships (adding the view beside a newly reported agent, once per terminal,
  via a durable per-terminal marker); the extension-declared identity, so it is
  on by default, is the immediate follow-up.
- **Badge clicks.** A badge click selects its tab and has no action of its own
  (the recorded decision of 2026-10-06), replacing "focuses the view pane
  covering that pane, or adds one".

## Out of scope

| Not included | Notes |
|---|---|
| Floating panes | Popovers cover small floating needs |
| Saved layouts or templates | Later; the agent start rule covers the common case |
| Archiving | Dropped; may return as its own design |
| Pinning a view to a chosen scope | Placement decides |
| Different layouts per client | The layout is shared |
| Automatic size rebalancing | Sizes change only on user action |
| Moving panes between tabs or workspaces | Unchanged from today |
| Actions and inputs inside views | Still deferred, as in panel v1 |
| Sidebar and tab bar as extension views | A later UI framework step |

## Build stages

Approved 2026-10-04.

Agent wall-clock estimates, split into work and waiting. This is a refactor-risk
change (layout identity, persisted state, live handoff, protocol), so
`AGENTS.md` requires characterization tests and a roundtable first.

| # | Stage | Work | Waiting |
|---|---|---|---|
| 0 | Characterization tests for layout, Collections, session v7 restore and live handoff; roundtable on the design | 2–3 h | ~1 h roundtable |
| 1 | Core tree: split and stack boxes at any depth (cap 4), panes as leaves, box IDs (names in stage 4), close rules, geometric focus. Testable with `AppState::test_new()`, no UI change | 4–6 h | ~1 h tests and review |
| 2 | Session v8, v7 conversion with backup, live-upgrade conversion, `PROTOCOL_VERSION` bump, fixture replacement, `AGENTS.md` endpoint section change | 3–4 h | ~1 h |
| 3 | View panes: view content, scope derivation, per-scope rendering, light frame, Tasks and Messages moved to scopes, panel v1 machinery deleted, badges, agent start rule, `vqro pane view` | 5-7 h | human screenshot review |

Stage 3 landed view content, placement-derived scope, the light frame, the
Tasks/Messages views on scopes, the panel v1 deletion, badges, `vqro pane view`
and the server-side agent-start rule (identity passed in). The remaining stage-3
deltas -- the extension-declared agent-start identity (on by default) and
re-keying the view request to a scope -- are named follow-ups in
[Stage 3 implementation notes](#stage-3-implementation-notes-2026-10-07).
| 4 | Collections absorbed: package and archiving removed, names and stack switcher and list view in core, CLI updated | 4–6 h | ~1 h |
| 5 | Interaction: select box, drag drop zones, swap out of a box, box zoom and close | 4–6 h | human use and iteration |
| 6 | Render-scale benchmark with nested views; docs | 1–2 h | ~0.5 h |

Total roughly 23–34 h of work plus reviews. Stages 3 and 4 can run in
parallel after stage 2. Stage 3 comes first if only one owner is available,
because it delivers the per-agent Tasks views.

## Open questions

- Interaction: provisional; iterate after the first build.
None for the design. Interaction is provisional and will be iterated after
stage 5; archiving may return as its own design.
