# Conditional layout collapse

Status: implemented neutral JSON API and CLI. TUI companion and input controls
are not yet advertised. Private protocol remains 25; published binary codecs and
endpoint generation 2 are unchanged. See [accepted design](../architecture/generic-pane-collapse.md).

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
