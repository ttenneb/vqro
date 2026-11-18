# JSON depth recovery and accept pressure (2026-11-18)

## Findings and smallest fixes

Baseline `a5ec030` accepts the reviewer's depth123 `doc/deep` write and overwrite. The history preimage
adds enough wrappers that the next `prim serve` exits with `recursion limit exceeded`. Byte budgets alone
do not protect decoders. Separately, real child-only `RLIMIT_NOFILE=64` pressure makes the old listener
report `Too many open files` and shut down the core, including an already healthy client.

`src/limits.rs::json_depth` walks iteratively with one iterator per ancestor (not per leaf);64-container
opaque values leave room for file/history/snapshot wrappers. Wire publication additionally fences complete
96-container frames. Admission covers entries/reverts, live views, command arguments, provider registration,
facts (native and wire), stream reasons/errors and toast styles. Notes are strings with the existing4096-byte
bound: brackets/escaped quotes inside strings do not become containers. An over-deep unsent call is rejected;
a reply that cannot be published is indeterminate, never a false promise that an effect did not happen.

`Store::open` deserializes each entry/preimage value through syntax-validated `RawValue`, whose JSON skipping
is iterative. It checks raw nesting before constructing `Value`; it does not disable serde's recursion limit.
Tests include10000 containers, not just the original near-boundary123. Healthy entries and their versions
are unchanged. Unsafe views/layouts become inert recovery placeholders, keeping healthy references valid;
other unsafe current entries are omitted. Unsafe preimages are removed behind explicit `undoable:false`
boundaries, preserving author/invocation attribution and refusing rather than skipping undo targets.

Loading never rewrites the source. Before replacement, standard exclusive0600 tempfile/file-sync/directory-sync
mechanisms preserve a byte-identical raw `workspace.quarantine-*.json`. Failure prevents the workspace writer
from running. Shared backup ownership survives failed/discarded writer snapshots, so retries do not repeatedly
copy the source. Persisted recovery metadata supplies the existing snapshot/TUI recovery notice after restart.
Archives remain for manual inspection; no raw value is automatically interpreted or replayed. A crash during
backup can leave an unreferenced archive, as with other durable-write temporary files; the original is still intact.

`server::serve` now treats EMFILE/ENFILE and other transient accept failures as resource pressure, logs at most
once per5 seconds and backs off100ms while serving existing clients. The serving JoinSet is capped at2048;
a refused connection gets a bounded first-request/id read and a structured overload response, then closes.
Idle clients are not expired and existing clients are not evicted. Startup best-effort raises the process's
soft nofile limit to its inherited hard limit, logs the result and never raises the hard limit. Native execution
ownership, caller effect deadlines and input/no-replay semantics are unchanged.

## Red/green evidence

Private binaries/receipts are under `~/.local/share/herdr/scratch/prim-final-store-limits/`:

- `deep-red-green.json` / `baseline-deep-brick.log` / `fixed-deep-brick.log`: exact reviewer
  `prim-final-rereview/run/deep_brick.py`; baseline accepts then cannot restart, fixed rejects then restarts.
  The fixed binary also opens the original bricked file, reports quarantine and preserves exact raw bytes
  before a successful unrelated write.
- `accept-red-all.log`: all three native cases fail against the frozen `a5ec030` binary: over-cap client is
  admitted, EMFILE exits the core, and soft64/hard128 remains64/128 rather than128/128.
- `tests/accept_limits.rs`: genuine private-child EMFILE with an existing hello client, subsequent admission,
  cap refusal/EOF slot release, and `/proc/<pid>/limits` confirmation of soft-only raising. No production
  fault environment or real machine-wide file-table exhaustion is used.
- `tests/store_depth.rs`: boundary64 apply/overwrite/revert/restart, rejected65 with unchanged file, arbitrary
  raw legacy depth, exact private backup/restart, disabled unsafe undo, healthy layout references, native fact
  fields, RPC publication admission, and escaped bracket strings.
- `store::tests::recovery_backup_precedes_write_and_survives_a_discarded_writer_snapshot`: failed backup never
  reaches the writer; pre-rename failure retains source; the next snapshot reuses its completed backup.
- `rpc::tests::depth_fences_reject_unsent_calls_and_mark_unpublishable_replies_indeterminate`: pending cleanup,
  honest reply uncertainty (ordinary and completion-barrier replies) and a still-usable boundary-depth connection.
- `server::accept_tests::descriptor_and_transient_accept_failures_are_not_fatal_listener_errors`: ENFILE and
  related error classification (not a claim of genuine host-wide ENFILE induction).

All native children use empty private extension directories, private HOME/state/socket/runtime/HC paths and
`PRIM_HERDR=["/bin/false"]`; no live server, model, terminal or session is involved. Linux native pressure tests
are included by `scripts/check.sh full`; actual macOS/Windows behavior is not claimed by Linux evidence.
