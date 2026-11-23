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
Zig's intermediate compiler cache is temporary, not retained in the shared installed entry. Missing, changed,
extra or corrupt artifacts and mismatched inputs cause a rebuild; they are never linked as a cache hit.
File symlinks must resolve within the entry; directory symlinks are refused. The GNU Windows archive alias
is created before publication, so consumers never modify a shared entry. Cargo watches the installed entry
as an input too, so a removed or modified artifact invalidates cached build-script output before a later link.

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

## Validation (2026-11-23)

- RED: the entry/manifest symlink regression failed against the initial verifier; eight other tests passed.
  Rejecting symlink entry and manifest types made it pass.
- A compatibility regression verifies the computed source digest equals SHA-256 of the checked-in vendor
  manifest. It caught the distinction between pathlib's component ordering and whole-path string ordering;
  matching pathlib fixed it. All eleven cache tests pass.
- A real native build took **50.898 s** (existing Zig packages/global cache were already present). Running the
  compiled build script against a second copied source checkout and separate `OUT_DIR` reused the same key
  in **1.227 s** (**1.258 s** including the FFI check/process). A recording Zig wrapper saw only `version`,
  `cc`, `version`: **zero `zig build` invocations** on reuse.
- A fake-Zig integration probe observed build → reuse → rebuild after changing SIMD, and two explicit
  `VQRO_LIBGHOSTTY_CACHE=off` audit runs both built into their own output directories.
- The quick gate passed all nine steps with zero skips, including zero-warning builds, all Rust/Python tests,
  and the Windows GNU cross-check. The Windows GNU alias was therefore exercised as well as Linux native
  linking. This is not a native Windows runtime or lock-concurrency test.
