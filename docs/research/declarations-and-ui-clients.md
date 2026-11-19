# One declaration model: extension dependencies and client UI features — 2026-11-19

Status: design note, not yet a decision. Builds on `<private-notes>
ui-architecture/report.md` (UI clients research) and the human's decisions of 2026-11-19: one "declare what you
provide / need, with fallbacks" model for both extension dependencies and client UI features; depend on what is
provided, not on who; turning on offers to enable missing needs (one confirmation); shared data is a declared
dependency; anyone may turn off a depended-on extension after a warning (D24: there is no grant system yet, so
"anyone with the grant" is everyone today); vqro reports and warns, never enables or disables by itself.

## 1. Capabilities: one vocabulary

A **capability** is a string `kind/name[@major]` (major defaults to 1; only majors, no ranges):

| Kind | Example | Provided by | Meaning |
|---|---|---|---|
| `command` | `command/tabs.show` | extension manifest | a command name (already owner-prefixed) |
| `view` | `view/settings`, `view/agent-*` | extension manifest | a view entry or a named view family (trailing `*`) |
| `data` | `data/themes`, `data/workspaces` | extension manifest | live data published under that name (`view.publish`) |
| `doc` | `doc/workspaces/list` | extension manifest | a workspace entry whose schema the provider owns |
| `ability` | `ability/vqro.terminals`, `ability/vqro.agents`, `ability/vqro.resource.forget` | extension manifest (providers) | something a running program can do |
| `ui` | `ui/vqro.list_detail`, `ui/x.acme.table@1` | client `hello` | a client UI feature or profile |

- `vqro.*` names are defined in `docs/spec/core-protocol.md`; anything else is namespaced by its owner
  (`<extension>.*` for commands, data and views; `x.<owner>.*` for third-party abilities and UI features).
- The **core provides** its built-ins: `command/vqro.*` and the spec-defined shared settings documents
  (`doc/settings/tui`, `doc/settings/theme`, `doc/settings/icons`, `doc/keys/profile`). Readers of those always have
  a spec fallback (built-in palette, `C-g`, no icon set), so reading them is never a dependency on whichever
  extension writes them. An extension-owned document (for example `doc/workspaces/list`) is provided by its owner.
- Several providers of one capability are alternatives (herdr and tmux both provide `ability/vqro.terminals`).

## 2. Manifests: `provides`, `needs`, `works_with`

```json
"provides": ["command/tabs.open", "command/tabs.show", "command/tabs.close", "command/tabs.rename"],
"needs": [{"cap": "command/tabs.show", "suggest": "tabs", "why": "opens agent panels in a tab"}],
"works_with": [{"cap": "data/themes", "suggest": "themes", "without": "The Theme page is empty"}]
```

- `needs`: without it the extension's main purpose fails. `works_with`: optional; `without` says what is lost.
  A bare string is shorthand for `{cap}`. `suggest` is a provider name to offer for enabling or installing; it
  is a hint, never a binding (any provider of the capability satisfies it).
- Existing `requires.client_features: ["items", ...]` stays valid and means `needs: ["ui/vqro.items", ...]`.
- Everything stays **static data** (D15 manifest rule: never run a program to learn what it does). The manifest is
  descriptive; the actual command registry and live data remain authoritative at run time.

## 3. Resolution: the core reports, never acts

The manifest catalogue (`src/manifest.rs`) resolves every declaration's needs against all declarations' manifests,
their runtime status and core built-ins. `extension.list` and snapshot `extensions` gain:

```json
"dependencies": {
  "status": "ok" | "degraded" | "missing",
  "needs": [{"cap": "command/tabs.show", "status": "satisfied" | "off" | "missing" | "version",
             "providers": [{"name": "tabs", "display_status": "off"}], "suggest": "tabs"}],
  "works_with": [ ...same shape... ],
  "used_by": [{"name": "agents", "cap": "command/tabs.show", "hard": true, "enabled": true}]
}
```

- `satisfied`: a provider is running (or the core provides it). `off`: providers are declared but none runs
  (disabled, failed, stopped by safe mode). `missing`: no declared provider; show `suggest`. `version`: only another
  major is provided.
- Per extension: `missing` if any need is unmet, `degraded` if only `works_with` items are unmet, else `ok`.
- `ui/*` needs resolve against **connected clients** (from `hello.features`): reported as satisfied or "lacking
  in: <client>"; never `missing`, because content must carry its own fallback (section 5).
- `vqro.extension.enable|disable` remain plain mechanism and never cascade. Their reply adds the facts the caller
  should show: `needs_unmet: [cap]` after enable, `breaks: [{name, cap}]` after disable. The CLI prints them.
- Dependents keep running when a need disappears; their calls fail as today (`not_found`), and their status shows
  `missing`. Resolution never gates recovery: safe mode, the chooser and built-in commands ignore it.

## 4. Settings > Extensions (an extension, D17)

- Detail: **Needs**, **Works with**, **Used by** and **Provides**, each row with its status (`ok`/`off`/`missing`).
  List row badge: `needs Tabs (off)` or `needs …(missing)`.
- **Turn on** with unmet needs whose providers are declared: one popup, "Agents needs Tabs (off). Turn on both?"
  with [Turn on both] (default), [Turn on Agents only], [Cancel]. Missing providers are listed with the suggested
  name ("not installed"); the extension can still be turned on.
- **Turn off** an extension that enabled extensions need: "Turning off Tabs breaks: Agents (opening agent panels),
  Pi (showing new agents). Turn off anyway?" [Turn off] [Cancel]. `works_with` users are listed as "loses: …".
- Both flows use existing primitives: `menu`-style popup rows and `vqro.extension.enable|disable` per extension.
  In safe mode the same offer uses `vqro.safe_mode.allow` session choices. The built-in Rust recovery chooser shows
  the `needs …` text only (no offer flow), so recovery depends on no resolution logic.

## 5. Clients declare UI features and profiles

`hello {client, version, profiles: ["base", "terminal", "keys"], features: ["vqro.list_detail", ...]}`, exported in
`client.list`; older cores ignore the fields. Today's `SUPPORTED` list (`src/tui/panel.rs`) becomes `vqro.*`
feature ids; bare names in panel `requires` remain aliases.

- **`base`** (every interactive client, including recovery): snapshot/event cache with resync; layout tree (splits,
  tabs); `panel` views with base rows (item, toggle, header, separator, text), menus, prompts and pick-one
  options; command invocation with `args.context` and reply hints (`popup`, `close`, `focus`); `ui.notify`;
  semantic styles over a built-in palette; unknown rows as plain items; an unsupported view kind shows its
  `fallback` view or a labelled placeholder; **recovery**: badge, prefix with `C-g` fallback, the shared recovery
  content (prefix recovery rows and the safe-mode chooser), themes and icon sets ignored in safe mode.
- **`terminal`**: attach, credit, repaint-on-gap, ordered input and paste rules, xterm keys plus kitty
  "disambiguate", provider-owned resize, no query responses unless declared.
- **`keys`**: prefix, binding precedence, `client.*` commands.
- **Optional features** are namespaced and need a **mandatory fallback** in content: a row or component may carry
  `fallback: [row] | "drop"`; a view of a non-base kind may carry `fallback: <view id>`; enhancement requirements are
  scoped like `list_detail`. Putting a non-base feature in a panel's outer `requires` stays legal but is linted.
- **Recovery content is one shared definition** in the renderer-independent client model, with its expected panel
  JSON committed as conformance fixtures so non-Rust clients implement against the fixtures.
- `vqro ui [<client>]` picks a client (default the built-in ratatui client; `vqro tui` stays as an alias). The
  ratatui client always ships in the binary as the rescue client.

## 6. Graceful degradation and third parties

- Missing need: the extension still runs; Settings and `extension.list` say what is missing and who could
  provide it. Missing `works_with`: the extension shows its `without` text where the feature would be.
- Missing UI feature: the client renders the fallback; `client.list` lets extensions and Settings say
  "2 connected clients lack `x.acme.table`".
- A third party adds a capability by naming it in its own namespace, listing it in `provides` (optionally
  documenting it in its manifest or docs), and others list it in `needs`/`works_with` with `suggest`. A UI
  component additionally ships its base fallback; native renderers per client come later and are optional.
- Per-window settings and per-window active workspace (planned) fit as `doc/settings/tui` sub-keys or client-local
  state; nothing here assumes one window.

## 7. Release-check lint

`scripts/check_manifest_deps.py` (stdlib, in `scripts/check.sh quick`): statically scans each bundled extension for
command names it invokes or puts in rows/menus/bindings, live data it reads (`ext.live.get`), views and documents of
other owners, maps each to its provider through `provides`, and fails when a cross-extension use is not declared
in `needs` or `works_with`, or a `provides` entry is never registered/published by the source.

## 8. Bundled map (draft, verified in step 2)

agents needs `command/tabs.show`; pi needs `ability/vqro.terminals`, works with `command/tabs.show`,
`command/agents.show`, provides `ability/vqro.agents` and `view/agent-*` (through its bridge); sidebar needs
`command/tabs.show` (its Show action) and works with `data/workspaces`,
`command/workspaces.*`, `command/agents.config`; workspaces needs `command/tabs.open`; settings works with
`data/themes`, `data/icons`, `data/keys` and their apply commands; keys and gestures work with the `tabs.*`/`panes.*`
commands they bind; retention needs `ability/vqro.resource.forget`, works with `view/agent-*`; herdr and tmux provide
`ability/vqro.terminals` and `ability/vqro.resource.forget`; tabs and panes work with `ability/vqro.terminals`.

## Open decisions for the human

1. **Granularity of command dependencies.** (a) per command, `command/tabs.show` (recommended: names already exist,
   the lint can check them exactly); (b) per group, `ability/tabs`.
2. **Missing provider on turn-on.** (a) allow turning on with a warning (recommended: vqro reports, never blocks);
   (b) refuse until the provider exists.
3. **Safe-mode chooser.** (a) built-in chooser shows `needs …` text only, Settings has the offer (recommended:
   recovery code stays minimal); (b) the built-in chooser also offers to allow needs.
