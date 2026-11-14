# Neutral layout tree export and session durability

Status: implemented. Session v8 established canonical nested trees; session v9
adds view leaves. Session v10 adds generic collapsed-node state without adding
stage-4 box selection, naming, or close authority.

## Optional API

`layout.tree.export` uses the same target parameters as `layout.export`:
`tab_id` or `pane_id`, never both, or the active tab when omitted. It is
advertised as an optional native endpoint method. Absence disables only this
read-only action; generation-1 snapshot/surface/input/blob codecs are unchanged.

The response result contains `layout` with `kind: "layout.tree.v1"`, public
`workspace_id`, public `tab_id`, `zoomed`, `focused`, and `root`. Node objects:

- `{"type":"pane","pane_id":"w1:p1"}`
- `{"type":"view","pane_id":"w1:p2","extension":"vqro.tasks","view":"tasks"}`
- `{"type":"stack","box_id":"container_000000000000002a","selected":0,"children":[...]}`
- `{"type":"split","direction":"right","ratio":0.5,"first":{...},"second":{...}}`

Direction is `right` (horizontal) or `down` (vertical). Stack selection is a
child index, not a pane identity; children can be panes, stacks or splits. The
empty-stack sentinel is index zero. Box IDs use the host's stable container-ID
syntax; unnamed binary splits have no allocated identity. Focus is a pane with
`pane_id` or a stack with `box_id`. Node and focus enums have `Unknown` fallbacks.
Every node can carry optional `"collapsed": true`; absent means expanded and
expanded nodes omit the field. Collapse preserves the node's kind and children.
The export is observational, not a conditional mutation token; see the
[generic collapse contract](../architecture/generic-pane-collapse.md).

### Identify a collapsed box and its hidden members

Use `vqro pane layout --tree` (active tab), or
`vqro pane layout --pane w1:p1 --tree` for a pane's tab. This explicitly requests
`layout.tree.export`, not the legacy geometry report. Walk **all** `children`,
`first` and `second` branches, including unselected stack children. A collapsed
node retains its kind, stable box ID when it is a stack, selected child index,
saved split ratios, and all shell/view member IDs; `collapsed: true` hides their
bodies, not their membership. Nested collapsed flags distinguish an expanded
outer box from a still-collapsed selected child. The result is structural, not a
report of current pixel/cell rectangles.

Native JSON clients can request
`{"id":"inspect-layout","method":"layout.tree.export","params":{"tab_id":"w1:t1"}}`.
For fresh conditional collapse/expand addresses, use `layout.collapse.describe`.
Neither `vqro pane layout` without `--tree` nor `vqro api snapshot` is a complete
box/member inventory; their existing selected-only semantics are unchanged.

Existing `layout.export`, `layout.apply` and `layout.set_split_ratio` retain
pane-only, selected-branch response semantics. Their parameter shapes are
unchanged. The portable terminal-only `layout.export` omits view leaves and
collapses branches whose selected path has no terminal, without searching hidden
stack members. Its `focused_pane_id` remains the **actual host focus**, which can
name an omitted view; clients must not infer a first-terminal focus from the
exported tree. A view-only tree has no portable terminal layout. The new method
does not imply nested tree application support.

## Pane content kind

A pane holds either a terminal or an extension view, so the `PaneInfo` returned
by `pane.get`, `pane.focus` and the move responses names its content:

- `content` is `terminal`, `view`, or `unknown` for a kind the client does not
  know. An absent `content` means `terminal`, so a terminal pane's info is
  unchanged and a view pane always carries `content: "view"`.
- `terminal_id` is absent for a view pane, never the empty string.

## Persistence and upgrade

All writers produce session version 10, independently of Collections authority.
The durable tree contains `Pane`, `View { pane, extension, view }`,
`Stack { id, selected, children }`, `Split` and `Collapsed { node }`;
the wrapper adds no identity, path step, scope boundary, or box depth. Adjacent
wrappers coalesce during recovery, and deleting/promoting a child preserves the
survivor's collapsed state. Older trees default expanded; all previously admitted
v0–v9 input conversions remain available. The v9 view-leaf and older frozen input
fixtures are retained, with separate v10 writer/roundtrip expectations.

Collections records retain only label/archive policy, never membership or
selection. Terminal identities and public workspace/tab/pane numbering remain
host facts. History remains independently versioned at 4.

The separate one-way legacy converter admits v6 writers, v7 authority files,
and predecessor v8 trees. A valid view has no terminal row. An ambiguous view
reference sharing an ID with a saved terminal row is repaired in favor of that
existing terminal before runtime admission; duplicate placement is pruned by the
usual recovery, without allocating a new identity or exceeding pane caps.
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
128-level JSON recursion budget, for versions 8/9/10 as well as legacy input. This
budget applies during the first raw JSON decode, before semantic admission or
construction of a typed layout; no unbounded-recursion feature/path is enabled.
Excessive nesting refuses the complete snapshot and preserves its bytes rather
than partially restoring it. The maximal admitted legacy split chain (31 splits,
32 panes, ending in a Stack) survives conversion and a v8 reader roundtrip.

Before replacing an older session, the host creates a private, create-new sibling
`session.json.pre-v10-v<old-version>.<random>.bak` with the exact original bytes.
The backup file and containing directory are synced before publication. Failure
before replacement leaves the old file intact; every conversion/publication
failure retains the backup, including an uncertain post-replacement sync outcome.
The same guard covers ordinary saves, checkpoint writes and handoff publication.
It operates in the resolved target directory when the session is a symlink.

Handoff v3 admits explicitly session v6, v7, v8, v9 and v10. Legacy layouts convert before
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

## Recovery images and unavailable terminals

Ordinary saves, checkpoint publication, handoff publication and intentional clear
preserve best-effort rolling structural recovery images in `session-snapshots`
beside the resolved session target. At most 48 owned images are retained, with a
15-minute cadence and unchanged-image suppression across process restarts. Clock
rollback does not indefinitely suspend preservation. Unrelated user files,
symlinks and incomplete copies are not pruned. Copy failure never blocks a healthy
primary save or clear. These images are manual recovery material, not authority:
startup never selects an arbitrary copy or replays package state from one. The
fail-closed rejected-load policy and byte-exact conversion backups still apply.

Missing saved directories and failed shell launches retain their nested/hidden
leaves, stable terminal identity, selection, labels, cwd and agent session facts
without executing in a fallback directory. A terminal without a runtime displays
a restore error; fixing the directory/shell and restarting retries the saved
facts. Imported live terminals are not restarted merely because their saved cwd
is unavailable. Saved checkout membership survives unavailable/unreadable Git
metadata; only positive evidence of a different repository at that exact checkout
rejects membership. Explicit checkout provenance never follows a shell's cwd.

History v4 adds optional `layout_fingerprint`, a deterministic SHA-256 binding of
the complete structural image, including hidden/nested layout and normalized
unordered maps/sets. Only matching bound history is replayed. Unbound legacy or
mismatched history is ignored without rejecting an otherwise valid structural
session. This digest is presentation provenance, not per-node identity or package
authority; it cannot lower the action high-water barrier.

## Host shutdown and launching-shell hangup

Host shutdown takes precedence over queued pane deaths, agent-exit observations,
due autosaves and new API mutations, so the final save sees intact layout and
resume facts. Still-planning Collections actions/creates are canceled; an already
publishing checkpoint is settled only through its existing persisted-completion
event and authority/mutation fences before the final ordinary capture. Death,
agent and new provider-plan events are not applied during settlement. A bounded
settlement timeout or checkpoint error reports a failed final save, retains
recovery evidence and never force-unblocks an uncertain write. Linux uses
logind's delay inhibitor until that save decision completes;
unavailable/lost logind or bus connections retry without blocking normal use.
Windows probes the current session's shutdown/logoff state rather than guessing
from child exit codes. macOS observes loginwindow restart/shutdown announcements,
cancellation and point of no return before background agents die; undocumented
or unavailable notifications degrade to the normal SIGTERM save path.

Current JSON handoff manifest v3 adds optional `host_shutdown_intent` with
`announced_at_ns` in the boot-wide monotonic clock domain. Absent/null means no
pending announcement; writers omit None. Import adopts it before ownership and
before starting the new monitor. It is not session/package authority and changes
no terminal/bincode codec. The frozen v1 manifest remains an unchanged input
fixture. Announcements/cancellations arriving after manifest capture but before
replacement monitoring still reach only the old process; native platform trials
must cover restart/handoff ordering. No cross-protocol live handoff is implied.

A server survives SIGHUP and the closing terminal/shell that launched it; new
clients can reconnect. SIGINT, SIGTERM and `server.stop` remain explicit save/exit
requests. The early hangup handler resets on exec, preserving normal child-shell
signal semantics. Unit server loops use injected probes/flags rather than
watching the real host; the ignored Linux integration harness uses only private
D-Bus/logind and a disposable server. Native macOS/Windows shutdown remains a
separate acceptance trial, not a consequence of cross-build success.

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

The original session-v8 tree change used private protocol 23 / endpoint
generation 1 without altering binary payloads. Terminal admission later moved to
24 / 2. View panes now use private protocol **25**, endpoint generation **2**,
and negotiated **shell.surface.v2**, with historical named v1 payloads preserved
as decode inputs and surface v1 retired from serving. The private bump covers
the appended surface-v2 bincode message, not session JSON itself. See the
[surface codec decision](../architecture/endpoint-surface-codec-v2.md).
