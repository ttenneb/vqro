# Extension metadata and the Extensions page

Settings → Extensions is a local catalogue of **declared** extensions, not a registry or installer. The list
shows a checkbox, title/name, short summary and runtime badge. Select a row to read what it does and what is lost
when it is turned off. Static information remains available when the extension is off, failed or stopped by safe
mode. A missing/bad manifest affects information, not whether the program is permitted to run.

## Runtime Python

Supervised extensions with literal `python3` argv[0] use core interpreter discovery. On Unix, the core
checks `VQRO_PYTHON` first, else walks PATH then missing Homebrew directories `/opt/homebrew/bin` and
`/usr/local/bin`, selecting Python 3.11+. Version checks are isolated, bounded to two seconds per candidate,
with a shared five-second search/probe budget and cached until core restart. The core runs discovery off its
mutex; Python extensions remain `starting` while recovery and other calls continue. It revalidates current
declarations/generations before spawn, using the same absolute interpreter path that passed the probe even if
the child cwd differs. An invalid override fails instead of falling back. Configure the core's launch
environment for GUI/launchd startup; see [setup](setup.md#macos-runtime-python). Windows discovery is unchanged.

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
UI dependency facts caused solely by the checkers' own non-rendering client labels are ignored; another
client's unknown or missing UI support still warns. Other unmet dependencies remain visible.
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
`not_shipped` records exclusions). Before copying installed files, the installer validates the mapping, sources
and every test output destination (`scripts/install_extension_tests.py --check DIR` writes nothing);
symlinked test parents, suite directories or `tests.json` are refused, as are outputs of the wrong file type. Each mapped `tests/<name>/` directory is staged beside its destination before
replacement so failed copying leaves the old suite intact and removed tests do not linger. Publication uses
renames with rollback on failure; unrelated extension suites are left alone. Extensions can ship their own `tests/<name>/test_*.py`
stdlib unittest suites using the same convention. Bundled tests resolve code through `VQRO_TEST_EXTENSIONS_DIR`
when set, otherwise the repository-relative extensions directory. Shared pure helpers are copied beside each suite.
The workspace/pane suites exercise several cooperating bundled extensions against an in-memory fake core.
A healthy status does not prove behaviour, rendering or performance; use the separate disposable integration
checks for behaviour. Use the separate performance report for repeated timing and size measurements. `--no-tests` skips even installed tests and records that coverage gap.

### Current setup evidence and the optional finish rule

The completion rule is **off by default**: `doc/settings/testing.require_unit` is an optional boolean workspace
entry, treated as false when absent. Only arm it deliberately after a clean setup check. Arming is itself a setup
change: record fresh Unit evidence and attach Integration or explain its omission afterwards. This extension does
not prevent prose such as “done”, add a Pi runtime veto, or enforce policy for every runtime. Participating
plugins and explicit finish tools may use its deterministic assessment; this is cooperative workflow policy,
not a security boundary. Terminal (tier1) Pi and the negotiated default RPC launcher can load the plugin explicitly.
Missing plugin/skill assets do not stop Pi launching: they produce one unavailable notice/log and no testing flags.

RPC admits exactly one separate bridge field, `--testing-plugin`, before the native `--`: the regular installed
`tier1/vqro-testing.ts`, anchored to the bridge host's extensions directory. Its sibling testing skill is derived,
never another arbitrary path. The bridge maps that field to native `-e`/`--skill`; arbitrary native `-e`, extension
and prompt-template flags remain refused. Only `VQRO_TESTING_SOCKET` and `VQRO_TESTING_STATE_DIR` are added to the
native protected-name allowlist. The existing unrelated environment policy is unchanged. The isolated host wrapper
probes the resolved bridge's `--help` once per path/mtime, with a two-second execution and bounded output. An older
bridge that does not advertise the field gets one warning and a single ordinary launch without the field, never a
retry. Unknown configured `bridge_argv` launchers also stay unhooked/NOT VERIFIED rather than receiving an
unnegotiated field.

Passing argv is not loading evidence. The bridge publishes `resource.data.agent.testing_plugin` only for an
admitted plugin: `state: loaded | unverified`, installed `path`/`skill`, existing per-launch `principal`, and
`source: rpc:get_commands`. `loaded` requires Pi itself to report the marker command `vqro-testing-status` from
that exact plugin and the derived skill path. The marker is registered after the hooks and finish tool, and its
handler only inspects evidence; it never requests inference. These are registration/path facts, not loaded-byte
attestation or proof the skill body was read.

`test.setup.status {rpc_principal: <launch principal>}` (CLI `--setup-status --rpc-principal ...`) removes the RPC
caveat only for that available, confirmed launch; the plugin requests this scope in RPC mode using the already
inherited principal. It is read-only display scope, never owner authority or a finish override. `rpc_testing`
lists the individual launches and whether that scope is confirmed. The global menu/settings/receipt view keeps
its caveat, and old, unavailable or unconfirmed launches remain visible as unverified:
**NOT VERIFIED: rule not active for vqro-launched Pi agents yet**.
The bottom-bar segment is deliberately compact (warn style): while policy is ON it shows `tests !` when
finish is not ok. With ON and an enabled RPC Pi launcher, an accepted finish still shows `not verified`:
accepted test evidence does not imply that the RPC rule is active. With policy OFF it is hidden for missing/unverified/stale evidence
and RPC caveats; only an actual current Unit/Integration FAIL may show it. Performance never shows a segment
by itself. Clicking the segment opens the full setup/health report; full diagnostics are not crowded into the bar.
The injected prompt/skill requires the finish tool while the current authoritative policy is ON, even when
receipts are stale or NOT VERIFIED. OFF or genuinely unknown/unavailable policy does not impose an unconditional
rule. Run-start policy labels are samples; public per-request context conveys observed policy changes without
requesting another provider turn. Malformed policy is explicitly unknown, never inferred OFF.

`test.setup.status {}` returns the current `setup_id`, observed `generation`, `require_unit`, explicit
`policy_state` (`on`, `off`, `unknown`), Unit/Integration/Performance evidence and `finish_ok`. Unit is required only when armed: a current recorded Unit pass or an
explicit current owner exception is then necessary, plus an Integration receipt or a nonempty skip explanation.
When required, an off/missing/failed testing extension also blocks finish unless a current owner exception applies:
changes are not tracked while it is off. The built-in recovery surface can make the extension accessible for an
explicit owner action; stale exceptions are never carried across setup changes. Performance recommendations
and results never gate finish. A Unit pass with no installed tests remains a pass;
`coverage_gaps` and the Testing page make that missing coverage visible without silently claiming it was tested.

```sh
python3 extensions/testing_unit.py --setup-status --json --socket SOCKET --state-dir STATE --extensions-dir EXTENSIONS
python3 extensions/testing_unit.py --record --json --socket SOCKET --state-dir STATE --extensions-dir EXTENSIONS
```

A status evidence-store/observer failure still exits 2 and refuses finish. Its version-1
`vqro.setup-status-error` JSON document carries policy only from that call's successfully read snapshot:
ON remains mandatory despite the evidence error; an unavailable/malformed policy is unknown, never a cached ON.
The Pi adapter accepts exit-2 policy only after strict whole-document validation (null identity, no receipts,
error/unverified suite states, `finish_ok:false`); arbitrary/malformed stdout or other exit codes establish no policy.
The in-app invalidate-error path also reads a fresh snapshot, rather than treating its cached ON flag as authority.

`--record` owns the receipt filename under `<state>/test-unit/`, writes atomically at 0600 and retains 20 runs.
It cannot be combined with `--no-tests`: deliberately disabled installed tests cannot grant completion.
Recorded in-app checks with `tests:false` remain unverified; adoption requires `tests_enabled:true`.
In-app Unit uses recording only while policy is ON. OFF status reads/ordinary health checks do not import or
lock the Unix recorder; an OFF health receipt is ad hoc, not finish evidence. Explicit CLI `--record` still
requests recording. If its lock/observer is unavailable, ordinary checks still run: `health_verdict` reports
their outcome, `recording.state` is `unsupported`, `recorded` is false, and finish evidence is NOT VERIFIED.
No no-op lock is substituted and no unsupported recording is published as a current receipt. A failed armed
recording attempt latches observer invalidation: recovery cannot silently reuse an older Unit PASS after a
new failed health report. A newly completed recorded run is needed for a current acknowledgement.
JSON includes its path. `--out` is mutually exclusive, ad hoc and **never adopted** as Unit completion evidence.
Status reads adopt only complete, supported, recorded receipts for this setup, generation and selected instance;
wrong suites/instances, old generations, incomplete JSON and unfinished runs never count as a pass. Integration
and Performance can report `stale`: that receipt is non-current and never grants finish. Integration still needs
a current completed receipt or an explicit current skip reason; stale Performance evidence remains advisory and
never blocks finish. Status-query exit 0 means an assessment was read, not that `finish_ok` is true; checker/read
errors return exit 2 and NOT VERIFIED.

Fingerprint version 2 covers `extension/*` declarations, `doc/settings/*` values, recursive installed packages,
tests/scenarios and assets, and each declaration's resolved executable/script bytes. It excludes runtime
receipts/logs/results and caches, including the checker's own state. Limits are 10,000 entries, 32 MiB per asset,
256 MiB per executable, 512 MiB total and depth 64. A disabled declaration's missing or unresolvable
program/script is deterministically fingerprinted with its declaration bytes and an `absent` marker; it does not
make identity unknown. Bare-program fallback preserves first-match PATH ordering: a readable earlier input
wins, uncertainty before a usable match is unknown, and inaccessible later entries cannot override that match.
Appearance of that input changes identity, and enabling an unresolved declaration makes
identity unknown. Existing files that cannot be read (even for disabled declarations), oversized inputs and
unresolved enabled inputs make identity unknown (`setup_id: null`), never a successful empty digest. External directory links and an overlapping
state/extensions root are unsupported inputs. Hashes describe configured bytes, not verified loaded images or
arbitrary unmanaged imports. Selected endpoint/state/extensions paths and the core epoch bind the evidence.

Unit fingerprints before and after checking. Concurrent changes make closure unverified. Observed entry changes,
including change/revert, increment the persisted generation while armed or explicitly recording; armed
restart/missed events invalidate prior evidence and rescan. OFF assessments use read-only previews, not a
persisted observer. Filesystem changes are detected on explicit status checks and while armed evidence is pending. This does
not claim perfect detection of unobserved transient external file edits. Live notices use only `view.publish`:
`testing.status`, a status-menu item, and `settings.page.testing`. The unscoped/unconfirmed RPC caveat is retained
in bounded Unit/Integration/Performance error/size fallback receipts, not dropped by replacement reporting.
Missing, stale, failed, pending or unknown evidence
is prominent; a coverage gap is a warning, not a failed or unverified Unit result.

`test.setup.skip {"suite":"integration","reason":"..."}` records an explicit current skip explanation; it cannot
hide a completed/pending Integration result. `test.setup.accept {"reason":"..."}` accepts a current Unit receipt
only when the core-recorded actor and effective principal both equal the current workspace owner. Acceptance
records its reason, Unit receipt digest, setup identity and generation, and expires on the next observed change.
Failures remain displayed as failures even when accepted. Integration/Performance receipts are likewise stamped
with identity/generation/instance and closure; their measurement/verdict semantics are unchanged.

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
External launch paths stay off and unverified unless explicitly allowed with `--allow-external`.
Dependencies blocked only because the fake-backend audit disabled their providers are unverified and listed
as not covered, including in candidate comparisons. At least one provider must have been enabled before that
audit; genuinely missing/incompatible providers, or requirements with only externally blocked or already-disabled
providers, still fail when unmet. This does not claim real-backend integration coverage. Credential-like
workspace entries and contributed documents are excluded and listed; the extension tree is copied whole,
with symlinks preserved rather than dereferenced. Protected documents refused by the private core are reported
as not copied. This is
cooperative isolation, not a security sandbox: only run extension code and scenarios you trust. Agent profiles
are not copied yet. Use `--self-test` for crashing-extension and zero-coverage negative controls.

`--root DIR` must be absent or empty and short enough for sockets under 100 bytes. Copies are removed unless
`--keep`; CLI receipts are written only with `--out` (choose a path outside the disposable root). The report
records the source declarations digest, generations and seq before and after. Only declaration or generation
changes fail the setup-consistency check: results would no longer describe one setup. Seq movement from normal
client/agent activity is informational, and unavailable consistency proof does not change the verdict. A changed
setup is not attributed to the harness without evidence. Exit codes remain 0 pass, 1 fail, 77 unverified, 2 harness/canary error.

### Repeated performance measurements

`vqro run test.perf` starts an asynchronous subprocess; `test.perf.show` opens its completed report.
The CLI mirrors integration flags and adds `--repeats N` (1–50, default 5) and `--window S` (1–120 seconds,
default 10). Each repeat uses a fresh private copy, serially, with a two-second settle before its idle window.

```sh
python3 ~/.local/share/vqro/extensions/testing_perf.py --json --repeats 5 --window 10 --out perf.json
python3 ~/.local/share/vqro/extensions/testing_perf.py --candidate-ext /path/to/overlay --out comparison.json
```

The report separates core/extension time-to-ready, idle event/content rates, attributed process CPU/RSS,
scenario invocation reply/content latency, and peak published item/total sizes. Extension time-to-ready starts
when the harness leaves safe mode and ends after observing running status plus registered commands. Local
monotonic receive times include event delivery latency; they are not process exec times. CPU/RSS use only
harness-owned processes, and unavailable attribution/platform facts are unverified, never fabricated zeros.

Every scope retains raw samples, count, median, minimum and maximum. Missing/non-finite samples fail.
Limits are **configurable defaults and breaches only warn**, including the default quiet-window ceiling of
0.5 events/s. Relative slowdowns beyond a noise floor warn only outside the baseline spread; overlapping
spreads are unverified. Warnings never change a pass verdict. The first report line counts performance warnings;
exit codes are 0 pass (including warnings), 1 broken measurement, 77 unverified, 2 harness/canary/usage error.

`doc/settings/test-perf` overrides known defaults, including nested `absolute` ceilings; null disables a ceiling.
Receipts record effective values, override names, ignored unknown keys, and whether settings or defaults were
used. Malformed settings warn and fall back to defaults. Relative timing defaults are 25% with a 5 ms floor;
RSS uses 25% and 10 MiB, size growth uses 2×. Absolute defaults are 170 ms interaction latency, 40% idle CPU,
90 MiB RSS, 1 MiB/item and 8 MiB total content; startup has no default ceiling. Busy-machine load is recorded
and warns, without pretending that one noisy sample proves a regression. Functional correctness, rendering
and real agent profiles remain outside this report; use integration checks for functional failures.

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
