# `host.view.v1`: the extension view format

Status: shipped in v1 (implementation mandate:
[extension UI framework](../architecture/extension-ui-framework.md)).
Presentation is by nested panes: a **view pane** is a pane whose content is an
extension view (stage 3, [nested panes](../architecture/nested-panes.md)); the
pane-attached split band and its per-client claims are gone.

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
that changes what a view shows (a message, a task, an acknowledgement) must
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
  "pane_id": "w2:p2",        // root only: the terminal pane this view was published for
  "view_id": "tasks",        // root only: the view id a view pane names as (extension, view_id)
  "state": "blocked",        // open | in_progress | blocked | reported | done
  "action": "unblock",       // one word Vqro composes the waiting sentence around
  "waiting": "them",         // you | them: whose move the row waits for
  "since": 1791072060        // Unix epoch seconds; Vqro draws the age
}
```

- A **view root** (no parent) names both `pane_id` and `view_id`. It is one
  terminal pane's slice; a view pane composes the roots whose `pane_id` is in
  its placement-derived scope. `state` on a root is the slice's aggregate.
- A section's summary is `detail`, and a row's tone comes from its `state`.
  Per-item `count` and `tone` overrides are **deferred**: no view sends them,
  and an optional field can be added later without a codec break.
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

## View panes and scopes

A view pane is an ordinary layout pane: a stable pane identity, a rect, focus,
zoom, move and close through the same layout engine as a terminal pane, and the
content kind `(extension, view_id)` instead of a PTY. It has no terminal.

- **Scope comes from placement, not configuration.** A view pane covers every
  terminal in its nearest parent container, in tree order. A view at the tab
  root covers the terminals of the root subtree. The host derives the scope from
  the shared layout, so every client sees the same set.
- The header always shows the scope: `Tasks · agent1` for one covered terminal,
  `Tasks · 3 terminals` for several.
- The composition above is still per terminal pane: the host publishes each
  terminal's slice as a view root, and a view pane composes the roots its scope
  covers. The extension keeps the meaning; core supplies only the covered
  terminals. Re-keying the request to a scope in the host<->extension contract is
  a later, separate change.
- **Frame.** One outer terminal perimeter encloses its unframed terminal body
  and fully framed attached views, side by side or stacked inside the bottom.
  Membership includes hidden children: only a maximal subtree with exactly one
  terminal and views scoped to it qualifies. Other boxes keep normal pane
  frames, independent of names. A standalone/zoomed view keeps its full frame.
  See [Nested panes](../architecture/nested-panes.md#frame-style).
- A view pane that is not visible in the composed tab does no presentation work;
  background panes still parse output as today.

## Delivery and negotiation

Views travel in the `host.documents.v1` projection, which is a published codec.
A client advertises the kinds it can parse in its handshake; a client that lists
`host.view.v1` receives view entries, and one that does not is sent legacy
entries only. That is not a nicety: the projection is validated as a whole, so an
older client that received a view entry would reject the projection, not ignore
it. A server must filter the projection per client, and a separate optional
control would be an equally safe carrier.

## Compatibility: unknown blocks

`kind` is a free string, never a closed enum: an unknown variant would make an
older client reject the whole view, while an unknown string lets it draw
`unsupported <kind>` **in place** — the row keeps its tree position and label,
and a section that Vqro cannot draw still reserves its edge and says so. The
block set can therefore grow without breaking an older surface.

`unavailable` is Vqro-owned too: when a view pane's provider stops publishing it
(the extension failed or was disabled), Vqro keeps the pane and draws one
`extension unavailable` row, instead of silently dropping the pane. When the
provider returns, the view is drawn again.

## User-facing entry points

- `vqro pane view add <pane> <extension>:<view> [--direction right|down]
  [--ratio FLOAT] [--focus]` adds a view pane beside a pane. The ratio is the
  **view's share**, in either direction (0.1 through 0.9), before
  child border insets and subject to integer cell quantization. Omitting it
  opts a new qualifying view into transient automatic sizing from accepted,
  cached neutral document facts (initial tree fallback 0.5). Explicit ratios
  and divider drags opt the affected view out; directional pane resize opts
  its terminal group out. New defaults do not rearm earlier manual choices.
  AUTO bounds may produce a finite interior share below ten percent; only
  explicit manual/API ratios have the 0.1–0.9 limit. Restart/handoff preserves
  that last actual ratio unchanged, never the auto policy.
  `vqro pane view
  close <view-pane>` drops it. Both are neutral: core stores the content
  identity and the placement, never a Tasks literal, and a view pane keeps a
  public pane identity like any pane.
- A badge on the tab bar and the sidebar row selects its tab; the recorded
  decision is that a badge has no action of its own.

## Image boundary verification

The real-parser side/stacked Kitty test can export its actual host frame,
accepted image bytes, clipped placements and body/view rectangles when
`VQRO_IMAGE_EVIDENCE_ROOT` names a private scratch directory. The optional
`scripts/replay-kitty-surface.py --font <installed-monospace-font> <side.json>
<stacked.json>` replays those records with already-installed Pillow. It does
**not** add a destination body clip: positive cases require zero changed pixels
outside the terminal body or anywhere in the sibling view; oversized/title
negative controls must detect leaks. It writes full and seam PNGs plus hashes.
This is host-scene pixel evidence, not native GUI/font acceptance or a claim
that client-owned view rows were rasterized by the replay. Native appearance
and human visual acceptance remain separate gates. No installs are performed.

## Bounds

512 items, 8 levels, 4096 characters of text, 256 actions, 24 characters of
action text, 2–4 choice options. Unknown fields are ignored; an unknown future
block must degrade to "unsupported" rather than reject the view.
