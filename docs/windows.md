# vqro on native Windows (preview)

Status: native MSVC builds and limited named-pipe, extension and herdr-core terminal smokes have run on Windows.
**This is not daily-use qualification.** A desktop console-window storm escaped the headless smoke; repeat the
Desktop acceptance checklist below after every push. The PowerShell installer still needs native acceptance. Report what
breaks. WSL2 with the Linux build is the other way to run vqro on a Windows PC and is not covered here.

What works on Windows, by design:
- The core, the built-in terminal UI and the Python extensions talk over a named pipe
  (`\\.\pipe\vqro-<id>`, reachable only by your user, D22).
- Backends: **herdr-core** (native Windows; vqro finds `hc.exe` on `PATH`, or set `VQRO_HERDR`), the in-process fake
  backend for tests. **tmux is not available on native Windows** (the tmux provider stays up and reports that tmux is unavailable); use WSL for
  it. The optional rex provider exists but is untested on Windows.
- State lives in `%LOCALAPPDATA%\vqro\state`, bundled extensions in `%LOCALAPPDATA%\vqro\extensions`
  (`VQRO_STATE_DIR` / `VQRO_EXTENSIONS_DIR` override).
- Extensions whose `argv` says `python3` run with `py -3`, `python` or `python3`, whichever comes first on `PATH`
  (WindowsApps directories skipped entirely to avoid Store stubs); set `VQRO_PYTHON` to a `python.exe` to choose.

## 1. Prerequisites (Windows 11, PowerShell)

Check what you have:

```powershell
winget --version; git --version; rustup --version; cargo --version; zig version; py -3 --version
```

Install what is missing:

| Tool | Version | Install |
|---|---|---|
| Git | any recent | `winget install --id Git.Git -e` |
| Visual Studio Build Tools (MSVC + Windows SDK) | 2022 | `winget install --id Microsoft.VisualStudio.2022.BuildTools -e --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"` (skip if Visual Studio with "Desktop development with C++" is installed) |
| Rust (rustup, MSVC host) | stable, edition 2024 (1.85 or newer; built here with 1.96.1) | `winget install --id Rustlang.Rustup -e`, then a new terminal and `rustup default stable-x86_64-pc-windows-msvc` |
| Zig | **exactly 0.17.1** (the vendored terminal engine needs it) | download `zig-x86_64-windows-0.17.1.zip` from https://ziglang.org/download/, unzip to `%USERPROFILE%\.local\zig-0.17.1\` so `%USERPROFILE%\.local\zig-0.17.1\zig.exe` exists (the build looks there), or set `$env:ZIG` to its `zig.exe` |
| Python | 3.11 or newer | `winget install --id Python.Python.3.13 -e` |
| herdr-core | current | per herdr-core's own Windows instructions; `hc.exe` on `PATH` |

Open a new Windows Terminal tab after installing so `PATH` updates.

## 2. Build

```powershell
git clone <vqro repository> vqro; cd vqro    # or: git pull
cargo build --release
cargo test --lib
```

The first build fetches the terminal engine's Zig packages (~310 MB) into `%LOCALAPPDATA%\vqro\zig-pkg` once.

## 3. Install or upgrade (PowerShell 5.1 or 7)

Close all vqro UIs and bridge processes first so they cannot restart the core during installation.
The script uses one outer confirmation for the entire operation (required backup copies cannot be skipped
individually). Before building, it refuses links/junctions/reparse points in managed paths and their ancestors,
wrong destination types, and overlapping install/source/build/backup directories. Custom `-BinDir` must be outside
the checkout and Cargo output. Separation checks use normalized volume-GUID physical paths (including SUBST
aliases), not drive-letter prefixes; volumes whose final physical identity cannot be read are refused.
The root is derived from the script location and must match Git's exact physical checkout root; there is no
wildcard-capable `-Root` argument. Filesystem operations use literal paths, including checkouts with brackets in
their names. Reparse/junction paths are refused rather than followed; extended path forms must pass the same
physical-volume identity checks or fail closed.
Cargo metadata must report both `target_directory` and `build_directory` (verified with Cargo 1.96.1; older
versions without the latter field are refused). Both final output and independent intermediate output
(`build.build-dir` / `CARGO_BUILD_BUILD_DIR`, including Cargo's resolved templates) are checked against protected
install, backup and source trees **before building**. Executable paths come from that build's JSON artifact
messages, not a guessed `target/release` directory. Duplicate binary-name records are refused rather than
last-wins; all seven must belong to the checkout package, one profile and one physical output directory. The script builds release
binaries, checks all seven and the bundled test mapping, requests protocol shutdown
if a core is running, and waits for vqro processes to exit. If they remain, it refuses to install; it never
forcibly kills processes. Backend-owned terminals are not stopped by `vqro stop`.

```powershell
.\scripts\dev-install.ps1 -WhatIf       # plan only: no build, shutdown or writes
.\scripts\dev-install.ps1               # build and install; leave core stopped
# Optional explicit user-PATH change:
.\scripts\dev-install.ps1 -AddToPath
```

Defaults: `%LOCALAPPDATA%\vqro\bin` for executables, `%LOCALAPPDATA%\vqro\extensions` for bundled content.
Use `-BinDir "$env:USERPROFILE\bin"` to upgrade the earlier manual install in that directory instead; avoid
leaving old binaries earlier on PATH (`Get-Command vqro -All`). Python discovery follows `VQRO_PYTHON`, `py -3`,
`python`, `python3`, skipping WindowsApps stubs; as in the app, PATH directories are searched in order, trying
`py`, `python`, then `python3` within each directory. Python 3.11+ is required. The installer includes manifests,
mapped unit suites, optional rex files, the tier-1 Pi plugin, text client and Python packages, but not OpenTUI
or herdr-core. It prints installed executable hashes, version and source commit (labelled dirty when the working tree is dirty).

PATH is unchanged unless `-AddToPath` is supplied; otherwise the script prints the exact user-PATH command.
Open a new terminal after changing PATH. Run `vqro tui` to start the installed core. Before replacing files, the script copies existing seven binaries and the entire extensions directory into a
unique UTC-timestamped `%LOCALAPPDATA%\vqro\backups` folder and prints its path. It verifies the entire backup
inventory and SHA256 hashes against the prior install before writing `inventory.json` and the `restore.txt`
completion marker, then replacing anything. The inventory records prior presence/absence for all seven binaries
and the extensions root, including hidden files and empty directories. Previous backups are retained
(no automatic deletion); keep at least the last few known-good installs. On failure it prints restoration guidance:
close vqro processes, verify `restore.txt` and backup hashes against `inventory.json`, and reject any links/reparse
points before restoring. For each `Binaries` entry with `Present=true`, copy the saved file back; for each with
`Present=false`, remove **only that named managed executable** from the failed install. Preserve unrelated BinDir
files. Move the failed extensions directory aside; copy the backup directory back only when
`ExtensionsPresent=true`, otherwise leave extensions absent. This also removes newly introduced managed binaries
from a failed first install rather than overlaying an incomplete old set. An incomplete backup is not safe to
restore blindly. Staging and renaming protects
each executable from truncated copies, not the entire install from every failure: a later I/O error can leave
mixed versions. Keep UIs closed, correct the error and rerun before launching. This development installer does
not substitute for the release gate (the existing Unix full gate is not a native Windows qualification).

## 4. Smoke test with the fake backend (no herdr-core needed)

Use a scratch state so nothing touches a later real setup:

```powershell
$env:VQRO_STATE_DIR = "$env:TEMP\vqro-smoke"; $env:VQRO_HERDR = '["cmd.exe","/c","exit","1"]'
vqro serve --fake          # tab 1: leave running
vqro tui                   # tab 2 (same two env lines first): the UI opens; the sidebar, tabs and status bar come from Python extensions
vqro stop                  # tab 2 afterwards: the core stops
```

Things to report: does `vqro serve` start, does `vqro tui` connect and draw, do the extensions show up (no
"failed" extensions in the status bar), does a fake terminal open, does `vqro stop` stop it, and the files
`$env:VQRO_STATE_DIR\serve.log` and `ext-*.log` if anything fails.

## 5. With herdr-core

Remove the two smoke variables (`Remove-Item Env:VQRO_STATE_DIR, Env:VQRO_HERDR`), make sure `hc --version` works,
then `vqro tui`. New terminals start herdr-core's shell for your machine (PowerShell or cmd).

## State-file readers and writes

A process holding `workspace.json` open can block writes on Windows, even if that reader allows delete
sharing. vqro keeps its write-through replacement barrier and retries a refused rename with bounded
backoff (about one second of scheduled waits). If the reader stays open, the write fails with `io`:
the original file and committed workspace state remain unchanged, and the candidate temporary file
is cleaned up. Close the reader before explicitly trying a new write. No transaction or agent input
is replayed automatically; POSIX-semantics replacement is not used as a durability fallback.

## Desktop acceptance checklist (after every push)

Run in a real logged-in Windows desktop, not only ConPTY/headless automation. Record commit, Windows/PowerShell
versions, installer output and observations. Use a private `VQRO_STATE_DIR` and backend endpoint/session per
herdr-core's instructions; do not reuse a daily workspace. Clear inherited `VQRO_SOCKET` and
`VQRO_SOCKET_SOURCE` first so the scratch state selects its own pipe. Never kill unrelated Python processes.
If any extra console appears, stop the scratch core immediately and report failure; do not leave it running.

1. **Idle desktop:** start the installed `vqro tui`, wait 60 seconds and inspect the desktop and taskbar.
   Click the `vqro` badge to open recovery/commands and dismiss it; confirm the TUI remains responsive.
   **No console windows appear** beyond the user's terminal. In a second
   PowerShell window run:
   ```powershell
   Get-Process | Where-Object {
       $_.ProcessName -match '^(vqro.*|python.*|py|hc|herdr-core)$' -and $_.MainWindowHandle -ne 0
   } | Select-Object Id, ProcessName, MainWindowHandle, MainWindowTitle
   ```
   Expected: zero additional top-level windows from background vqro/Python/hc children; only the user's terminal
   windows are visible. Record the count. Windows Terminal/conhost may own console windows rather than the child,
   so a zero process-list result alone is **not** a pass: inspect the desktop and Task Manager process tree too.
2. **Extension restart:** identify one scratch extension by PID, parent core and command line (not just its name):
   ```powershell
   Get-CimInstance Win32_Process | Where-Object { $_.Name -match '^python.*\.exe$' } |
       Select-Object ProcessId, ParentProcessId, CommandLine
   # After checking the exact scratch extension PID:
   Stop-Process -Id <extension-pid>
   ```
   Expected: its supervised replacement has a new PID, extension functionality returns, and no new window appears.
   Repeat the visible-window count and desktop inspection.
3. **Shutdown cleanup:** close the scratch TUI first (so it cannot restart the core), then `vqro stop` in its
   scratch environment. Inspect the same process tree/command lines after up to 15 seconds. Expected: no scratch
   vqro core/provider or Python extension processes remain. Backend-owned interactive terminals may persist by
   design: close/forget only the terminals created by this test, and shut down its private backend separately.
   Do not demand that unrelated user Python or hc processes exit.
4. **Fake-backend smoke:** run section 4 in private state, including both environment lines in each tab.
   Expected: TUI draws, extensions load without failed status, a fake terminal opens, and `vqro stop` shuts down
   the scratch core cleanly with no extra desktop windows or extension processes. Save logs on failure.
5. **Real terminal:** remove the fake override and use a private herdr-core endpoint/session, then start `vqro tui`.
   Click the `vqro` badge, choose the new-tab command (or press Ctrl-g then n), type
   `echo vqro-desktop-ok` and confirm the output; close that test terminal with Ctrl-g then x.
   Expected: responsive input/output inside the TUI, no separate extension console, and the test resource is gone.
   Close the TUI, stop the scratch core and private backend, and verify cleanup again. Never shut down the daily backend.

## Known gaps

- No `vqro` auto-update on Windows yet; the development installer above requires native acceptance.
- The Claude Code hook bridge is Unix-only for now.
- Agent launchers (Pi bridge, tier-1 runs) start `python3` inside a backend terminal, resolved by that terminal's
  host, not by vqro. On Windows that is often missing or the Store stub: configure the runtime's launcher explicitly
  (for example `py -3`) until this is handled.
- tmux backend and coordination archive export: not supported on native Windows (they say so); see
  `docs/research/windows-extensions.md` for the full extension audit.
- Integration/performance diagnostic sandboxes (`test.integration`, `test.perf`) are not supported on native
  Windows yet; their constructor refuses before filesystem/process effects. The unit-health diagnostic uses the
  shared local transport, but its standalone no-argument directory/endpoint defaults remain Unix-shaped: select
  the endpoint and directories explicitly with `--socket`, `--state-dir` and `--extensions-dir`.
- Optional OpenTUI client: not checked on Windows.
- `scripts/check.sh` and the e2e harnesses are Linux-only (namespaces, cgroups, procfs).
- `tests/windows_background_smoke.py` refuses to run for now (exit 77): it is not isolated from your real profile
  and credentials yet. Check for console windows by hand: after `vqro tui` and a minute idle, no console windows
  other than your own terminal should be open, and none should appear later.
