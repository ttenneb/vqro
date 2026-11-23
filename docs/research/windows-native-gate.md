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

Interpreter discovery mirrors `src/program.rs`: literal `VQRO_PYTHON` first;
otherwise walk PATH directories, skipping directories named WindowsApps, checking
`py.exe -3`, `python.exe`, then `python3.exe` within each directory. A venv earlier
on PATH wins over a later launcher. The selected interpreter must be 3.11+.

The script retains Windows/toolchain discovery environment, removes inherited
HERDR/VQRO/HC endpoint configuration, and selects unique temporary vqro state,
empty extension directory, backend pipe/session, and XDG directories. The default
herdr provider command is deliberately unavailable. The environment and working
directory are restored afterward. Logs and temporary state are retained for
inspection under the unique printed run directory; they are not live install
paths. A new receipt is written there, plus the requested/latest receipt. Receipt
fields follow `check.sh`'s kind/version/steps/result pattern; `mode` explicitly says
`windows-baseline`. There is no Windows process-leftover scan, and the receipt
explicitly reports `leftover_scope: not-scanned` (never a fabricated clean scan).
This gate does not claim process-tree containment, a full release qualification,
or real desktop acceptance. Tests must still own and clean up their children.

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
interpreter discovery, private-state execution, and environment restoration. The
Python encoding regression and unchanged offline inventory check run on Linux.
