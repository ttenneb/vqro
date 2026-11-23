# Native Windows: friction, issues, and bugs found

Date: 2026-11-23  
Base checkout: `9ddd1e2`; feature branch: `fix/windows-background-console-windows`  
Platform: native Windows 10.0.26200 x86-64; no WSL

## Executive summary

Native debug/release builds, named-pipe communication, workspace persistence, a real herdr-core
provider, and PowerShell input/output inside the TUI worked in limited smoke tests. **This does not
establish that the Windows installation is ready for daily use.**

The most serious finding came from the user after the smoke tests: **more than two dozen separate
console windows opened, and closing them caused them to reopen.** Background extensions were being
launched without Windows console suppression; the supervisor's restart behavior compounded this.
The earlier success assessment missed a desktop-visible regression and was too strong.

**Current safety state:**

- The private demo core and backend were stopped through their shutdown commands.
- Subsequent process inspection found **zero remaining vqro/demo processes**.
- The corrected release binaries and Python background helpers are installed. The earlier binaries
  were backed up locally before replacement; installed binary hashes were verified.
- The scratch launcher is re-enabled with an experimental-build warning. It does not auto-launch
  services until the user runs it. No test service was left running.
- Detached-core regression tests and a bounded real-backend desktop/TUI check passed, including a
  repeat against the installed files. The desktop monitor observed **zero new visible windows**.
- The full release gate has never passed on this machine. Remaining failures are not waived.
- Work is isolated on the named feature branch; it is not merged to `main`.

## Environment and installation footprint

| Component | Observed configuration |
| --- | --- |
| Filesystems | NTFS on C: and E: |
| Rust / Cargo | 1.94.0, native MSVC target |
| Python | CPython 3.14.2 |
| Shells | Windows PowerShell and Git Bash |
| Zig | Required 0.17.1; initially missing, subsequently installed |
| Backend | Separate herdr-core 0.1.22 Windows distribution |
| TUI harness | ConPTY through pywinpty 3.1.2, installed only into scratch dependencies |

Installed files remain at:

- Release binaries: `C:\Users\bengar\bin\vqro*.exe` — seven executables.
- Extensions/manifests/test suites/text client/tier-1 plugin: `%LOCALAPPDATA%\vqro\extensions`.
- Separate backend, including ConPTY runtime and notices: `%LOCALAPPDATA%\Programs\herdr-core-vqro`.
- Zig: `%USERPROFILE%\.local\zig-0.17.1`.

Existing `vqro.tasks` data was preserved. The existing Herdr application was not replaced. No global
PATH edit, administrator installation, or service registration was performed. Test terminals were
killed and forgotten. Testing used private vqro and backend endpoints/state, not the daily workspace.

Local logs and disposable harnesses are under `E:\vqro\.run\windows\`. These are ignored local
artifacts, not committed CI evidence. Results are recorded below so the report survives their removal.

### Trying the corrected local installation

No services are left running. On this machine only, the existing scratch launcher now uses the
corrected installation and emits an experimental-build warning:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File E:\vqro\.run\windows\Start-Vqro.ps1
# Ctrl-g n: new pane/tab; Ctrl-g q: quit only the UI.
powershell -NoProfile -ExecutionPolicy Bypass -File E:\vqro\.run\windows\Start-Vqro.ps1 -Action status
# Close any demo panes first; explicitly stop both private services:
powershell -NoProfile -ExecutionPolicy Bypass -File E:\vqro\.run\windows\Start-Vqro.ps1 -Action stop
```

This launcher is an ignored local artifact, not a maintained installer. It selects private vqro
endpoint `\\.\pipe\vqro-native-demo` and state `.run/windows/demo-state`, plus separate herdr endpoint
`\\.\pipe\herdr-vqro-native-demo`, session `vqro-native-demo`, and private XDG runtime/state/config
paths. It explicitly supplies the standalone backend executable and installed extensions path.
A bare `vqro status` without these settings does not report this demo's status. Do not connect a
scratch test to the existing Herdr application or assume `--fake` isolates the default provider.

## Issue inventory

| ID | Finding | Classification | Current status |
| --- | --- | --- | --- |
| W01 | Separate extension console windows; reopening on close | High-impact product bug | Fix installed; bounded detached-core and desktop/TUI checks passed; services stopped |
| W02 | Quarantine backup publication fails with Access denied | Persistence/recovery bug | Source fixed; existing native regression tests pass |
| W03 | Replacement fails while an ordinary reader remains open | Persistence bug / unsupported assumption | Unresolved |
| W04 | Concurrent extension logging fails with Access denied | Logging defect candidate | Reproduced in test; not diagnosed |
| W05 | No native Windows installation/upgrade path | Packaging gap | One-machine copy workaround only |
| W06 | Release gate relies on Linux and Unix tool layout | Validation infrastructure gap | Unresolved; no passing full receipt |
| W07 | Git autocrlf breaks vendor byte hashes | Checkout/reproducibility bug | Attribute fix; vendor validation passes |
| W08 | Required Zig version missing | Setup friction | Installed and checksum-verified |
| W09 | `python3` resolves to Store alias | Tool discovery friction | Application handles it; gate does not |
| W10 | Rust tests assume Unix endpoints and temp roots | Test portability defects | Unresolved |
| W11 | Many Python tests assume Unix APIs | Test portability and compatibility failures | Unresolved; not all individually classified |
| W12 | Licensing check reports stale generated notices | Reproducibility/validation issue | Unresolved |
| W13 | Forward-slash cmd.exe path exits unexpectedly | Backend argv/path issue | Workaround observed; root cause unknown |
| W14 | UTF-8 diagnostics decoded using cp1252 | Harness encoding defect | Local harness corrected |
| W15 | SDK disconnect exits controller before cleanup | Harness/API-lifecycle mismatch | Independent CLI cleanup used |
| W16 | Headless smoke missed desktop behavior | Verification/process failure | Report corrected; desktop acceptance still needed |

## Confirmed bugs and significant failures

### W01 — Console-window storm and automatic reopening

**User impact:** more than two dozen separate windows appeared. Closing one caused it to reopen.

**Cause supported by source:** `src/ext.rs` starts the supervised extensions as console-subsystem
programs with redirected standard streams, but without a Windows no-console creation flag. Redirection
alone does not stop Windows allocating a console, particularly when the parent core is detached.
The demo had 24 extension declarations. Closing an extension console ends that process; the existing
crash-restart policy then starts it again. The stdio backend connector in `src/adapter/herdr.rs` had
the same missing suppression.

These windows were not evidence of two dozen backend workspace terminals: the demo snapshot had
**zero resources** before the user report.

**Containment:** stopped the private core and backend; verified zero remaining vqro/demo processes;
temporarily disabled the local launcher's TUI action until the corrected installation was verified.
Unrelated user processes were not terminated.

**Source fix:** added `program::background_command`, using `CREATE_NO_WINDOW` on Windows, and used
it for supervised extensions and the stdio herdr connector. Added stdlib-only
`vqro_process.background_kwargs()` for Python extension helpers: workspace Git probes, tier-1
metadata probes, and diagnostics subprocesses. Windows does not inherit this flag into grandchildren,
so each background boundary needs it. Unix options are unchanged. Interactive UI, link openers and
backend-owned terminal launch paths are unchanged. This is not a change to restart policy or a new
execution host.

**Validation:**

- Rust native test `program::tests::background_child_has_no_console_and_retains_stdio` passed:
  PowerShell reported `GetConsoleWindow() == 0` and returned stdout normally.
- New `tests/test_vqro_process.py`: four tests passed, covering Unix/Windows options, native console
  handle and stdin/stdout/stderr, and the actual background Python launch sites.
- New `tests/windows_background_smoke.py` passed against debug and release builds: a detached private
  core starts a probe extension, its Python helper and a native-connector fixture through `vqro-herdr`.
  Each reports zero console handles; both service declarations are restarted; replacement children
  also report zero. Supervisor stdout/stderr draining and process exit on shutdown are checked.
- A bounded full-bundle desktop check (local harness) ran 24 declarations, real private herdr-core,
  workspace Git scanning and a workspaces-extension restart. ConPTY TUI PowerShell echo, recovery
  menu and quit passed. Repeated with the installed executables and installed extensions.
  `EnumWindows`/`IsWindowVisible` monitoring saw **zero new visible desktop windows** during each
  completed run. Test panes were killed/forgotten; backend inventory was empty and shutdown succeeded.
- Final process inspection: zero vqro/private-backend processes. No unattended demo was restarted.

This is a bounded observed pass, not certification of every subprocess launched by every extension
or agent runtime. Optional diagnostic suites still have Windows portability failures (below).

Evidence: `src/program.rs`, `src/ext.rs`, `src/adapter/herdr.rs`, `extensions/vqro_process.py`;
`background-smoke-{debug,release}.log`, `background-unit-rust.log`, `console-python-tests.log`,
`real-console-fix-summary.json`, `installed-real-console-fix-summary.json`,
`console-install-receipt.json`, `emergency-stop.log`, `processes-after-stop.json`.
Microsoft mechanism: [process creation flags](https://learn.microsoft.com/en-us/windows/win32/procthread/process-creation-flags)
and [Python CREATE_NO_WINDOW](https://docs.python.org/3/library/subprocess.html#subprocess.CREATE_NO_WINDOW).

### W02 — Quarantine backup publication failed on Windows

The recovery test
`store::tests::recovery_backup_precedes_write_and_survives_a_discarded_writer_snapshot`
failed with `ERROR_ACCESS_DENIED`.

The reserved backup destination remained an open `NamedTempFile` while `MoveFileExW` tried to replace
it. The fix converts that reservation into a `TempPath`: close the file handle while retaining the
reserved pathname and cleanup guard. The separately staged bytes are still synced and published with
the existing write-through barrier.

The recovery regression and publication-barrier tests now pass. No retry, replacement API fallback,
or weakened durability barrier was introduced. This fix was included in the earlier installed build.

Evidence: `src/store.rs`; `backup-fixed.log`, `store-tests-after.log`.

### W03 — Replacement over an open reader still fails

`store::tests::windows_replace_with_open_delete_shared_reader` fails on the second write with
Access denied, despite the normal Rust reader sharing delete access.

The separate sharing-violation test passes and preserves the original file. Ordinary workspace writes
and restart worked during the smoke, but that does not establish safe replacement while an external
reader holds the file open.

Earlier static research in `windows-store-process.md` asserted that ordinary overlapping readers were
harmless. This native result contradicts that assumption. The exact replacement semantics remain to
be investigated; removing `WRITE_THROUGH` or dropping the reader just to pass the test is not a fix.

Evidence: `store-isolated.log`, `store-tests-after.log`, `cargo-lib-final.log`.

### W04 — Concurrent extension logging fails

`ext_log::tests::one_generation_and_overlapping_writers_stay_within_two_fixed_files` fails with
Access denied in its writer threads.

This is a potential runtime logging/rotation defect, not merely an unsupported Unix fixture. It has
not been diagnosed. An open-handle/replacement interaction is a hypothesis, not an established cause.

Evidence: `cargo-lib-final.log` and `src/ext_log.rs` test failure.

## Build, install, and validation friction

### W05 — Unix-only installer and no Windows upgrade contract

`scripts/dev-install.sh` targets `~/.local/bin` and `~/.local/share`, while the native Windows extension
default is under `%LOCALAPPDATA%`. It also assumes Unix-style replacement of running executable inodes.
That is not a verified Windows executable-upgrade strategy.

A disposable one-machine Python script copied the seven stopped release executables, bundled
extensions, mapped test suites, client/plugin files, and separate backend runtime. Binary hashes
were compared with build artifacts. This is **not** a maintained, atomic Windows installer/upgrader.

### W06 — The full release gate cannot currently qualify Windows

`bash scripts/check.sh full` was attempted before installation and failed. Problems include:

- `env -i` removes Windows environment needed by native tools.
- `python3` is unavailable or resolves to the Microsoft Store alias.
- `/proc/net/unix`, Linux process inspection, and systemd/cgroup assumptions do not apply.
- `/usr/bin`-only PATH excludes native tools.
- The build step fails under the sanitized environment; dependent steps are recorded as failed/not run.
- Final receipt generation also cannot locate Python.

There is **no successful full-gate receipt**. The experimental copy was installed despite failed
qualification; it must not be described as a release-gate-approved installation.

### W07 — Git CRLF conversion broke exact vendor hashes

The checkout had `core.autocrlf=true`. Vendor files showed `i/lf w/crlf`, so byte-hash validation
reported changes despite the initially clean Git tree.

Added `vendor/** -text` to `.gitattributes` to preserve exact indexed bytes, including intentional
upstream line endings. Restored the initially clean vendor tree without automatic conversion. The
vendor check now passes: **1400 files, zero patches**. No vendor content changes or staged changes remain.

### W08 — Missing pinned Zig

The initial build failed because libghostty-vt requires **Zig 0.17.1**, which was not installed.
Downloaded the Windows x86-64 archive from the [official index](https://ziglang.org/download/index.json),
verified SHA-256 `b03f477a1943673e02aca0f570e8030dffd536ce1f45cbc4d3326ec31159562e`, and placed it in
the user-local directory already supported by `build.rs`. Native builds then succeeded.

The pinned dependency should be prominent in Windows setup instructions. No MSVC/SDK repair was needed.

### W09 — Python executable discovery differs between application and tooling

A real interpreter was available as `python`, but `python3` resolved to the Windows Store alias.
The application's existing resolver in `src/program.rs` correctly skips WindowsApps stubs. The shell
validation tooling does not use that mechanism and failed to find the real interpreter.

### W12 — Licensing reproducibility is unresolved

The offline licensing check initially failed because cross-platform Cargo metadata dependencies,
including `bumpalo`, had not been downloaded. `cargo fetch` resolved that prerequisite.

After fixing vendor line-ending corruption, licensing still reported `THIRD_PARTY_LICENSES.md`
stale. The generated inventory was not rewritten without investigating the discrepancy. This is a
failed check, not evidence that a particular dependency is unlicensed.

## Test results and portability defects

### W10 — Rust tests

Initial `cargo test`: **358 passed, 10 failed, 1 ignored** in the library suite.

After the quarantine fix, serial library run: **359 passed, 9 failed, 1 ignored**. After the console
fix, another serial library run completed: **360 passed, 9 failed, 1 ignored**, with the same nine
failures below. `cargo build --all-targets` and `cargo build --release` passed with no compiler warnings.
The full shell gate was attempted again before replacing the experimental installation and still
failed on the previously recorded infrastructure assumptions.

| Failing test | Observed failure |
| --- | --- |
| `commit::tests::operational_owner_retires_at_the_existing_writer_dispatch_linearization` | Timeout assertion; not diagnosed |
| `commit::tests::shutdown_deadline_rejects_queued_work_but_never_claims_active_write_failed` | Hard-coded/absent `E:/tmp` root |
| `ext::qa2_tests::cooperative_provider_gets_time_for_its_one_second_cleanup` | Hard-coded/absent `E:/tmp` root |
| `ext_log::tests::one_generation_and_overlapping_writers_stay_within_two_fixed_files` | Access denied in concurrent logging |
| `server::actual_system_authors_are_never_default_undo_targets` | Filesystem socket path instead of a named-pipe fixture |
| `store::tests::windows_replace_with_open_delete_shared_reader` | Access denied replacing an open destination |
| `tui::tests::owner_unset_hello_reaches_the_real_snapshot_and_renderer` | Unix-style endpoint fixture |
| `tui::tests::reconnect_draw_reattaches_terminal_and_repaints_backend_output` | Hard-coded/absent `E:/tmp` root |
| `ui_launch::tests::manifests_are_checked_listed_and_resolved_against_the_clients_directory` | Expected path mixes native separators with `clients/text.py` |

Several failures are fixture defects, but the timeout, logging, and replacement failures require
investigation. Full `cargo test` stops at the library failures, so it does not establish integration
suite success on Windows. Store-only tests after the fix: **13 passed, 1 failed**.

### W11 — Python tests

Discovery ran **1604 tests: 44 failures, 130 errors, 8 skips**. Recurring causes include:

- `asyncio.start_unix_server` and other Unix transport fixtures.
- Unix signals such as `SIGSTOP`.
- Shell/lifetime helper assumptions.
- Native buffering/timing differences; one provider cancellation test expected a nonempty write buffer
  but observed zero bytes.

These have not all been individually triaged; it would be incorrect to dismiss every failure as
fixture-only. Focused `test_extension_windows.py`: **6 tests passed**.

Follow-up focused runs: workspace/tab tests **10 passed**; Windows extension tests **6 passed**;
new background-helper tests **4 passed**. Tier-1 tests ran 12 with one error because the fixture
creates a Unix hook script that Windows cannot execute (`WinError 193`). Diagnostics suites remain
non-green: unit 41 tests (9 failures/3 errors), integration 31 tests (4 errors), performance 23 tests
(1 failure). Observed causes include Unix sockets/signals/process-group APIs, Windows symlink
privileges, executable-bit and 0600 permission assertions, and `python3` lookup; adding no-console
flags does not make these suites Windows-portable.

Other checks: manifest dependency validation passed for 24 manifests; vendor validation passed after
the attribute fix; `git diff --check` passed. Licensing and the full release gate remain failed.

## Backend and harness issues

### W13 — Backend discovery and argv/path behavior

No `hc`, standalone `herdr-core`, tmux, or Rex executable was available on PATH. The existing Herdr
application is not the separate core CLI and was left alone. The GitHub latest-release endpoint
returned 404; the documented [release service](https://releases.herdr.dev/core/latest.json) worked.

Downloaded the Windows core 0.1.22 distribution and verified SHA-256
`37cae9d6c522e205efcdb55a42d354285f41b8cbb4c6e60fb505b73b151966c9`.
The distribution's ConPTY runtime and notices must remain with it; copying only the executable is
not the tested deployment.

A terminal launched with `C:/Windows/System32/cmd.exe` briefly appeared live then exited with code 1.
Using `C:\Windows\System32\cmd.exe` kept it alive, and the backend's default PowerShell worked.
This is an observed distinction, not a diagnosed vqro argument-parsing bug. Native tmux is explicitly
unsupported; Rex was not installed/tested.

### W14 — Encoding failures in diagnostic harnesses

Python's captured-text default was cp1252, but CLI snapshots contain UTF-8 and Unicode UI content.
Decoding/printing failed with `UnicodeDecodeError` / `UnicodeEncodeError`. Explicit subprocess
`encoding='utf-8'` and `PYTHONIOENCODING=utf-8` fixed local diagnostics. These failures did not prove
that JSON-RPC transport itself was corrupt.

### W15 — Harness cleanup and SDK lifecycle mismatch

The extension SDK calls `os._exit(0)` when its transport fails/closes. A controller using that SDK can
exit before its outer cleanup runs. Initial scratch harnesses also mistakenly used nonexistent
`resource.remove` and `Vqro.close` APIs. The correct backend cleanup operation is `resource.forget`
after kill, and an independent CLI controller was used for final cleanup.

These were harness mistakes, not failures of the supported resource API. Cleanup was subsequently
verified: private backend `ls --all` returned an empty terminal list and shutdown succeeded.

### W16 — Verification failure: smoke success was overclaimed

The ConPTY smoke established shell input/output, a rendered recovery menu, and clean UI exit. It did
**not** inspect all desktop windows generated by the core and its children. Zero workspace resources
also does not mean zero separately allocated Windows consoles.

The earlier report and handoff called the system working too broadly and left the demo running before
this desktop acceptance check. The user's report exposed the gap. The assessment has been corrected. The subsequent bounded
fix verification explicitly monitored visible desktop windows, exercised background-child restart,
and checked cleanup. Continue including those observations in Windows acceptance, not just TUI
output and workspace resource counts.

## What was genuinely verified

These successes remain useful, within their limited scope:

- Native MSVC debug/release builds completed without compiler warnings in successful build logs.
- Rust core/CLI named-pipe communication, status, snapshots, workspace apply/get, restart persistence,
  and protocol shutdown worked.
- Python synchronous overlapped and asyncio proactor pipe smokes passed hello/snapshot/close.
- Real herdr provider registration/inventory and terminal spawn/kill/forget worked.
- Bundled extensions connected; `tabs.open` created the backend's default PowerShell shell.
- Installed binaries/extensions repeated the TUI shell echo, Ctrl-g recovery menu, and Ctrl-g q exit.
- Quarantine recovery and publication-barrier tests passed after the fix.
- Console suppression was installed and passed bounded installed desktop/TUI verification; all
  services were stopped afterward.

Evidence includes `build-zig.log`, `build-release-final.log`, `scratch-results.log`,
`installed-integration-results.log`, `tui-output.ansi`, `installed-cleanup-results.log`,
`install-receipt.json`, `cargo-lib-final.log`, and `python-tests.log` in the scratch evidence directory.

## Reproduce the targeted no-console regression

From native Windows with Rust/MSVC, pinned Zig and CPython 3.11+ available:

```powershell
cargo build --bins
cargo test --lib program::tests
python -m unittest discover -s tests -p test_vqro_process.py
python tests/windows_background_smoke.py --vqro target/debug/vqro.exe
```

The native smoke declares only two private fixture services and does not start a real backend,
agent, or workspace terminal. It tests a detached core, stdio, nested helper, provider connector,
restart and shutdown. Do not substitute the daily core endpoint. This targeted pass does not
replace `scripts/check.sh full` or the remaining Windows acceptance work.

## Known gaps — not yet tested or certified

- Extended desktop use beyond the bounded post-fix smoke, and background processes of real agent runtimes.
- Cross-user/privileged named-pipe security negatives; same-user smoke is not security certification.
- Crash/power-loss durability and a completed Windows upgrade/re-exec procedure.
- Pi, Codex, and Claude agent launches, hooks, credentials/profile handling, and conformance.
- Non-ASCII installation paths, full font/glyph fidelity, resize, paste, reconnect, and broader UI flows.
- Rex operation and unsupported-backend behavior beyond the existing declarations.
- A fully passing native core, extension, integration, and release validation matrix.

## Recommended order of work

1. Keep post-fix desktop/descendant/restart/cleanup checks in the native regression path. The corrected
   experimental installation is available for user testing, but is not release-gate-qualified.
2. Resolve open-reader persistence and concurrent log rotation failures without weakening durability.
3. Implement a native Windows gate with receipts, usable tool discovery, private endpoints, and
   attributable process-tree cleanup.
4. Fix Unix-specific fixtures; rerun and classify remaining Rust/Python failures individually.
5. Provide a maintained Windows installer/upgrader and resolve licensing reproducibility.
6. Complete real runtime, security-negative, non-ASCII-path, and desktop UI acceptance tests.
