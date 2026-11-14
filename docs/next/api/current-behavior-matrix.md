# Current layout, input and message behavior

Status: **current implementation inventory**, audited against `f1575377`
(2026-11-14). This matrix distinguishes shipped behavior, source-observed
limitations and proposed work. It does not create a codec, source, input lane,
message outcome or mutation authority. Accepted contracts linked below remain
the authority for their surfaces; a design's acceptance alone is not availability.

## Availability and supersession matrix

| Surface | Implemented behavior / availability | Contract and supersession |
|---|---|---|
| Canonical layout | One split/stack/pane/view tree, with transparent collapsed wrappers and complete hidden membership. Session v10 persists it. Stack IDs do not confer anonymous split identity. | [Tree export and persistence](layout-tree-v1.md); supersedes the historical Collection-leaf baseline and v8 writer description in [Nested panes](../architecture/nested-panes.md). |
| Complete inventory | `layout.tree.export` / `vqro pane layout --tree` includes all hidden stack branches, view leaves and collapsed flags. Read-only observation, not a conditional edit token or nested apply API. | [Inventory workflow](#diagnostic-and-inventory-default); [tree contract](layout-tree-v1.md). |
| Legacy layout projections | `layout.export` is terminal-only, selected-branch and **not a full-tree round trip**. `pane.layout` is a selected geometry report; snapshots are not a box inventory. Existing methods and bare CLI output retain their shapes. | [Legacy boundary](layout-tree-v1.md#diagnostic-and-inventory-workflow); no silent reinterpretation of advertised methods. |
| Generic collapse | Conditional pane/subtree collapse, preserved shares/membership, optional paired TUI metadata and compact geometry are implemented. Side collapse needs three usable columns, normally five outer when framed, with contextual chrome. | [Collapse API](layout-collapse-v1.md) and [accepted geometry](../architecture/generic-pane-collapse.md#geometry-and-size-memory) supersede the old one-column strip clause. |
| Agent-start views | Default Tasks-above-Messages group always below the terminal; atomic layout admission, once marker and transient sizing. Explicit layout edits can move it beside. | [Agent-start contract](agent-start-views.md) supersedes the aspect-dependent single-Tasks promise in [Nested panes](../architecture/nested-panes.md#agent-start-rule). |
| Stage 3 versus stage 4 | View panes and placement-derived scope ship; evaluation still composes per-pane extension roots. Core absorption of Collections, generic box naming/selection/close and new anonymous box IDs do **not** ship. | [Stage 3 notes](../architecture/nested-panes.md#stage-3-implementation-notes-2026-10-07); [Collections authority](../architecture/collections-authority-transition.md) remains in force. |
| Terminal input today | Paired collapse guards and visible client-refusal notice ship. Normal input names a pane (popup input a terminal), not a rendered runtime incarnation/route epoch. No end-to-end batch disposition guarantee. | [Current input rules](#input-authority-and-admission); [surface codec decision](../architecture/endpoint-surface-codec-v2.md). |
| Atomic scene / all-input exact lane | **Proposed implementation, unavailable at this baseline.** Atomic scene direction is accepted; one-event installation and separate route authority are in progress. Exact-runtime envelopes, transactional prefix/held admission and dispositions are a later lane, not supplied by pairing or atomic publication alone. | [Tracked scene decision](https://github.com/ttenneb/vqro/blob/00c91140f1a1a344169eb422bffaf11eec984846/docs/next/architecture/atomic-client-scene-v1.md) and [availability](https://github.com/ttenneb/vqro/blob/00c91140f1a1a344169eb422bffaf11eec984846/docs/next/api/client-scene-v1.md), committed separately, not implemented/installed here. Existing [endpoint obligations](../../../AGENTS.md#stable-client-endpoint-contract) still apply. |
| Deferred-only lifecycle (B) | **Parked, superseded direction; unavailable.** BEGIN/RESUME/tail/END, lifetime CompletedExact and recovery scaffolding are not a shipped input fix or fallback. | No such optional method/codec is advertised by this baseline. Do not treat a parked branch or proposal review as availability. |
| Durable messages | Exact full recipient binding controls list/claim. Waiting → claimed is durable, but claim is not SDK delivery, execution, reply or acceptance. Durable delivery-attempt/attention/recovery outcomes are **not yet available** here. | [Task conventions](task-conventions.md); [message limitations](#message-claim-and-reply-correlation). |
| Automatic sizing | Explicit creation provenance plus transient armed state, cached measures and resume fences; only actual ratios persist. Restore/handoff is AUTO **off**. | [Sizing contract](../architecture/nested-panes.md#transient-automatic-sizing) and [collapse fences](../architecture/generic-pane-collapse.md#geometry-and-size-memory). No persistent mode/pin or consolidated sizing state machine is introduced. |

## Diagnostic and inventory default

Start structural diagnosis with the **complete tree**, not a visible terminal
projection:

```sh
vqro pane layout --tree
vqro pane layout --pane w1:p1 --tree
```

For native JSON clients:

```json
{"id":"inspect-layout","method":"layout.tree.export","params":{"tab_id":"w1:t1"}}
```

Walk every `children`, `first` and `second` branch, not only the selected stack
child. Hidden/collapsed is visibility, not deletion. If the optional tree method
is absent, report complete inventory unavailable; do not silently substitute a
selected-only projection. Use `layout.collapse.describe` separately when a fresh
conditional collapse/expand fence is needed.

This is the default **workflow**, not a change to bare `vqro pane layout` JSON.
Bare CLI still calls `pane.layout`; replacing that result with `layout.tree.v1`
would break existing geometry consumers/parsers. `layout.export` likewise stays
terminal-only. It can omit the actual focused view and cannot recover views,
hidden members, collapse state or complete container structure through apply.

## Input authority and admission

### Eligible, focused, selected and rendered are different facts

**Implemented rules, not a focused-only admission contract:**

- Server eligibility uses the client's selected workspace/tab and the canonical
  expanded-pane fact; effective body zoom additionally restricts to focus.
  An expanded Collection preview need not be the selected stack member or the
  focused pane. Full tree membership alone does not grant ordinary input.
- When the optional collapse companion is negotiated, client classification
  also requires a current coherent surface/companion and an eligible visible
  body. A collapsed header anchor is not a body target. Legacy peers do not wait
  for this optional metadata.
- Committed text/paste in an unentered Collection list is consumed by that UI.
  An entered, expanded member can receive it. A focused inspector consumes it;
  an unfocused inspector with a visible body can route it to its pane. Otherwise
  normal focused-pane routing applies. Views and popups have their own handling;
  they are not a license to write to every tree member.

Source witnesses: [client classification](../../../src/client/shell/input.rs),
[collapse guard](../../../src/client/shell/collapse.rs),
[server eligibility](../../../src/server/headless/client_views.rs) and
[host dispatch](../../../src/server/headless.rs).

### Route revocation versus presentation churn

**Current limitation:** snapshot, structure, surface and collapse metadata are
separately installed events. Broad projection changes (including task/status or
document-only changes) can close the client guard while old pixels remain.
A paired socket write is not atomic client installation. The refusal notice is
client-local evidence only, not proof that every server rejection is surfaced.

Observed server rejection boundaries include inactive shell/handoff, missing
pane/runtime, another selected workspace/tab, collapsed ancestry, effective zoom
and popup ownership. Current pane resolution can accept historical aliases; it
is not proof of the incarnation that produced the displayed pixels.

**Accepted direction, implementation pending, not a current guarantee:** the
separately committed
[closed scene route dependencies](https://github.com/ttenneb/vqro/blob/00c91140f1a1a344169eb422bffaf11eec984846/docs/next/architecture/atomic-client-scene-v1.md#closed-route-dependencies-first-shipment)
separate route authority from scene/raster revision. They cover:

1. Boot/connection generation, activation/handoff and selected location/focus.
2. Exact terminal attachment/runtime, popup ownership and lifecycle changes.
3. Selected canonical membership, collapse/zoom, content identity and view scope.
4. Collection membership/selection and expanded-preview ownership.
5. Dimensions, target/hit rectangles and local viewport/resize generation;
   geometry conservatively revokes **keyboard as well as mouse** in the first
   proposed shipment.
6. Input interpretation modes, popup blocking and local overlay/inspector/list
   or endpoint ownership (local state is not host data).

Task/status/agent observations, labels, document bodies, terminal cell content
and provider refresh are not dependencies unless actual structural eligibility
changes. See the linked decision for the closed list and exclusions, not the
current broad projection guard. The permalink records the accepted design
without implying that its new codec, atomic installation or source availability
has landed on this baseline. No exact-runtime input admission is supplied by
scene publication alone.

Source witnesses: [publication](../../../src/server/headless/render.rs),
[transport scheduling](../../../src/server/client_transport.rs),
[one-message client events](../../../src/client/transport.rs).

### Local consumption, empty encoding and partial prefixes

**Implemented behavior:** plain unmodified PageUp/PageDown can operate host
scrollback (when the terminal mode permits), not the PTY. Encoders can emit zero
bytes; mouse scroll can also be consumed locally. These are not by themselves
lost terminal input. Other events are encoded and queued individually in order.
A later error can follow an already queued prefix; the batch is not atomic and
cannot be treated as wholly rejected or safely replayed. Queue admission is not
OS write, program execution or accepted outcome.

**Held-state limitation:** the host currently tracks held presses before the
encoder/queue result. A local no-op or failed suffix can therefore leave
speculative ownership. Publishing held state for only a proven admitted prefix
is proposed exact-lane work, not current behavior.

Source witnesses: [event encoding/admission](../../../src/server/pane_input.rs),
[dispatch order](../../../src/server/headless.rs),
[held tracking](../../../src/server/clients.rs).

### Release identity across lifecycle changes

**Current limitation:** client release leases retain the recorded pane/popup
target and releases bypass the press guard so route changes do not suppress
cleanup. Host teardown cleanup resolves stored pane/terminal strings again.
Those strings do not identify a runtime incarnation after respawn, relocation,
deactivation or disconnect; current cleanup is not an exact-runtime guarantee.

**Proposed/unavailable:** press identity should own a weak, non-lifecycle-owning
reference to the admitted runtime; repeats validate current authority, releases
clean only that proven press on that still-live incarnation, never an alias or
replacement. The exact lane must cover teardown as well as ordinary key-up.

Source witnesses: [client leases](../../../src/client/shell/input.rs),
[host held targets](../../../src/server/clients.rs),
[disconnect cleanup](../../../src/server/headless.rs).

## Message claim and reply correlation

**Implemented authority:** incoming list/history and claim compare the complete
normalized terminal/agent/execution recipient binding, not execution alone.
`reply_to` validates that a referenced message exists; it is a cross-reference,
not proof of reversed sender/recipient relationship or conversation ownership.

**Delivery gap:** claim commits before SDK delivery or before-run context
contribution. A crash or failed/currentness-lost delivery can leave a claimed
record outside the waiting queue. The adapter's `messages-undelivered` session
entry is not a durable Tasks delivery outcome. Neither an automatic replay of an
uncertain SDK effect nor retargeting to a successor execution is justified.
An authoritative delivery-attempt/attention path and explicit original-record
recovery remain separate proposed work, unavailable at this baseline.

**Correlation, not causal proof:** automatic reply ownership checks an
untruncated observed prompt for a message marker substring. Follow-up input
revokes ownership conservatively. Explicit-reply suppression recognizes
message-shaped bash output with matching execution, waiting state and revision
one; it does not prove a committed send operation or compare the full recipient
binding. Copied markers or printed JSON can satisfy those heuristics. This is a
trusted cooperative adapter limitation, not an authenticated message boundary.
Explicit `message send --reply-to` remains available. An opaque SDK delivery ID
and structured committed-send correlation are future work, not new authority.

Sources: [provider](../../../extensions/tasks/messages.py),
[Pi policy](../../../extensions/tasks/pi.ts), and
[shipped conventions](task-conventions.md). This qualifies the conventions'
positive-ownership terminology; it does not replace the full-binding checks.

## Geometry and sizing provenance

**Implemented tie-break:** when both split children are compact, tree-first gets
its bounded extent before tree-second gets the remaining bounded extent. Focus
does not reorder that priority. Framing depends on the inherited terminal group;
three usable header columns normally need five outer framed columns, not a
universal one- or five-column allocation. Impossible geometry can clip or give
zero extent. See [geometry](../../../src/layout/frames.rs) and the
[accepted collapse contract](../architecture/generic-pane-collapse.md).

**Implemented provenance:** a terminal-free tree is not automatically an AUTO
views box. All leaves must retain the same explicit creation provenance, and
that provenance is separate from active cached measures and resume fences.
Move/swap/close normalization and manual resize can invalidate that eligibility;
a structurally similar new split must not acquire it merely by containing views.
Adding another default does not rearm an older manual choice. Whole-box collapse
uses resume fences; an individual child's collapse/expand refits an already-armed
box without rearming a manual/restored box or clearing an enclosing fence.
Only actual ratios persist; restart/handoff restores AUTO off.

Source witnesses: [creation membership](../../../src/layout.rs),
[transient state](../../../src/view_sizing.rs),
[event application](../../../src/app/view_sizing.rs). A consolidated per-group
Manual/Auto/Suspended state machine and common scope oracle are proposed cleanup,
not a second canonical tree, persistent policy, pin, box ID or shipped feature.
