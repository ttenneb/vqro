# Conditional layout collapse

Status: implemented neutral JSON API, CLI and optional TUI companion/controls.
Private protocol remains 25; published binary codecs and endpoint generation 2
are unchanged. See [accepted design](../architecture/generic-pane-collapse.md).

## Methods

Optional advertised methods:

- `pane.collapse` / `pane.expand`: `{ "pane_id": "w1:p1" }`, desired state for
  that exact stable pane, including a view pane. Repeating the desired state is
  a no-op success; no ancestor or nearest terminal is substituted.
- `layout.collapse.describe`: the same target form as `layout.tree.export`
  (`tab_id`, `pane_id`, or neither; not both). Result type
  `layout_collapse_description` contains `collapse` with `kind:
  "layout.collapse.v1"`, `boot_id`, `tree_fingerprint`, and `layout`, the complete
  ordinary `layout.tree.v1` description including hidden stack children.
- `layout.collapse` / `layout.expand`: conditional desired-state edit with
  `{ "boot_id": "…", "tab_id": "w1:t1", "tree_fingerprint": "…", "path": […] }`.
  Result is the ordinary `ok` response. No box-close authority is added.

Paths are arrays of `{ "split": "first" }`, `{ "split": "second" }`, or
`{ "stack_child": 0 }`. Empty means the tab root. A collapsed wrapper is
transparent and consumes no path step. Path kinds must match the current
container, including inside selected or hidden stack branches. The bound is 128
steps; invalid/missing indices or container kinds return `invalid_layout_path`.
A missing tab returns `layout_not_found`.

The server incarnation must match the serving host and the fingerprint must
match the **independently recomputed complete current tree** in the same App
actor operation as mutation. Stale tokens return `stale_layout_target` without
mutation; the host never retargets by a nearby pane. A desired-state no-op still
requires the complete fence and path to validate. Persistence/checkpoint fences
reject with `layout_mutation_blocked`. There is no asynchronous gap between
validation and edit. Explicit authorized terminal-write APIs are unaffected.

The fingerprint is SHA-256 over a domain-separated, length-delimited structural
encoding covering all pane/view content and stack identities, hidden membership,
order, selection, split orientation/shares and collapse. It excludes terminal
bytes, provider text and presentation geometry. A derived cached value is
refreshed at canonical tree mutation boundaries for projection; conditional
mutation independently verifies current state even if that cache is stale. It
is concurrency provenance, not a stable box identity or authorization token.

## Optional TUI companion

Clients request `layout.collapse.v1` in hello `control_kinds`; servers advertise
`layout_collapse_v1` and send existing
`EndpointControl { kind: "layout.collapse.v1", data: JSON }` messages. Data
contains `boot_id`, `projection_revision`, `base_surface_revision`, and `body`:
current `tab_id`/`tree_fingerprint`, surface `width`/`height`, `enabled`, optional
`unavailable`, `headers`, `anchors`, and `containers`. Header entries carry a
surface-relative `rect`, `action_rect`, tagged `path`, `collapsed` and `focused`;
anchors map visible leaf-header/body `pane_id` to a path; containers carry a path
and `stack` observation. A collapsed leaf-header anchor is not a body descriptor:
it enables the existing stable-pane menu, never ordinary typing. These are ephemeral conditional addresses, not new box identities.

A client installs a full surface and companion only as an exact boot, projection,
base-revision, tab and geometry pair, in either arrival order. Verified patches
advance the admitted base; metadata-only changes force a full pair. Newer pending
metadata disables body/header input until coherent. Existing held releases still
reach their original target; suppressed presses acquire no forwarded lease.
Collapsed bodies have no pane/view descriptors, body interests or graphics.
Independent explicit viewers retain their own interests. Legacy peers never wait
for metadata and receive safe raster without new controls.

Projection is bounded to 4,096 combined headers/anchors/containers, 128 path steps,
validated rectangles and half the transport frame byte budget. Invalid or
unavailable optional metadata disables only collapse controls; visible ordinary
body input and the connection remain usable. Controls also require the explicitly
advertised `layout.collapse`/`layout.expand` methods.

Existing framed headers get a collapse hit cell. Anonymous expanded boxes do not
get new frames: use **Collapse box** in a child header's context menu, the ancestor
picker, or CLI. The picker exposes only collapse, not box-close/selection authority.
Collapsed headers/strips expand directly. Right click offers Expand only for a
generic container, or the existing pane menu/close authority for a leaf; it adds
no anonymous box-close scope. Configurable `[keys]` defaults are
`collapse_pane = "prefix+m"` (collapse pane or expand focused header) and
`collapse_box = "prefix+shift+m"` (choose a containing box).

## CLI

`vqro pane collapse|expand <pane_id>` edits the pane. Add `--box` to edit its
nearest containing box: the CLI obtains a fresh description, walks full tagged
split/stack paths, and issues the fenced mutation. A root pane has no containing
box and refuses `--box`; it does not silently become a box operation. A stale
race returns the server rejection instead of retrying a potentially different
subtree. Other ancestors are addressable through the public conditional API.

Collapse releases vertical space as one header row or horizontal space as a
one-column title strip, preserving remembered shares. Root collapse can leave
unused space. Expansion first restores those shares, with transient auto-sizing
resume fences as specified by the accepted design. Shared pane/terminal identity,
PTY parsing and last hidden nonzero terminal size remain intact.
