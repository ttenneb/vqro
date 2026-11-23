# Native Windows baseline gate (W06/W09/W12)

Run from a native PowerShell 5.1+ console:

```powershell
powershell -NoProfile -File scripts/check.ps1
# Optional explicit reusable build directory and latest receipt location:
powershell -NoProfile -File scripts/check.ps1 -TargetDir .run/windows-target -Receipt .run/windows-check.json
# Dependency-free tests for interpreter selection and native result handling:
powershell -NoProfile -File tests/windows_gate.Tests.ps1
```

This is a **baseline**, not the Linux full gate and not desktop acceptance. It
checks Python 3.11+, offline licence policy/notices, exact vendor bytes, manifest
dependencies, an all-target Cargo build with zero warnings, `cargo test`, and
Python unittest discovery. It neither installs tools nor installs/restarts vqro.
Cargo, the pinned Zig required by build.rs, and Python must already be available.
The licence checker requires all locked Cargo packages cached; missing offline
metadata prerequisites fail visibly rather than triggering an implicit fetch.
This is not a network-free gate: other Cargo build/test activity can fetch packages.
A failed build blocks the Cargo test step (recorded FAIL), not an implicit rebuild.
Full Python discovery is never removed or graded PASS around portability errors;
it is expected to FAIL on Windows until the separately reviewed W11 fixes land.

Interpreter discovery mirrors `src/program.rs`: literal `VQRO_PYTHON` first;
otherwise walk PATH directories, skipping directories named WindowsApps, checking
`py.exe -3`, `python.exe`, then `python3.exe` within each directory. A venv earlier
on PATH wins over a later launcher. The selected interpreter must be 3.11+.

Every child (including the source-revision query and first Python probe) uses a
cleared `ProcessStartInfo.EnvironmentVariables` populated from an explicit
allowlist. Only Windows OS/SDK/tool PATH discovery inputs, the pinned Zig
executable/cache overrides and approved Rustup toolchains/settings survive.
HOME, USERPROFILE, APPDATA, LOCALAPPDATA, TEMP/TMP and XDG config/cache/state are
private. Cargo gets a private CARGO_HOME with junctions to only the approved
registry/git package caches, never the real Cargo config or credentials files.
No native agent/runtime/account/auth/import overrides are inherited: PYTHONPATH,
PYTHONHOME, PYTHONSTARTUP and PYTHONUSERBASE are absent, not merely emptied.
VQRO_SOCKET_SOURCE and inherited HC_/HERDR_ configuration are absent. Every direct
Python launch also uses `-I -X utf8` (explicit flags, not ignored PYTHON* encoding
injection); `PYTHONNOUSERSITE=1` protects ordinary descendant
Python launches. No credentials are copied. Retained run directories can contain registry/git
cache junctions: unlink those junctions before any recursive cleanup; never
follow them into shared caches. This is environment isolation, not a
sandbox against trusted repository tests deliberately accessing other files.

Each run has explicit private VQRO_SOCKET/VQRO_STATE_DIR, empty extensions and
backend pipe/session; the
default herdr command is deliberately unavailable. The calling shell environment
and working directory are never modified. Retained logs/state and a fresh retained
receipt live under the printed unique run directory, plus a requested/latest
receipt. These files are not called immutable, exclusive or read-only; latest
publication is a direct write. A bootstrap or receipt write failure is nonzero,
not evidence that an older latest receipt qualifies this run. Bind evidence to
the fresh printed run path.

Capture, decoding, log write and log read failures are sticky FAILs even after a
native exit 0. Stdout/stderr drain concurrently; stderr warnings still count but
ordinary stderr is not itself failure. Both streams are retained (stdout then
stderr, not a promised interleaving). Native exit codes do not pass through a
PowerShell redirection pipeline. All seven expected terminal steps must occur
exactly once and the normal-completion sentinel must be true; empty, prefix,
interrupted and duplicate-step runs fail. The receipt declares expected steps and
normal completion explicitly.

Receipt fields follow `check.sh`'s kind/version/steps/result pattern with mode
`windows-baseline`. There is no Windows leftover scan: `leftover_scope: not-scanned`
is not a clean-process proof. No process-tree containment, full release qualification
or desktop acceptance is claimed; tests still own and clean up their children.
The native helper tests cover injected auth/PYTHONPATH startup canaries, private
roots, failed log writes/reads, ordinary stderr, stderr warnings, failed launch,
exit codes and empty/prefix/interrupted/duplicate/complete verdict controls.

## W12: reproducible encoding failure, not target filtering

The licence checker read `cargo metadata` using Python's locale-dependent default
text encoding. Cargo emits UTF-8 JSON. On a legacy cp1252 Windows Python process,
non-ASCII author names become mojibake, so generated notices differ even though
the package set and original licence bytes agree. For example, generic-array's
`Bartłomiej Kamiński` becomes `BartÅ‚omiej KamiÅ„ski`; lazy_static's `Marvin Löbel`
becomes `Marvin LÃ¶bel`. Decoding the current 214-package metadata as cp1252
reproduces those inventory differences. `test_cargo_metadata_is_utf8_even_with_a_cp1252_locale`
fails before the explicit UTF-8 subprocess decoder and passes afterward. Project
TOML, manifest JSON and vendored notice metadata reads also explicitly use UTF-8.
Original legal-byte hashing and byte-identical inventory comparisons are unchanged.
A stale check now prints the first differing line, using ASCII escapes so diagnostics
remain printable on legacy consoles; CRLF versus LF differences remain visible,
not normalized away.

A Windows CPython run under Wine, given the same metadata with translated manifest
paths and correct UTF-8 decoding, rendered the inventory byte-for-byte identical
(691912 bytes). The inventory, root licence and standard fallback texts already
have `-text` attributes, so normal Git autocrlf does not explain this particular
failure. No target filtering, package exclusion, hash normalization, or regeneration
of the inventory is needed for the demonstrated encoding bug. The native report
also records cp1252 diagnostics (W14), but confirming that this was the exact cause
of its stale inventory still requires rerunning the checker on that Windows host.

## Verification limits

PowerShell is not available on the Linux development host: the script and its
native tests are **not executed here**. Native PowerShell 5.1 must verify parser,
external process exit/stderr capture, zero-warning enforcement, receipt generation,
interpreter discovery, private-state/cache isolation and canary tests. The
Python encoding regression and unchanged offline inventory check run on Linux.
