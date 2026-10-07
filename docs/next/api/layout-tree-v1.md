# Neutral layout tree export and session v8

Status: implemented by nested-panes session durability work. This adds no box
interaction or view content kind.

## Optional API

`layout.tree.export` uses the same target parameters as `layout.export`:
`tab_id` or `pane_id`, never both, or the active tab when omitted. It is
advertised as an optional native endpoint method. Absence disables only this
read-only action; generation-1 snapshot/surface/input/blob codecs are unchanged.

The response result contains `layout` with `kind: "layout.tree.v1"`, public
`workspace_id`, public `tab_id`, `zoomed`, `focused`, and `root`. Node objects:

- `{"type":"pane","pane_id":"w1:p1"}`
- `{"type":"stack","box_id":"container_000000000000002a","selected":0,"children":[...]}`
- `{"type":"split","direction":"right","ratio":0.5,"first":{...},"second":{...}}`

Direction is `right` (horizontal) or `down` (vertical). Stack selection is a
child index, not a pane identity; children can be panes, stacks or splits. The
empty-stack sentinel is index zero. Box IDs use the host's stable container-ID
syntax; unnamed binary splits have no allocated identity. Focus is a pane with
`pane_id` or a stack with `box_id`. Node and focus enums have `Unknown` fallbacks.

Existing `layout.export`, `layout.apply` and `layout.set_split_ratio` retain
pane-only, selected-branch response semantics. Their parameter shapes are
unchanged. The new method does not imply nested tree application support.

## Persistence and upgrade

All writers produce session version 8, independently of Collections authority.
The durable tree contains `Pane`, `Stack { id, selected, children }` and `Split`;
Collections records retain only label/archive policy, never membership or
selection. Terminal identities and public workspace/tab/pane numbering remain
host facts. History remains independently versioned at 4.

The separate one-way legacy converter admits v6 writers and v7 authority files.
An old Collection placement becomes a stack with the same stable identity,
ordered pane children, and selected child index. Split direction and ratio are
preserved. Older pre-v6 identity normalization remains available. Conversion
is a shape translation; the existing typed-layout recovery remains the single
semantic repair rule for duplicate/missing/over-limit membership, unplaced
policies, nonmember selection and archived references. It retains every admitted
pane and terminal identity instead of rejecting repairable legacy files. Unknown
schemas, invalid JSON and unsafe pane/identity admission still refuse. Restore
does not impose the new-operation depth cap on existing trees.

Stage3 nested-scope rendering is the intended consumer of this optional export.
The surface is retained for that consumer, not a new privileged product path;
remove it if stage3 does not consume it.

Untrusted session input stays bounded to 16 MiB and the existing serde_json
128-level JSON recursion budget, for version 8 as well as legacy input. This
budget applies during the first raw JSON decode, before semantic admission or
construction of a typed layout; no unbounded-recursion feature/path is enabled.
Excessive nesting refuses the complete snapshot and preserves its bytes rather
than partially restoring it. The maximal admitted legacy split chain (31 splits,
32 panes, ending in a Stack) survives conversion and a v8 reader roundtrip.

Before replacing an older session, the host creates a private, create-new sibling
`session.json.pre-v8-v<old-version>.<random>.bak` with the exact original bytes.
The backup file and containing directory are synced before publication. Failure
before replacement leaves the old file intact; every conversion/publication
failure retains the backup, including an uncertain post-replacement sync outcome.
The same guard covers ordinary saves, checkpoint writes and handoff publication.
It operates in the resolved target directory when the session is a symlink.

Handoff v3 admits explicitly session v6, v7 and v8. Legacy layouts convert before
terminal descriptor admission; schema conversion is permitted but public,
internal, structural or projection identity repair is not permitted for v3.
The receiver adopts `max(manifest.collections_action_sequence, durable checkpoint
sequence)` before descriptor admission: the previous exporter omitted that mark,
and an omission must never lower the durable replay/idempotency barrier. The
adopted mark survives subsequent captures even under legacy/unavailable authority.
Only this monotonic metadata is reconciled; no checkpoint layout or identity is
merged over the live tree. The reader extracts only version/action-sequence
metadata after bounded JSON syntax decoding; it never converts or normalizes a
stale checkpoint layout. Unreadable/invalid JSON, future versions and invalid
counter metadata refuse before descriptor transfer. Per-write version guards use
the same two-field metadata reader. Remove the reconciliation when no pre-v8
live sender can be a source.

Running terminals are imported rather than restarted. Persistence rolls forward
through durable temporary-file replacement and never automatically restores a
backup after publication. Downgrade is unsupported: future session versions are
refused, and writers refuse overwriting a higher-version file with a lower one.

## Previous live exporter field audit

The complete unconditional omitted/zeroed durable-field set on the previous
exporter is `collections_action_sequence` (reconciled above), `sidebar_width`
(None), `sidebar_section_split` (None), and `collapsed_space_keys` (empty). The
last three are dormant legacy presentation fields: no production importer reads
them; client presentation is separately owned. In
`src/client/shell/preferences.rs` / `config.rs` / `state.rs`, widths and splits
come from `ClientChromePreferences` at `state_dir()/client-shell/local-<socket-hash>.json`;
client collapses use `collapsed_groups`. Those paths do not read the durable
SessionSnapshot or `session.json`. File conversion retains the dormant fields'
values and the exact backup preserves them, but live upgrade does not invent a
new host presentation authority or claim to restore them.

All workspace/tab/Collection structural and identity fields have explicit live
capture rules. Pane agent name/kind are deliberately omitted during an
unconfirmed managed launch; agent session references come from active hook or
persisted authority, not a stale checkpoint. Cwd is live process/provenance data.
All manifest and runtime identity, PID, geometry, keyboard, input and title fields
are supplied explicitly. Existing native-agent resume omits old screen-history
replay; pane history is independent from structural session ownership. Extension
state remains its existing validated durable checkpoint; pending extension
transactions refuse handoff. Requests, subscriptions and client sockets are
volatile, not missing durable fields. No additional durable host-fact omission
was found; stale pane/agent/identity data is never overlaid from disk.

Private protocol version remains 23 and endpoint generation remains 1. This
change does not alter a type reachable from the binary protocol; session JSON
schema changes do not by themselves require a binary protocol bump.
