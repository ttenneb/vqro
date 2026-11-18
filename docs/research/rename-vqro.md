# Project rename: prim -> vqro (2026-11-18)

Human decision: the final product name is **vqro**, lowercase (D27). Nothing is published, and this is the only
user's one-time cutover, not a backwards-compatibility or multi-install support project. Main baseline: `0f718a6`.

## Current public surfaces

- Cargo crate/package `vqro`; binaries `vqro`, `vqro-herdr`, `vqro-tmux`.
- Every reserved `PRIM_*` name becomes `VQRO_*`; the runtime reads no old environment aliases. Default paths are
  `~/.local/state/vqro`, `~/.local/share/vqro/extensions` and the state directory's `vqro.sock`.
- Built-in `prim.*` command names become `vqro.*`; owned `doc/prim/*` keys become `doc/vqro/*`. Hello/status client
  names and the recovery badge/menu use the new name.
- SDK modules `vqro_ext`, `vqro_provider`, `vqro_provider_stream`, `vqro_display`, `vqro_views`; `Vqro`, `VqroError`
  and `Extension.vqro`. Current tests, manifests, installers, gates and docs use these names.
- Earlier decision entries and existing research records are byte-identical. Decisions gained only the approved
  top note and D27. The words primitive(s), primary and their capitalized forms are not renamed.

## One-time human-state copy

`scripts/migrate-from-prim.py` is stdlib-only and offline. Stop the old core first. It exclusively creates a private
new directory, translates known workspace keys, command references, installed paths, environment/module names and
the default keymap profile, including undo before values. Transaction IDs/revisions/authors/notes and arbitrary
user text/project directories stay intact. `resources.json` and `runtime-tokens.json` are copied byte-for-byte.
The old directory is retained, never rewritten; no symlinks, native commands, process signals or replay occur.

Active state locks, existing targets, key collisions and source symlinks reject. Transient socket/PID/lock files
are not copied. A copy IO failure reports an incomplete target for inspection rather than retrying or overwriting.
Installation and the human's one-time endpoint cutover belong to the parent, not the utility or this work stream.

**Old tmux markers are not carried over.** The human's tmux extension is disabled and there is no old owned tmux
server. Per the human's explicit scope decision, no marker-transfer tooling, other-install instructions or runtime
compatibility reads are shipped. New native markers use the new name only.

## Focused evidence

- Twelve migration regressions cover references/history, literal argv, SDK paths/modules, environment/profile
  values, collisions, target/source guards, active locks, transient files, symlinks, privacy modes and report secrecy.
  A copy-only translator mutant fails the namespace/history test; the real translator passes.
- A private actual old `0f718a6` core generated three fake host terminals and two fake agents, settings, layouts,
  bindings/menu entries and undo history. The new core resumed all five IDs and the old runtime credential after
  migration. Explicit undo restored the translated prior command. The old live core's Rust state lock excluded
  the migrator; the stopped source stayed unchanged. No model calls or real backend sessions were used.
- The authorized read-only real-state copy contained 20 regular files (176450 bytes), 82 entries and 253 history
  records. It migrated successfully with identity/credential files byte-identical and transaction metadata intact.
  Both private copies were deleted after owner/no-active-user checks; no copied declaration was executed.
- The ordinary post-rename Rust suite and 215 pre-migration Python cases passed before adding the migration tests.
  Final full-gate receipt and logs are kept separately under `~/.local/share/herdr/scratch/rename-vqro/`.

No install, live mutation, push or merge was performed. Linux/Python 3.14 is verified; native Windows transport and
Windows/macOS runtime verification remain the project's pre-existing limitations, not new portability claims.
