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
- There is no in-app log viewer yet. The detail reports the core-host log path for external inspection.

Wide windows put details beside the list; narrow windows put them below. Settings uses the generic `list_detail`
panel enhancement in `spec/core-protocol.md`; other extensions can use exactly the same primitive.

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
