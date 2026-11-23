# libghostty-vt build cache

`build.rs` shares installed native artifacts across checkouts, Cargo targets and Rust profiles. The default
root is the user's cache directory under `vqro/libghostty-vt` (`XDG_CACHE_HOME`, Windows `LOCALAPPDATA`, or
`HOME/.cache`). `VQRO_LIBGHOSTTY_CACHE` overrides the root; `VQRO_LIBGHOSTTY_CACHE=off` retains the old
`OUT_DIR` build for audits. With no user cache/home available, sharing falls back to `OUT_DIR`.

Each key hashes a versioned JSON record containing the actual vendored source manifest (the same sorted
path/SHA-256 representation as `scripts/check_vendor.py`), Rust target triple, optimization mode, Zig version,
complete Zig command arguments, and native configuration: approved reuse boundary, Cargo host triple,
resolved Zig executable/library directories and host target (`zig env`), and consumed compiler/SDK caller settings. Only the install prefix and temporary compiler-cache paths are replaced
by stable placeholders. Dirty source files change the key even before the vendor manifest is regenerated.
The layout schema must be bumped when installed layout or build semantics change.

A persistent `<key>.lock` serializes verification and building (`File::lock`: flock on Unix, LockFileEx on
Windows). Builders install into a unique temporary directory under the cache root. A manifest records all
installed artifacts and their SHA-256 hashes, and the complete directory is atomically renamed into place.
Zig's intermediate compiler cache is temporary, not retained in the shared installed entry. Under the key's
lock, an access with GC explicitly enabled considers that key's abandoned `.<key>-*` stage directories,
including on cache hits. Each stage records its owner PID and observed process start identity. The cleanup
**heuristic** requires **both** a positive owner-identity-gone observation **and** no modification anywhere
in the stage tree for at least one hour. Unknown
owner/start records, failed process queries, unreadable or future timestamps, live owners, and recent writes
are retained. Start identity is observed from Linux boot ID/start ticks, Unix `ps` start time, or Windows
process creation time; unavailable probes record unknown and disable removal. Windows recognizes only the
specific `NoProcessFoundForGivenId`/`ObjectNotFound` error as absence; access/query/start-time failures stay
unknown. Unix `ps` failure or empty output is also unknown; Linux requires positive directory absence in a
working proc view. No process is killed. Owner gone plus a quiet hour is not proof that an orphan exited:
a paused/idle orphan or activity after the scan can still lose staging. Automatic maintenance assumes a
trusted host-local cache, process view and clock; it is not a no-active-writer certificate. Missing, changed,
extra or corrupt artifacts and mismatched inputs cause a rebuild; they are never linked as a cache hit.
File symlinks must be relative, never lexically ascend above the entry root, and resolve within the entry.
Rooted/absolute links, escape-and-return links such as `../../install/lib/native.a`, and directory symlinks
are refused before publication. This confines the supported relative-alias subset; pre-rename canonical
containment alone is not a relocation guarantee. The GNU Windows archive alias
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

## Source observations and their limit

The initial source digest only routes the candidate key lock. The source is hashed again under that lock,
including after waiting for another builder, and immediately before a verified reuse is accepted. Builds
rehash after output verification and before publication. If any observed digest changes, the artifact is
copied/verified only into this build's `OUT_DIR`, never published into the shared cache, with the diagnostic
`vendored source changed during the build; not cached` in stderr and the JSON note.

These are before/after checks, **not proof of an immutable source or of the exact executed revision**.
Sources and the Zig installation/packages must remain stable while building. Changes restored between
observations (ABA) or concurrent reads can escape observation. The separate FFI header check does not
authenticate artifact provenance. This preserves local build behavior without knowingly sharing an observed
source/key mismatch.

## Approved platform reuse boundary

Approved by vqro-lead (Main), 2026-11-23: shared reuse is limited to explicit Linux GNU/musl and Windows GNU
targets using Zig's bundled libc, archive tools and target linker. Every other supported target still builds;
only shared caching is disabled with a reason recorded in stderr and `OUT_DIR/libghostty-cache.json`.

| Target/configuration | Behavior |
| --- | --- |
| x86_64/aarch64 Linux GNU or musl, explicit target, bundled toolchain | Shared cache |
| x86_64/aarch64 Windows GNU (including Rust `gnullvm`), explicit Zig GNU target | Shared cache |
| Darwin, native or cross | Local uncached build; Darwin caching deferred until SDK/toolchain keying is proven |
| Windows MSVC | Local uncached build; Windows SDK/Visual Studio registry discovery is not claimed keyed |
| Any external CC/CXX, sysroot, libc, SDK, linker/header/library or packaging override | Local uncached build naming the setting |
| Unrecognized/unresolvable `zig env` executable/library paths | Local uncached build; never guess configuration |

The exact environment allow/opt-out policy is `build/native_config.rs::ENV/reason`, with matching Cargo
`rerun-if-env-changed` declarations. It includes `SDKROOT`, `DEVELOPER_DIR`, deployment targets, Windows SDK,
VC/VS, `CC`-related variables, sysroot/libc settings, Nix/include/library flags, pkg-config and install overrides.
`PROGRAMDATA` alone is an ordinary Windows host setting: it is keyed but does not select an external SDK.
Native SDK discovery (`xcrun` SDK path/version, Apple archive postprocessing, or Windows registry/VS discovery)
is deliberately not approximated: those builds are uncached. The SDK is still chosen by the original producer.

The cached configuration's explicit OS/ABI prevents native-libc discovery (`std.Target.Query.isNativeAbi` /
`canDetectLibC` and `std.zig.LibCDirs.detect` in Zig 0.17.1). `Config.zig::genericMacOSTarget`,
`GhosttyLibVt.zig::initLib`, and `pkg/apple-sdk` have additional host behavior, hence the Darwin opt-out.
The static producer's optimize/SIMD/features/target arguments remain in the single keyed/executed argv.
Cargo's Rust linker flags and target features are not forwarded to Zig; they govern the caller's separate
Rust/link step, not the native archive. Cargo injects a worktree-specific `LD_LIBRARY_PATH` to run build
scripts; it is not keyed or treated as an external compiler setting. Ghostty reads it only in its native-CPU
Nix branch, which explicit CPU targets cannot enter; `IN_NIX_SHELL` independently opts out of shared reuse.
An integration probe caught the overly broad runtime-path opt-out before release.
Toolchain installations/packages are assumed immutable between
ordinary version/path changes; this is not compiler authentication or a host-SDK fingerprinting framework.

## Disk usage and manual pruning

**GC is off by default during vqro-lead's machine-wide pruning pause (2026-11-23).** The
`VQRO_LIBGHOSTTY_CACHE_GC` switch uses the existing `env_bool` grammar:
**grammar accepted by vqro-lead (Main)**. `1`, `true`, `yes`, or `on` (case-insensitive) enables stale-stage reclamation and LRU;
`0`, `false`, `no`, or `off` (case-insensitive) disables it; an absent variable means off. Any other value,
including an empty string, stops the build with a clear message. Do not enable GC against
production until vqro-lead lifts the pause. Tests opt in only on disposable roots. With GC off, accesses do
not reclaim abandoned stages or perform LRU eviction, so disk usage is not bounded by the retention setting.
Ordinary per-build temporary cleanup and invalid-entry replacement remain build behavior.

When GC is enabled, successful builds and reuses keep the **six most recently used keys** by default. Set
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
- Source-hold follow-up: twenty-five cache tests cover source mutation inside the build seam (local result,
  no publication and no later stale hit), source changes before reuse/after a lock wait, explicit native reuse
  boundaries and key inputs, absolute-link rejection/relative-link relocation, and dead-owner plus one-hour
  recursive quiescence (including PID reuse, unknown identity and fresh descendant activity).
- Renewed owner-query tests exercise the real generated PowerShell command and output/exit classifier
  through subprocess transport controls, including the original suppressed-error-to-absence RED. Additional
  native Windows tests inject cmdlet failures inside the real interpreter when run on Windows; they were
  **not run natively on this Linux host**. Unix command/filesystem controls cover ambiguous `ps` output,
  broken proc queries and positive absence. The error-ID/category selection is checked against PowerShell's
  [`RetrieveMatchingProcessesById` source](https://github.com/PowerShell/PowerShell/blob/master/src/Microsoft.PowerShell.Commands.Management/commands/management/Process.cs):
  `ProcessCommandException` alone also represents other errors and is not sufficient absence evidence.
- Linux descriptor controls inspect the actual live production key-lock descriptors for `FD_CLOEXEC`
  and confirm none survive a real exec child. Process-spawn and descriptor-owning fixtures are mutually
  exclusive: CLOEXEC closes at exec, not at fork, so unrelated pre-exec copies must not interfere with
  immediate private-fixture eviction assertions. No timer, polling retry, or production fd workaround is used.
- The exact escape-return relative-link fixture is rejected before publication, while a confined parent
  alias remains valid after rename. GC-off controls retain both old keys and otherwise-reclaimable stages;
  the corresponding GC-on controls explicitly enable maintenance only in disposable roots.
- The quick gate passed all nine steps with zero skips, including zero-warning builds, all Rust/Python tests,
  and the Windows GNU cross-check. The Windows GNU alias was therefore exercised as well as Linux native
  linking. This is not a native Windows runtime or lock-concurrency test.
