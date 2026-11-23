# vqro on native Windows (preview, untested on real Windows)

Status: vqro builds for Windows from source; it has been type-checked for `x86_64-pc-windows-gnu` on Linux and smoked
under Wine with the fake backend only. **Nothing here has been run on a real Windows machine yet.** Report what
breaks. WSL2 with the Linux build is the other way to run vqro on a Windows PC and is not covered here.

What works on Windows, by design:
- The core, the built-in terminal UI and the Python extensions talk over a named pipe
  (`\\.\pipe\vqro-<id>`, reachable only by your user, D22).
- Backends: **herdr-core** (native Windows; vqro finds `hc.exe` on `PATH`, or set `VQRO_HERDR`), the in-process fake
  backend for tests. **tmux is not available on native Windows** (the tmux provider says so and stops); use WSL for
  it. rex: not yet.
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

## 3. Install (by hand; no installer script yet)

```powershell
$bin = "$env:LOCALAPPDATA\vqro\bin"; New-Item -ItemType Directory -Force $bin, "$env:LOCALAPPDATA\vqro\extensions" | Out-Null
Copy-Item target\release\vqro*.exe $bin
Copy-Item -Recurse -Force extensions\* "$env:LOCALAPPDATA\vqro\extensions"
[Environment]::SetEnvironmentVariable("Path", "$bin;" + [Environment]::GetEnvironmentVariable("Path", "User"), "User")
```

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

## Known gaps

- No installer script and no `vqro` auto-update on Windows yet.
- The Claude Code hook bridge is Unix-only for now.
- Agent launchers (Pi bridge, tier-1 runs) start `python3` inside a backend terminal, resolved by that terminal's
  host, not by vqro. On Windows that is often missing or the Store stub: configure the runtime's launcher explicitly
  (for example `py -3`) until this is handled.
- tmux backend and coordination archive export: not supported on native Windows (they say so); see
  `docs/research/windows-extensions.md` for the full extension audit.
- Optional OpenTUI client: not checked on Windows.
- `scripts/check.sh` and the e2e harnesses are Linux-only (namespaces, cgroups, procfs).
- `tests/windows_background_smoke.py` refuses to run for now (exit 77): it is not isolated from your real profile
  and credentials yet. Check for console windows by hand: after `vqro tui` and a minute idle, no console windows
  other than your own terminal should be open, and none should appear later.
