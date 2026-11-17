# Searchable Extensions catalogue and static manifests — 2026-11-17

Human decisions: list + detail pane, a manifest per extension, local catalogue now; registry/search/install
policy later. Implementation uses existing supervised declarations and public workspace/command primitives.
No live installation or sessions were touched.

## Release

- `src/manifest.rs`: static UTF-8 JSON, name checked, regular files only, 32 KiB/file and 1 MiB aggregate cache.
  Default discovery is `<extensions dir>/manifests/<name>.json`; explicit declaration paths are supported.
  Reconciliation/list refreshes observed metadata; snapshot reads are cached. No import/version probe or install.
- All fourteen bundled manifests describe behavior, disabling consequences and contribution summaries checked
  against the extension/provider source. Package version is 0.1.0; unestablished authors/licenses are not invented.
- `extension.list`/snapshot facts include metadata, diagnostics, enabled/effective mode, supervisor generation and
  runtime status without public process ids. Private process-measurement/crash fixtures now inspect their own
  process trees instead; backend lifetime behavior is unchanged.
- `src/tui/panel.rs` supplies optional `list_detail` and wrapped focusable text, responsive geometry, independent
  detail state, search/count/range, viewport paging, group headers and normal row actions. Enhancement-scoped
  requirements leave a plain-list fallback for older clients. Cached flattening remains in use.
- `extensions/settings.py` is uniform: metadata, status and generic actions, no Pi/self special cases. On/off in
  safe mode uses D21 session allowances, not saved enabled edits. Log paths are disclosed with an honest note that
  no in-app log viewer exists. Root prefix recovery works inside popups/search.

## Verification and rendered capture

Evidence root: `~/.local/share/herdr/scratch/prim-independent-review/`.
- `extensions-focused-final.log`: loader/API/renderer/input tests, including budget rejection, disabled/failed/
  safe metadata, cached refresh, responsive geometry, fallback, wrapped scrolling, paging/chip focus, and recovery.
- `extensions-tui-final.log`: private real TUI + herdr/tmux harness, **17/17**. Search/count, row selection without
  effects, restart preserving search, manifest settings command, paging, keyboard and checkbox toggles, disabled
  description, failed-program reason, wide/narrow rendering, quit and no leftovers all pass.
- Exact rendered text: `extensions-captures/extensions-wide.txt` (120×32) and `extensions-narrow.txt` (72×36).
  The narrow capture deliberately replaces the private icons executable with a missing fixture to show failure
  diagnostics while retaining its static description. It is not a failure in the installed icons extension.
- Runtime/About/Contributes are real header rows: palette accent + bold, also asserted on rendered buffer cells.
  Kind/version render on separate lines; missing programs say neutral “extension program,” not “provider.”
- `extensions-full-green.json`: **18/18** full scoped steps; **215 Rust passed / 21 optional ignored** in the regular
  suite, **127 Python passed**, zero warnings, scale budgets and cgroup no-leftover checks pass. Optional private
  backend suites and both Pi variants pass too. XDG runtime/user-bus variables were explicitly set.

The first full attempt found harness assumptions about public pids and a direct private-handler unit assertion
following recovery interception moving to the common input path; fixtures were adapted, not execution semantics.
The following full gate passed. Final clean-commit receipt is reported alongside the delivery.

Contracts/examples: `../spec/core-protocol.md`, `../extensions.md`. No registry, package installation, dependency
solver or generic log viewer was added. Windows transport/runtime checks remain outside this Linux TUI evidence.
