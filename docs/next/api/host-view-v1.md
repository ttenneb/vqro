# `host.view.v1`: the extension view format

Status: shipped in v1 (implementation mandate:
[extension UI framework](../architecture/extension-ui-framework.md)).

A **view** is what an extension publishes for Vqro to draw. It travels the same
live path as a host document: a trusted service declaring `provides =
["host_document"]` returns it from `host.document.render` for one scope, the
host validates and caches it, and the client receives it in the
`host.documents.v1` projection. `host.document.v1`/`v2` and the generation-1
endpoint codecs are unchanged and stay readable.

## Envelope

```json
{
  "contract": "host.view.v1",
  "namespace": "vqro.tasks",
  "generation": 7,
  "revision": 12,
  "surface": "navigation",
  "target": { "kind": "session" },
  "items": [ ... ],
  "actions": [ ... ],
  "required_capabilities": []
}
```

`generation`, `revision`, `surface`, `target`, `actions` and
`required_capabilities` mean what they mean in `host.document.v1`: a new
generation replaces the old one, actions are bounded and either routed back to
the publishing extension or run as a capability-gated host effect, and the
target names the scope the view belongs to.

**A view that changes must move `revision`.** Vqro re-fetches a provider's view
only when the version the provider reports for an action changes, so anything
that changes what a panel shows (a message, a task, an acknowledgement) must
move it too. An idempotent retry must leave it unchanged.

## Items

An item is a tree node. `id` is unique in the document; `parent_id` nests it
under another item (at most 8 levels); `label` is one line, truncated by Vqro
with an ellipsis; `detail` carries a short second line; `terminal` names a
terminal and Vqro joins its live name and agent state; `action_ids` reference
the document's actions; `flags` control expansion, selection and attention.

The `view` block is what makes an item drawable, and exists **only** under this
contract:

```json
"view": {
  "kind": "row",             // section | row; any other value draws as "unsupported"
  "pane_id": "w2:p2",        // panel root only: the pane this view fills
  "view_id": "tasks",        // panel root only: the user names this to `vqro pane panel`
  "state": "blocked",        // open | in_progress | blocked | reported | done
  "tone": "danger",          // optional override: default | muted | accent | success | warning | danger
  "action": "unblock",       // one word Vqro composes the waiting sentence around
  "waiting": "them",         // you | them: whose move the row waits for
  "count": 2,                // optional count in a section header
  "since": 1791072060        // Unix epoch seconds; Vqro draws the age
}
```

- A **panel root** (no parent) names both `pane_id` and `view_id` and becomes a
  section in that pane's panel. `state` on a root is the panel's aggregate.
- Any other item carrying a `view` block must have a `parent_id`, or the view is
  rejected: a block that belongs to nothing would silently never draw.
- Vqro draws the fixed Pi-style glyphs: `○` open, `◐` in progress, `⊘` blocked,
  `◎` reported, `●` done, toned with the tone set above. Extensions never send
  glyphs, colours or sizes.
- Row wording is Vqro's: the extension supplies one action word and whose move
  it is, and Vqro renders `waiting on you: report · 3m` or
  `waiting on panel-b: unblock · 3m`, joining the live name from its own facts.

## Actions

An action is a button (`operation` = `extension` or a capability-gated `host`
effect) with a `subject_id` naming the item it belongs to and an optional
`confirmation`. Every action reference must name an action in the same document,
and Vqro drops a click whose item no longer exists.

**v1 renders no actions.** Panels show and jump only; how a user answers an
action inside a panel (a 2-4 option choice, a one-line text field) is deferred
to a later slice, together with the interaction model it belongs to, so no shape
for it is frozen here. Adding one later is safe: an unknown field is ignored by
every client.

## Several clients at one pane## Several clients at one pane

One PTY serves every attached client, so panel sizes are arbitrated, never
raced:

- The pane's terminal is the **smallest** size any attached client claimed for
  it. The client with the largest panel always fits, no client's terminal is
  drawn under another client's panel, and the result never depends on which
  request arrived last.
- A claim from another boot than the current shell boot is stale and ignored.
- When no attached client claims a pane, it returns to its layout geometry.
- The tab's geometry controller applies the claims, so a client that does not
  control the tab's geometry still changes what the controller draws and what
  the pane's PTY is.
- A server that does not advertise `pane_panels_v1` draws no panels at all: a
  panel that could not claim its terminal's edge would be drawn over the
  terminal instead of shrinking it. The connection stays usable.

## User-facing entry points

- `vqro pane panel show|hide|toggle <pane> [<view>]` asks the attached clients
  to open, close or toggle a view. An older client ignores the request and keeps
  its own display state.
- A badge on the pane border, the tab bar and the sidebar row opens the panel
  for the panes it covers. A pane with views and nothing open draws its own
  badge.
- `ui.show_panels_on_agent_start` (off by default) opens a pane's first view
  when an agent starts there. Only a user action or a user-enabled rule opens a
  view.

## Bounds

512 items, 8 levels, 4096 characters of text, 256 actions, 24 characters of
action text, 2–4 choice options. Unknown fields are ignored; an unknown future
block must degrade to "unsupported" rather than reject the view.
