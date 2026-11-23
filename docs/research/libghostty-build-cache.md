# libghostty-vt build cache

`build.rs` shares installed native artifacts across checkouts, Cargo targets and Rust profiles. The default
root is the user's cache directory under `vqro/libghostty-vt` (`XDG_CACHE_HOME`, Windows `LOCALAPPDATA`, or
`HOME/.cache`). `VQRO_LIBGHOSTTY_CACHE` overrides the root; `VQRO_LIBGHOSTTY_CACHE=off` retains the old
`OUT_DIR` build for audits. With no user cache/home available, sharing falls back to `OUT_DIR`.

Each key hashes a versioned JSON record containing the actual vendored source manifest (the same sorted
path/SHA-256 representation as `scripts/check_vendor.py`), Rust target triple, optimization mode, Zig version,
and complete Zig command arguments. Only the install prefix and temporary compiler-cache paths are replaced
by stable placeholders. Dirty source files change the key even before the vendor manifest is regenerated.
The layout schema must be bumped when installed layout or build semantics change.

A persistent `<key>.lock` serializes verification and building (`File::lock`: flock on Unix, LockFileEx on
Windows). Builders install into a unique temporary directory under the cache root. A manifest records all
installed artifacts and their SHA-256 hashes, and the complete directory is atomically renamed into place.
Zig's intermediate compiler cache is temporary, not retained in the shared installed entry. Under the key's
lock, the next access removes that key's abandoned `.<key>-*` stage directories, including on cache hits.
This recovers work directories left by abrupt exits that bypass temporary-directory destructors; it does not
stop any orphan Zig process, which may fail when its abandoned directory is removed. Missing, changed,
extra or corrupt artifacts and mismatched inputs cause a rebuild; they are never linked as a cache hit.
File symlinks must resolve within the entry; directory symlinks are refused. The GNU Windows archive alias
is created before publication, so consumers never modify a shared entry. While holding the key lock, each
consumer copies only its required link archive into `OUT_DIR`, checking the copy against the manifest before
publication. Cargo links and watches that private snapshot, never the evictable shared path. Unchanged copies
retain their modification time; a corrupt private snapshot triggers verification and replacement on the next
build. Shared eviction cannot remove an archive while a Cargo linker is still waiting to open it.
The manifest detects corruption and partial writes, not deliberate same-user modification: that user can change
both artifacts and their recorded hashes. This per-user cache is not an authenticity/security boundary.

The FFI header check is deliberately outside the cache and still runs on every build-script invocation.
`OUT_DIR/libghostty-cache.json` records the key, whether it was `built`, `reused`, or `disabled`, and elapsed
milliseconds (source hashing/verification/native build, excluding the FFI check). The key is also available
as the compile-time environment variable `VQRO_LIBGHOSTTY_CACHE_KEY`; no Cargo warnings are emitted.

The cache itself never fetches or downloads anything. Zig's existing `ZIG_LOCAL_PKG_DIR` behavior remains
unchanged: the required content-addressed packages must already be present for offline builds. This is not
a new offline/network sandbox for Zig; its pinned 0.17.1 `--system` failure is outside this change.

Regression tests: `tests/ghostty_cache.rs` includes the same small module used by `build.rs`, with tiny fake
artifacts and subprocess builders. It covers every key input, dirty-source identity, partial/corrupt entries,
required artifacts, failed builds, cross-checkout reuse, target mismatch, symlink rejection, and six competing
processes producing exactly one build.

## Disk usage and manual pruning

Successful builds and reuses keep the **six most recently used keys** by default. Set
`VQRO_LIBGHOSTTY_CACHE_KEEP` to a positive integer to change that limit. The manifest's modification time
records the latest successful use under the key lock. Each eviction candidate is locked nonblocking, and its
use time is checked again before deletion. Busy or newly used candidates are skipped, so the limit may be
temporarily exceeded until a later access. Lock files remain in place to preserve their inode identity.

Each shared entry was about 24 MB in the audit. Private link snapshots also occupy archive space per
`OUT_DIR`, but retain no Zig work cache. Interrupted-build stages are reclaimed on the next use of their key.

To prune safely, first wait for or stop **all Cargo/Zig builds using the cache**, including orphan Zig processes
left by interrupted builds. Then delete selected `<cache root>/<key>/` directories, or delete the entire cache
root to start empty. Resolve the root from `VQRO_LIBGHOSTTY_CACHE` or the defaults above; do not delete the
separate `zig-pkg` package cache or vendored sources. Leftover `<key>.lock` files are harmless and may remain.
Never remove/recreate lock files or the cache root while builds run: new lock inodes could admit concurrent
builders. The next build recreates removed entries.

## Validation (2026-11-23)

- RED: the entry/manifest symlink regression failed against the initial verifier; eight other tests passed.
  Rejecting symlink entry and manifest types made it pass.
- A compatibility regression verifies the computed source digest equals SHA-256 of the checked-in vendor
  manifest. It caught the distinction between pathlib's component ordering and whole-path string ordering;
  matching pathlib fixed it. Later audit regressions cover abandoned stages, LRU eviction and locked-key
  skipping, configurable retention, verified private snapshots surviving shared eviction, and copy corruption.
- Before the audit's eviction/private-snapshot follow-up, a real native build took **50.898 s** (existing
  Zig packages/global cache were already present). Running the
  compiled build script against a second copied source checkout and separate `OUT_DIR` reused the same key
  in **1.227 s** (**1.258 s** including the FFI check/process). A recording Zig wrapper saw only `version`,
  `cc`, `version`: **zero `zig build` invocations** on reuse.
- A fake-Zig integration probe observed build → reuse → rebuild after changing SIMD, and two explicit
  `VQRO_LIBGHOSTTY_CACHE=off` audit runs both built into their own output directories.
- Audit follow-up RED: both interrupted-stage cleanup regressions failed before the fix. With cleanup,
  bounded LRU and private snapshots, all nineteen cache tests pass; a copied link input remains intact after
  its shared entry is evicted, and copied bytes that disagree with the manifest are never published.
- The quick gate passed all nine steps with zero skips, including zero-warning builds, all Rust/Python tests,
  and the Windows GNU cross-check. The Windows GNU alias was therefore exercised as well as Linux native
  linking. This is not a native Windows runtime or lock-concurrency test.
