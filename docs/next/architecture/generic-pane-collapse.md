# Generic pane and box collapse

Status: accepted design (2026-11-05). Canonical tree/session and compact geometry
are implemented; client controls and the optional companion below are not yet
advertised. This does not grant box naming, selection, or new box-close authority.

## State, membership, and recovery

Collapse is one host-owned shared layout fact, not view-specific state. A
`TypedNode::Collapsed { node }` wraps any ordinary pane, split or stack. It
preserves identities, complete membership, split shares, stack selection and
focus history. Structural paths and box-depth accounting ignore the wrapper.
Adjacent wrappers are invalid in a canonical tree and coalesce during recovery
or close-to-single-child promotion. Empty containers retain their existing
normalization and authority rules; collapse cannot manufacture a terminal.

Session v10 persists the matching JSON `Collapsed { node }` wrapper. Previously
admitted v0–v9 input remains supported, with absent collapse meaning expanded;
history stays v4. Backup-before-conversion, identity admission, live descriptor
transfer and downgrade refusal remain unchanged. Frozen input/codec fixtures are
not rewritten: v10 expectations are separate. See [tree persistence](../api/layout-tree-v1.md).
Neither this wrapper nor the session version requires a bincode change.

View-only containers inherit the nearest enclosing container with nonempty FULL
terminal membership. Hidden and collapsed members count; nonempty multi-terminal
boundaries retain their meaning. Without an enclosing terminal-bearing container,
scope is empty. Moving a view group rederives scope, never stores a terminal pin.
Frame qualification, sizing and view summaries use this same rule. See
[view scope](../api/host-view-v1.md#view-panes-and-scopes).

## Geometry and size memory

In a vertical split a collapsed child takes one header row. In a horizontal split
it takes one narrow column with an upright vertical or truncated title. Display
clipping must not let wide/combining/emoji clusters paint into the sibling.
Expanded siblings receive the released space. Fully collapsed nested subtrees
have compact extents too; both-collapsed, root and tiny rectangles saturate
without negative sizes. Only an ancestor header is visible when it is collapsed.
A root header can leave unused space.

Effective allocation never writes near-zero shares over expanded size memory.
Expand restores those shares, proportionally scaled after viewport changes.
Ineffective dividers cannot rewrite them. No second persistent pixel-size store
or automatic sizing policy is introduced.

Already-armed automatic views require a transient resume fence on affected
allocation edges. Expansion records accepted metric/source/availability facts,
controlling dimensions/cell size and host-owned async issue/notification
watermarks; pre-expand pending geometry is discarded. The sizing adapter must
filter fenced edges before consuming cached measures. Repaint, attachment,
equal-metric publication, same-size controller replay, or a delayed pre-expand
completion cannot release the fence. Only a genuinely later relevant changed
fact originating after the watermark, a changed controlling viewport, or an
explicit sizing action can resume. Fences follow canonical edge membership or
are explicitly invalidated on topology edits, not merely on fingerprint churn.
Manual opt-out and restored unarmed views must never be rearmed by expansion.

## Optional client projection and conditional targets

The proposed optional **`layout.collapse.v1`** JSON endpoint control uses existing
`EndpointControl { kind, data }`, with no change to frozen `shell.surface.v2`,
`PaneSurfacePatch`, snapshot, input or key-action wire shapes. Clients advertise
support in hello. Legacy peers do not wait for the companion and have no collapse
controls. Any required published-payload change needs a separate codec decision.

A per-client companion carries boot/projection/full-base-surface revision,
current tab, authoritative tree fingerprint, visible header/strip rectangles and
exact targets, plus bounded ancestor targets anchored to visible pane headers.
It is derived in the SAME geometry pass as its full surface and paired only
after the full surface revision is assigned. Suppression includes metadata, not
just raster equality. Initial attach, changed header geometry, topology, collapse
or selection sends a full pair even when raster cells happen to match.

The client stages a bounded newest pair and admits it only when boot,
projection, tab, base revision and validated geometry agree. Either writer-queue
order is valid. Ordinary patches retain metadata only through the verified base
revision chain; metadata changes require a new full pair. Missing/stale pairs
cannot enable header/body input or fall through to PTYs. Disconnect/tab switch
clears pending targets. A known newer companion invalidates old interactions
even if its projection revision did not advance. Collapsed subtrees supply no
body pane/view descriptors, row flattening, terminal snapshots or graphics.

Conditional subtree targets contain `{ boot_id, tab_id, tree_fingerprint, path }`.
Path steps are tagged `{ split: "first" | "second" }` or
`{ stack_child: unsigned_index }`; empty means root. Wrapper traversal consumes
no step. Wrong boot/fingerprint/kind/index or missing targets reject unchanged,
never resolve to a nearby pane. The App actor independently verifies current
complete tree state and performs the desired-state edit without an intervening
await. Fingerprints cover IDs, content, full hidden membership, order, selection,
orientation, shares and collapse, not titles or terminal bytes. Mutation/event
boundaries cache them; no per-render tree request or hashing is permitted.

A new optional `layout.collapse.describe` method supplies fresh fenced addresses;
existing `layout.tree.v1` export gains only observational optional collapsed
state. Stable pane-ID and fenced subtree desired-state methods share one internal
mutation. There is no retry-inverting toggle API and no new stable box identity.

## Input, summaries, and execution

Existing framed panes/groups get a collapse hit cell distinct from close/drag.
An anonymous expanded split gets no new frame/header: use **Collapse box** in an
existing child-header context menu, the matching ancestor-picker key, or CLI.
Collapsed headers/strips expand directly. A client-local active header uses its
fenced target, not a new shared anonymous-box focus type. Cold restore/attach and
remote collapse must establish this state, not rely on the initiating client.
Ordinary typing has no hidden-body target; held key/mouse release cleanup remains
intact. Explicit authorized terminal-write APIs keep their execution semantics.
Close appears only where an existing authorized close path and process
confirmation apply, never a newly invented generic anonymous-box close action.

View headers reuse accepted package root label/detail, bounded to 160 Unicode
scalars, single-line and control-free, cached by source/scope/incarnation and
invalidated on withdrawal or movement. Do not infer Tasks/Messages semantics in
the host or flatten rows just to get a summary. Terminals use narrow cached title
facts; box titles remain bounded neutral presentation. Render is pure and makes
no provider calls. Collapsed terminals keep parsing/detection and their last
nonzero PTY dimensions, with no body presentation work. Sibling collapse/expand
uses the existing single geometry/resize authority. Independent explicit viewers
retain their own interests.

## Default composition

Agent start adds one ordinary terminal-free box BELOW the terminal, containing
Tasks above Messages. Admission is atomic at the pane/depth caps; the existing
once-per-terminal marker preserves close intent across restart. Existing views
are not silently moved. Configuration can disable the rule or choose its views.
The outer frame belongs to the agent group, the terminal body is unframed, and
view children contribute inner frames. Existing layout operations can place the
whole box beside the terminal; no new expanded side-width cap is chosen.
