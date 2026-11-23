# Windows store, paths and process slice (2026-11-22)

Implementation: `src/store.rs`, `src/paths.rs`, `src/main.rs`, `src/tui/mod.rs`,
`tests/support/timing.rs`. This is protected-state/risky work; native runtime validation is outstanding.

## Replacement and durability

Unix keeps its existing temp-file sync, rename, directory-sync path. Windows uses an exclusive,
random tempfile in the destination directory, clears tempfile's `FILE_ATTRIBUTE_TEMPORARY` (as
its own `persist`/`keep` implementation does), syncs the file, then calls
`MoveFileExW(REPLACE_EXISTING | WRITE_THROUGH)`. There is no directory open/fsync on Windows,
including directory provisioning. The raw quarantine backup is staged separately, flushed, then
published through the same write-through move before the original can be replaced (round 2 correction).
A failed move returns
`AtomicWriteError { renamed: false }`; a successful move has no subsequent fallible barrier.
Existing callers, including `IdentityMap::save` in `src/provider.rs`, retain their adoption logic.
No retry/replay is added.

Microsoft documents WRITE_THROUGH as not returning until the file is moved on disk, and explicitly
guarantees flushing for a copy/delete move. We do not request COPY_ALLOWED; this is a same-directory
move. This reports the API result, not a tested power-loss guarantee or an independent guarantee
about newly-created ancestor directory entries on Windows.

- [MoveFileExW flags and return value](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw)
- tempfile 3.27.0 source: `src/file/imp/windows.rs`, `create_named`, `persist`, `keep`.

Windows-only store tests cover replacement while a default Rust reader is open (including a Unicode
filename and removal of the temporary attribute), and failure with a reader denying delete sharing
(original preserved, temporary file removed, subsequent replacement succeeds). The existing
failed-directory-destination cleanup test runs on both platforms. The injected directory-sync
failure test is Unix-only, because Windows has no such post-rename operation.

## Reader audit

All production destinations of `write_atomic`/the default `DurableWriter` were checked:

| File | Reader | Handle lifetime |
| --- | --- | --- |
| `workspace.json` | `Store::open`, `src/store.rs` | `BufReader<File>` consumed by `serde_json::from_reader`, dropped before returning the store |
| source workspace for quarantine | `backup_quarantine`, `src/store.rs` | temporary `File` dropped after `io::copy`, before publication |
| `resources.json` | `IdentityMap::open`, `src/provider.rs` | `std::fs::read`, closed before parsing/return |
| `runtime-tokens.json` | `Tokens::open`, `src/bridge_tokens.rs` | `std::fs::read`, closed before parsing/return |
| `operational/<domain>.json` | `Domain::load`, `src/operational.rs` | temporary `File::take` consumed by `read_to_end`, dropped at the end of that statement |

No Rust destination reader is retained across a replace; no fix is needed. In addition Rust's default
OpenOptions sharing includes READ, WRITE and DELETE, so an overlapping ordinary Rust read does not
block replacement: [Rust OpenOptionsExt::share_mode](https://doc.rust-lang.org/std/os/windows/fs/trait.OpenOptionsExt.html#tymethod.share_mode).

No bundled Python extension directly reads any of these authoritative state files. Python-related
cases for slice D: `extensions/logs.py` reads extension logs with a short `with open`; those logs are
not atomic-store destinations. `coordination_archive.py::verify` and archive inspections in
`coordination_engine.py` read independent archive files with bounded `with open` scopes; exports
are exclusive-create and never replace existing archives. `tier1.py` writes exclusive per-launch
files; its plugin read is a source file, not core state. The archive directory `os.open`/`fsync` is
not used on Windows: slice D refuses archive export before filesystem effects. This is not a replacement-reader issue.

## Lock lifetime

No lock change: `.workspace.lock` is opened read/write, then `File::try_lock` acquires an exclusive
nonblocking lock. Rust documents Windows `LockFileEx(EXCLUSIVE_LOCK | FAIL_IMMEDIATELY)` and release
when all duplicated/inherited handles close. Our Arc-held handle remains live through late writes;
no workspace-data handle is used as a lock.

- [Rust File::try_lock](https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock)

## Paths, process and timing

`state_dir` and `extensions_dir` now return `Result<PathBuf>` so Windows can report missing/empty
LOCALAPPDATA without panic or accidental current-directory storage. Explicit VQRO overrides remain
first; Windows ignores XDG/HOME defaults. Unix resolution is unchanged. Callers propagate the error.
Windows defaults are LOCALAPPDATA/vqro/state and LOCALAPPDATA/vqro/extensions.

Windows stop never invokes `kill`; failure to answer shutdown reports that only protocol shutdown
is supported. SIGTERM is Unix-only; Ctrl-C remains on both. The Windows TUI restores terminal state,
stops and joins its bounded-poll console event reader, then spawns the replacement executable with the
same arguments/environment and inherited standard I/O, waits, and exits with its status. A reader panic
refuses replacement rather than handing off without a confirmed join. Unix still uses exec and its
original event reader. Crossterm 0.29 `event::poll` guarantees that a following `read` will not block
when poll returns true, provided reads/polls are not mixed across threads; the input thread is the sole reader.

Round 2 powerless controls: `tui::console_reader::tests` uses a fake blocked reader to require completion
before the fake spawn, and rejects spawn on reader panic. `store::tests::quarantine_backup_requires_publication_barrier_before_original_replacement`
uses a failing fake publication call to require error propagation, original byte preservation and guard cleanup,
then a successful fake publication to check the backup survives original replacement. Negative controls
omitting stop/join fail both reader tests; omitting the publication callback fails the quarantine test.
The Windows production
callback is the same `move_file_write_through` used by ordinary store writes; no retry/replay is added.

Thread CPU timing uses GetThreadTimes on Windows (kernel + user, 100 ns units), not a wall-clock
fallback. Consumers: `tests/history_budget.rs`, `tests/identity_hygiene.rs`, `src/core.rs`'s inventory
work test, and the timing helper's three self-tests. Windows scheduler/accounting granularity must
be assessed natively before treating small CPU budgets as calibrated there.

- [GetThreadTimes units and return value](https://learn.microsoft.com/en-us/windows/win32/api/processthreadsapi/nf-processthreadsapi-getthreadtimes)

## Required native checks

Run Windows store tests (including the sharing violation and open-reader cases), quarantine recovery,
lock contention/release including late writer shutdown, missing LOCALAPPDATA/override startup,
protocol stop and Ctrl-C, TUI replacement with exit-code/standard-I/O preservation, and timing tests.
A Linux-hosted GNU-target check is compile evidence only, not these runtime checks.
