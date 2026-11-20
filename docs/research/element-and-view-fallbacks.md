# Element and view-kind fallbacks: design — 2026-11-19

Status: design only (no code), for the human's Q-UI-3. Builds on D29 (`declarations-and-ui-clients.md`) and the UI
clients research (step 6 there). It closes the review's P2 dead end: today a view of any kind other than `terminal`
or `panel` shows "kind `X` is not supported by this client" (`src/tui/mod.rs`, `draw_view`), and a panel whose outer
`requires` names an unknown feature shows "This view needs a newer vqro UI" (`src/tui/panel.rs`, `render`). Both are
blank panes for the user. The goal is that new kinds and components are progressive enhancements: every client
always shows something correct, richer clients show more.

## What exists already
- Unknown row `type`s render as plain items (text, command, menu still work).
- `list_detail` scopes its `requires` inside the enhancement; the ordinary items are the fallback.
- `embedded_views` references carry a `text` shown when the source can't be embedded.
These are three special cases of one rule. This design states the rule once and applies it at two levels.

## 1. Element fallback (rows, chips, detail panels)

Any row may carry two optional fields:
- `requires: [feature]`: features needed to render *this row* natively (scoped, like `list_detail`).
- `fallback`: what to render instead: a row, a list of rows, or `"drop"`.

A client renders a row natively when it knows the row's `type` and supports every scoped `requires`. Otherwise:
1. With `fallback`: render it, applying the same rule to it (at most 4 levels, then plain).
2. Without: today's rule: a plain item from `text`, keeping `id`, `command`, `args`, `menu`.
3. `"drop"`: omit the row (for purely decorative enhancements).

Details:
- Applied before flattening, so ids, filtering, selection and counts work on what is shown. A fallback row without
  an `id` inherits the original's, so selection survives a client upgrade.
- Fallback rows are complete rows: nothing is inherited implicitly except `id` (explicit beats surprising).
- Third-party components are namespaced types, `type: "x.acme.table"`, with `requires: ["x.acme.table@1"]`. A client
  renders one natively only if it claims that feature in `hello`.
- `chips` entries and `detail_panel` follow the same rule (a chip's fallback is a chip; a detail panel's is a
  panel value).
- Bounds: fallback rows count toward the panel's row budget; at most 4 nesting levels.

## 2. View-kind fallback

A view entry may carry `fallback: "<view id>"`:
`view/flow-1 = {kind: "x.acme.canvas", requires?: [...], fallback: "flow-1.panel"}`.

A client that can't render the view (unknown kind, or unmet outer `requires`, including a panel's) renders the
fallback view in the same pane instead, recursively (at most 4, no cycles), else today's labelled placeholder.
- The fallback is an ordinary view: usually a `panel` whose content the extension publishes live (it already does
  this for the TUI today), or a `terminal` view running a program that draws the same thing: the review's "program
  in a pane" escape hatch.
- Referenced, not inline: one mechanism, live-capable, and the fallback is a normal view other clients and the
  layout can use. (Inline panel values were considered: simpler for a static note, but a second way to do it.)
- The pane keeps the original identity: `args.context.view` stays the original id (so `when`, menus, layout
  commands and `focus` hints work), plus `context.shown_view` for the view actually drawn.
- Popups and embedded references (`type: "view"`) resolve fallbacks the same way.
- This also gives panels an escape from the outer-`requires` notice: put the enhanced panel in a new view with a
  plain fallback panel.

## 3. Declarations (D29)
- Kinds are features: a client claims `x.acme.canvas@1` in `hello.features` to render that kind. An extension
  declares `works_with: ["ui/x.acme.canvas@1"]` (never `needs`: the fallback is mandatory), so Settings and
  `client.list` say which connected clients show only the fallback.
- The release lint warns when a bundled view uses a non-base kind without `fallback`, or a panel puts a non-base
  feature in its outer `requires` (sidebar: `icon`/`icon_names` today; harmless now, both clients render them).

## 4. Conformance and effort
- New cases in `tests/fixtures/client/panels.json` (row fallback, drop, nested, unknown namespaced type, id
  inheritance) and a new `views.json` (kind fallback, chain, cycle, missing fallback, outer requires). Rust model,
  TUI renderer and the Python text client implement them; spec text in core-protocol "Live view content".
- Estimate: ~1.5 h work + ~0.5 h checks. Risk: low; old content renders exactly as before (no new fields).

## 5. The review's P2 test (after this lands)
Pick one real extension with a different interaction model and record every host change it forces. Candidates:
- **Agent transcript** (`vqro.agent_items@1`, already planned by K3/spike-panel-1): native item stream with the
  bounded transcript panel as fallback. Real, needed, and exercises the scoped-enhancement path.
- **Extension log viewer**: a focusable, filterable table with follow-tail; fills the known "no in-app log viewer"
  gap and exercises a view kind with a panel fallback.
Recommendation: the log viewer, because the agent transcript depends on the agent standard landing first.
