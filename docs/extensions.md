# Extension metadata and the Extensions page

Settings → Extensions is a local catalogue of **declared** extensions, not a registry or installer. The list
shows a checkbox, title/name, short summary and runtime badge. Select a row to read what it does and what is lost
when it is turned off. Static information remains available when the extension is off, failed or stopped by safe
mode. A missing/bad manifest affects information, not whether the program is permitted to run.

## Browsing and testing

- `/` starts a list search (title/name, summary and tags); Enter finishes typing, Esc clears the search.
- Up/Down selects a row. PgUp/PgDn moves by the visible row count; Home/End selects first/last. Group headings,
  result count, visible range and scrollbar keep position readable. `[` and `]` change Settings pages.
- Tab/Shift-Tab switches between list and detail. Arrow keys retain their usual chip/tree behavior in either.
- A mouse click on a list label only selects it. The checkbox, or Enter/Space on the list, deliberately toggles
  it. Detail buttons are explicit actions. The wheel scrolls detail text independently.
- The detail includes runtime status, crash count, last-exit reason, manifest kind/version/source, requirements,
  contribution summaries and controls. Restart retries the declared program; it is unavailable when not enabled.
  An extension's settings button comes from its manifest, not from a hard-coded name.
- On/off edits the normal enabled declaration. If Settings is explicitly run in safe mode, on/off instead uses
  the existing session-only recovery allowance; saved enabled declarations remain unchanged.
- Turning Settings itself off stops that page. The vqro badge/prefix still reaches commands and recovery,
  including enabling it again. The prefix remains available inside the popup and while searching.
- View log (when the `logs` extension runs) opens the extension's core-host log in a following, filterable viewer;
  the detail also reports the log path for external inspection.
- Dependencies (D29): the detail lists **Needs**, **Works with**, **Used by** and **Provides**, each with its status;
  a list badge says `needs Tabs (off)` (or `not installed`). Turning on an extension whose needs are off opens one
  confirmation: turn on both (default), turn on this one only, or cancel. A provider that is not installed is named
  with its suggested extension; the extension can still be turned on. Turning off an extension that enabled
  extensions need warns once with what breaks (and what is merely limited) before switching. If only optional users
  remain, it switches directly. vqro never switches anything by itself; the built-in safe-mode chooser only shows
  the `needs` text.

Wide windows put details beside the list; narrow windows put them below. Settings uses the generic `list_detail`
panel enhancement in `spec/core-protocol.md`; other extensions can use exactly the same primitive.

## Checking your installed setup

Run `vqro run test.unit` to check observed extension status, crashes, manifests, dependencies, launch paths,
log tracebacks and any installed `tests/<extension>/test_*.py` suites. `test.unit.show` reopens the last panel.
There are no background checks. The report only reads core facts; it never restarts or changes extensions.
Receipts include a fingerprint and coverage gaps, live under `<state dir>/test-unit/` (0600), and retain 20 runs.

When the core or extensions are broken, use the plain CLI:

```sh
python3 ~/.local/share/vqro/extensions/testing_unit.py --json
python3 ~/.local/share/vqro/extensions/testing_unit.py --offline --state-dir /path/to/state --no-tests
```

Options include `--socket`, `--extensions-dir`, `--strict` (warnings fail), and `--out FILE` (CLI writes no
receipt unless requested). Offline mode reads `workspace.json` without changing it. Exit codes are 0 pass,
1 fail, 77 unverified (no extension observed running), and 2 harness error, including zero declarations.
Disabled extensions have their metadata and launch paths checked, but do not run tests or fail for broken paths.
Tests use a private temporary HOME and a scrubbed environment with only PATH, HOME, and
`VQRO_TEST_EXTENSIONS_DIR` (the absolute installed extensions directory, not an endpoint). The CLI allows 120 seconds
per extension; the in-app command shares a 20-second tests budget and reports remaining tests as unverified.
Run the CLI for full tests if that budget is exhausted. This is cooperative isolation, **not a security sandbox**: only run installed tests you trust.

No installed tests means **no coverage**, not a test pass. `scripts/dev-install.sh` ships the mocked unit suites
listed in `extensions/tests.json` (`suites` maps bundled declaration names to repository test/helper files;
`not_shipped` records exclusions). Each mapped `tests/<name>/` directory is replaced on install so removed tests
do not linger; unrelated extension suites are left alone. Extensions can ship their own `tests/<name>/test_*.py`
stdlib unittest suites using the same convention. Bundled tests resolve code through `VQRO_TEST_EXTENSIONS_DIR`
when set, otherwise the repository-relative extensions directory. Shared pure helpers are copied beside each suite.
The workspace/pane suites exercise several cooperating bundled extensions against an in-memory fake core.
A healthy status does not prove behaviour, rendering or performance; use the separate disposable integration
checks for behaviour. Performance checks are not built yet. `--no-tests` skips even installed tests and records that coverage gap.

### Disposable integration checks

`vqro run test.integration` starts a subprocess and immediately returns its future receipt path;
`test.integration.show` opens the completed report. It copies declarations, credential-free documents and
extension files into a private core, audits launch paths, then checks startup, dependencies, registered commands,
views, safe-mode recovery, revert, restart and process cleanup. It never invokes extension commands during the
smoke or applies anything to the source workspace. Providers are disabled with the default fake backend.

```sh
python3 ~/.local/share/vqro/extensions/testing_integration.py --json --vqro /path/to/vqro --out receipt.json
python3 ~/.local/share/vqro/extensions/testing_integration.py --candidate-ext /path/to/overlay --candidate-ops changes.json
```

Source selection also accepts `--socket`, `--state-dir`, `--extensions-dir` and `--offline`. Scenarios live at
`tests/<extension>/scenarios/*.json` beside the extension. Steps support `apply`, `invoke`, event-driven `wait`,
and immediate `expect`, each with a bounded `within_ms` deadline. Agent-runtime commands are not invoked in
this slice. New scenarios (`"new": true`) must fail on the baseline and pass on the candidate; passing both is
`weak` and yields exit 77, never a pass. Existing failures are distinguished from regressions in comparisons.

`--backend private` allows sandbox-local herdr-core/tmux providers; the harness stops only its private endpoints.
External launch paths stay off and unverified unless explicitly allowed with `--allow-external`. Credential-like
workspace entries and contributed documents are excluded and listed; the extension tree is copied whole,
with symlinks preserved rather than dereferenced. Protected documents refused by the private core are reported
as not copied. This is
cooperative isolation, not a security sandbox: only run extension code and scenarios you trust. Agent profiles
are not copied yet. Use `--self-test` for crashing-extension and zero-coverage negative controls.

`--root DIR` must be absent or empty and short enough for sockets under 100 bytes. Copies are removed unless
`--keep`; CLI receipts are written only with `--out` (choose a path outside the disposable root). The report
records source seq/generations before and after; concurrent source changes are unverified, not attributed to the
harness. Exit codes remain 0 pass, 1 fail, 77 unverified, 2 harness/canary error.

## A manifest is data, never an executable probe

Each bundled extension has UTF-8 JSON at `extensions/manifests/<name>.json`. The supervisor looks for
`<extensions directory>/manifests/<declaration name>.json` by convention. An explicit declaration field
`manifest: "some/path.json"` selects a different file: absolute paths are used as supplied, relative paths are
relative to the core's extensions directory (`VQRO_EXTENSIONS_DIR`). These are core-host filesystem paths, not
endpoints and not paths on a terminal's execution host.

Example:

```json
{
  "name": "example",
  "title": "Example controls",
  "summary": "Browse and inspect example resources.",
  "description": "Adds a resource list and inspection commands. Turning it off removes those controls, not the resources owned by the backend.",
  "version": "0.1.0",
  "kind": "interface",
  "author": "Example author",
  "license": "MIT",
  "homepage": "https://example.org/example-extension",
  "requires": {"protocol": 1, "client_features": ["items", "menu"]},
  "provides": ["command/example.inspect", "command/example.settings"],
  "needs": [{"cap": "command/tabs.show", "suggest": "tabs", "why": "Inspected resources open in a tab"}],
  "works_with": [{"cap": "data/themes", "suggest": "themes", "without": "Previews use the default palette"}],
  "contributes": {
    "commands": ["example.inspect"],
    "keys": ["Prefix e: inspect examples"],
    "menu_items": ["Inspect example"],
    "panels": ["Example list"],
    "settings": ["doc/settings/example"]
  },
  "settings_command": "example.settings"
}
```

Only `name` is required; it must match the declaration's name. All other fields are optional, unknown fields are
preserved for future clients and ignored by the current catalogue. Use accurate plain language, a one-line
summary, and a short description that explains the consequences of disabling it. Do not invent authors, licenses
or versions. Bundled manifests use the package version and the established author `Bennett Garcia` and licence
`Apache-2.0`; no homepage/repository URL is invented. Other extension authors declare their own actual metadata.
The bundled extension code and SDKs share the root Apache-2.0 licence; third-party extensions retain their own terms.
Kinds are open text categories (`provider`, `runtime`, `interface`, `management`, ...), not permissions.

`provides`, `needs` and `works_with` declare capabilities (`kind/name[@major]`: commands, views, live data, workspace
documents, abilities, client UI features; see the protocol, Static manifests). Declare every other extension's
command, view, live data or document you use: in `needs` when your main purpose fails without it, otherwise in
`works_with` with a `without` line. Bundled manifests are checked by `scripts/check_manifest_deps.py` (part of
`scripts/check.sh`). The core reports resolution; it does not solve, install or enable anything.

`requires` and `contributes` are descriptive metadata today, **not** dependency solving, capability authority or
registration. Actual commands/contributions still use the public protocol. A string `settings_command` names its
own settings action, enabled only when that command is available and the extension is running. No execution
configuration or credential belongs in the manifest. The declaration still owns `argv`, `cwd`, `enabled`, provider
default selection and lifetime behavior.

The same JSON object is suitable for a future repository listing: there is no local-only runtime state inside
it. Registry discovery, download, verification, install and upgrade policy are deliberately deferred.

## Observation and limits

`extension.list` and snapshots combine metadata with platform-neutral runtime facts. Pids stay internal; clients
use running/status and the ephemeral supervisor generation, not operating-system process inspection. See the
protocol for field names and status meanings. Unknown/missing fields should retain a useful fallback list.

Regular files only; at most 32 KiB per manifest and 1 MiB cached records in aggregate. Invalid/mismatched JSON,
read errors and budget omissions appear as `manifest_error`. The supervisor never starts the program to ask what
it does or obtain a version. Reconciliation and `extension.list` check file stamps; snapshots reuse the last
observed cache. Editing a file is seen on the next list refresh; there is no background watcher.

`scripts/dev-install.sh` copies the bundled manifest directory with the extension code. This guide does not
change the rule that a live install/restart requires the human's approval. Development tests use private state,
endpoints, HOME and backend sessions. The pty regression in `tests/e2e_tui_providers.py` emits rendered wide/narrow
captures and checks controls against private providers; its injected missing-program failure is intentional.
