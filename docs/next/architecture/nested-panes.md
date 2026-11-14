# Nested panes

Status: accepted design (human approval, 2026-10-04), **partially implemented**.
Stage 3 view panes, scope and agent-start composition are implemented, with the
request-keying limitation recorded below. Stage 4 core-owned named boxes and
stage 5 generic box selection/zoom/close remain **unimplemented**; acceptance of
this design is not availability. Collections retain their accepted
[package authority](collections-authority-transition.md) until that cutover.
Use the [current behavior matrix](../api/current-behavior-matrix.md),
[tree/session contract](../api/layout-tree-v1.md), and
[agent-start views contract](../api/agent-start-views.md) for shipped behavior.

A pane can contain panes. Widgets such as the Tasks and Messages views become
ordinary panes that show an extension view instead of a terminal, so the one
layout engine handles sizing, PTY resize, drag and move. This replaces the
placement machinery of panel v1 (see
[Extension UI framework](extension-ui-framework.md)) and answers which agents a
view covers when a Collection holds several.

This note records the owner's decisions as they are made. It becomes an
implementation mandate only when accepted.

## Historical baseline (before stages 1–3)

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
| 2026-10-04 | **Agent-start views are on by default**: the original single-Tasks/aspect-dependent placement is superseded by the [implemented views-box rule](../api/agent-start-views.md): Tasks above Messages in a box below the terminal. Explicit pane-menu/CLI view placement remains available. |
| 2026-10-04 | **Closing the last pane in a box**: an unnamed box disappears and its space returns to its neighbors; a named Collection stays, shows "empty", and closes only when the user closes it. |
| 2026-10-04 | **Nesting depth is capped at four levels of boxes** inside a tab; the cap may be raised later. |
| 2026-10-04 | **Agent start rule details** accepted as written in [Agent start rule](#agent-start-rule). |
| 2026-10-04 | **Interaction model accepted as provisional**: good for the first build, explicitly open to iteration. See [Interaction](#interaction-provisional). |
| 2026-11-02 | **Frame-v2 (supersedes the earlier double-frame prototype)**: one terminal outer perimeter encloses its unframed terminal body and fully framed attached views, side by side or inside the bottom. Ordinary boxes keep normal pane frames. Automatic default sizing is transient; restore keeps only the last actual ratio. See [Frame style](#frame-style). |
| 2026-10-04 | **Unimplemented stage-4 target: Vqro core owns boxes completely, including their names.** At that cutover the Collections extension and archiving are dropped, and core absorbs named boxes, stack switcher/list view, member edits and box close. This does **not yet supersede** current package authority in [Collections authority transition](collections-authority-transition.md). See [Collections become boxes](#collections-become-boxes). |
| 2026-10-04 | **No compatibility floor while Vqro is in development (option B).** This permits explicit retirement/replacement decisions, not silent reinterpretation of named codecs, methods or fixtures. The implemented [endpoint contract](../../../AGENTS.md#stable-client-endpoint-contract) and [surface-v2 decision](endpoint-surface-codec-v2.md) supersede the original blanket fixture-replacement plan. |
| 2026-10-04 | **Identity and persistence** as in [Identity and persistence](#identity-and-persistence): stable box IDs; view panes keep their place and show "unavailable" when their extension is missing, recovering by themselves; live server upgrade keeps running terminals by converting the old layout on the way in; downgrade is unsupported; the old session file is backed up before conversion. |
| 2026-10-04 | **Migration from panel v1** and **out of scope** as in [Migration from panel v1](#migration-from-panel-v1) and [Out of scope](#out-of-scope). |
| 2026-10-04 | **Build stages approved** as in [Build stages](#build-stages). |
| 2026-10-04 | **Views are kept as a pane content kind**, alongside terminals, for the reasons in [Why views](#why-views). Terminal programs remain the escape hatch when an extension needs full control. |
| 2026-10-04 | **View scope rule**: a view covers every terminal inside its parent container. Placement is the only configuration. The view header always shows its scope ("Tasks · agent1", "Tasks · 3 terminals"). Vqro supplies the covered terminals; the extension decides what they mean. An explicit "pin" override may come later; scoping by the nearest named Collection was rejected. |
| 2026-11-05 | **View-only group scope**: terminal-free containers inherit the nearest enclosing container with nonempty full terminal membership. Hidden stack members count; existing nonempty boundaries, including multiple-terminal scopes, are unchanged. A view-only root has empty scope. Placement changes rederive scope without stored pins or new box IDs. Frame qualification and sizing use the same rule. This permits an ordinary terminal-free Tasks/Messages box inside an agent's frame. |

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
| Scope | None | Full terminal membership of the nearest nonempty enclosing container; terminal-free groups inherit |

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

The original 2026-10-04 single-view/aspect-dependent rule is superseded by
[Agent-start views box](../api/agent-start-views.md):

- Because the layout is shared, this is a server-applied user setting.
- The rule applies once per terminal, via `pane.report_agent`. Closing its views
  does not refill them on a later agent start.
- Existing covering views or an already-applied marker leave the terminal alone.
- The configured group is admitted atomically within pane/depth caps, always
  **below** the terminal regardless of aspect ratio. The default is Tasks above
  Messages. Existing layout edits can move the group beside the terminal.
- Default sizing is transient automatic sizing, not a fixed persistent size;
  restore retains actual ratios with AUTO off.

## Interaction (provisional)

**Target design, not a claim that generic box selection/zoom/close is available.**
The implemented collapse ancestor picker grants collapse/expand only; see
[generic collapse](generic-pane-collapse.md).

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

The human's 2026-11-02 frame-v2 brief supersedes the earlier double-terminal-frame
prototype. This is a render/sizing policy over the accepted tree, not new box
identity, persistence, or protocol. Source acceptance and human visual approval
remain separate gates.

```
┌ agent1 ──────────────────────────────────────┐
│ $ pi …                 ┌ Tasks · agent1 ────┐│
│                        │ ▸ Spike fix       ││
│                        └───────────────────┘│
└──────────────────────────────────────────────┘

┌ agent1 ──────────────────────────────────────┐
│ $ pi …                                      │
│┌ Tasks · agent1 ────────────────────────────┐│
││ ▸ Spike fix                               ││
│└───────────────────────────────────────────┘│
└──────────────────────────────────────────────┘
```

A maximal qualifying subtree has exactly one terminal across **all** membership,
including hidden stack children, and all its views scoped to that terminal.
It gets one outer terminal frame/title and an unframed terminal body inside;
every view retains a full frame. Ordinary/named/multi-terminal boxes keep normal
pane frames. Labels never grant grouping authority. Hidden branches emit no
presentation work; standalone or zoomed views keep a full view frame. Scope is
unchanged by zoom. The earlier schematics describe placement, not final chrome.

One shared geometry plan supplies the body used by PTY sizing, rendering,
cursor, hits, hyperlinks and Kitty clipping. Parent border/title hits belong to
the terminal; child view hits take precedence. Divider drag hits use only the
view-border cell, never terminal content.

### Transient automatic sizing

New views with no explicit ratio, including agent-start defaults, opt in to
cached neutral row/header/label metrics. Accepted document publication measures
once, with namespace/incarnation/scope/revision identity. Host scope-label widths
are included and width-changing metadata renames invalidate only their target. There are no provider
calls or document scans during frame/layout/client fanout. Age ticks use fixed
status budgets, widths use four-cell buckets, and equal metrics do not visit
geometry. Hidden targets retain metrics only. A changed live target queues a
neutral invalidation; existing host geometry control applies it with current
viewport/accessory exclusions. Document callbacks never resize PTYs themselves.
Unchanged integer allocations do not change ratios or request PTY geometry work.

Only the views box explicitly created by the agent-start rule or `pane view add`
is eligible for automatic sizing while armed. Ordinary user splits retain their
ratios even when they contain views. Side boxes target their aggregate text width
within 25–60% of the available split; stacked boxes target aggregate row count,
capped at twelve rows and 35%. Vertically arranged children sum their body rows
and individual frame/header chrome; their width is the maximum child width.
Collapsed children contribute only their compact geometry. Internal child shares
fit within that one capped box, rather than applying the cap to each child.
Aggregate minima reserve twenty terminal body columns plus a possible
scrollbar gutter, four terminal rows, and view minima. Existing stack headers
are charged to the actual selected terminal/view branch. Manual splits retain
their ratios. When the viewport cannot
satisfy all minima, terminal allocation wins, then nearest-to-terminal attached
views before outer views; saturating cell geometry handles otherwise impossible
pixel minima. AUTO allocations may use any finite interior ratio, including a
view share below ten percent, to preserve the actual row cap. Their saved ratios
round-trip unchanged. Manual/API ratio limits remain 0.1–0.9. Content may
truncate or become zero-sized, never escape its rectangle.

`pane view add --ratio R` is explicit manual view share (0.1–0.9), quantized to
cells. A successful explicit ratio/drag disarms the affected views box, including
its non-leaf child subtree; directional pane resize disarms the affected terminal
group. Rejected mutations
do not disarm. Adding another default never rearms an older manual choice.
Only actual ratios persist: restart/handoff restores them with auto **off**.
Persistent auto/manual policy is deliberately deferred.

## Collections become boxes

**Stage 4 target, unimplemented.** Decided 2026-10-04. At that future cutover,
a Collection becomes a named box owned by Vqro core. The following is not a
statement of current package removal or archiving availability.

- Core keeps: box names, split and stack kinds, the stack switcher and list
  view, adding and reordering members, moving a member out (drag out or the
  existing promote), and closing a box (all members, or members moved out).
- New terminals added to a box join the stack holding the box's focused
  terminal; if the box has no stack, they are stacked with the focused member.
- Dropped: the `vqro.collections` package, its package state, document,
  actions and exact-pin admission, and archiving.
- On upgrade, existing Collection names become box names, and archived
  terminals are restored into their stack so nothing stays hidden.
- `collections-authority-transition.md` becomes superseded at implementation
  cutover, not merely design acceptance. The Wasm package path would lose its
  production Collections user; the extension platform remains for other packages.

## Identity and persistence

Historical stage-1/2 plan plus **unimplemented stage-4 general box identity**.
Current session writers are v10, not v8; stacks retain container IDs, while
anonymous splits have no newly allocated stable box IDs. See the implemented
[tree/session contract](../api/layout-tree-v1.md). The original plan follows.

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
- **Historical agent-start setting, superseded.** The initial
  `ui.agent_start_view` single-view override remains supported, but current
  `ui.agent_start_views` defaults to Tasks and Messages below the terminal.
  Unset no longer means no views; an empty list disables the rule. See the
  [implemented configuration and admission](../api/agent-start-views.md).
- **Badge clicks.** A badge click selects its tab and has no action of its own
  (the recorded decision of 2026-10-06), replacing "focuses the view pane
  covering that pane, or adds one".

## Out of scope

| Not included | Notes |
|---|---|
| Floating panes | Popovers cover small floating needs |
| Saved layouts or templates | Later; the agent start rule covers the common case |
| Archiving redesign | Dropped in the stage-4 target; current Collections package authority remains |
| Pinning a view to a chosen scope | Placement decides |
| Different layouts per client | The layout is shared |
| General automatic size rebalancing | Only explicitly created default views boxes opt in to [transient automatic sizing](#transient-automatic-sizing) |
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
| 4 | **Unimplemented:** Collections absorbed: package and archiving removed, names and stack switcher and list view in core, CLI updated | 4–6 h | ~1 h |
| 5 | **Target design:** Interaction: select box, drag drop zones, swap out of a box, box zoom and close | 4–6 h | human use and iteration |
| 6 | Render-scale benchmark with nested views; docs | 1–2 h | ~0.5 h |

Stage 3 landed view content, placement-derived scope, the light frame, the
Tasks/Messages views on scopes, panel v1 deletion, badges, `vqro pane view`
and the server-side agent-start rule. Default views-box composition is now
implemented; re-keying the extension view request to a scope remains a named
follow-up in
[Stage 3 implementation notes](#stage-3-implementation-notes-2026-10-07).

Total roughly 23–34 h of work plus reviews. Stages 3 and 4 can run in
parallel after stage 2. Stage 3 comes first if only one owner is available,
because it delivers the per-agent Tasks views.

## Open questions

- Interaction: provisional; iterate after the first build.
None for the design. Interaction is provisional and will be iterated after
stage 5; archiving may return as its own design.
