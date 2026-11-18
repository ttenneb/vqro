# vqro workspace primitives, v0 (sketch)

> **Status (2026-11-16):** **Superseded by `core-protocol.md`**, which documents what was built: views and layouts, commands, bindings and gestures, menus, panels (pages, trees, toolbars, chips, icons), popups, live content, transactions with revert, the extension lifecycle (crash budget, safe mode). Rendering was decided as declarative panels plus terminal views; the hybrid is untested. Leftover questions moved to `../open-questions.md`. vqro is a working prototype, and fluid.

Status: sketch, to be shaped by the tests in `../test-plan.md`. This is the standard surface (D14) through which humans **and agents** build and change the
workspace. Success criterion: *an agent can safely build and change meaningful workspace behaviour using only
these primitives, with no host modification.*

## Primitives

| Primitive | What it is | Owned by |
|---|---|---|
| **View** | Something renderable with an ID: `terminal` (node via VT), `agent` (from agent facts), `ext` (from an extension) | the core for built-ins; the extension for `ext` |
| **Layout** | A versioned document: a tree of containers (split, tabs, stack, float, collapsible) holding view IDs | whoever holds the layout's write grant |
| **Command** | A named action with a typed argument schema, registered by core or an extension | its registrant |
| **Binding** | Maps input (keys, mouse, text) in a context to a command | a namespace (user, extension) |
| **Focus** | Which view receives input; a fact | the client, by request |
| **State** | Namespaced documents (compare-and-set) and facts | one writer per namespace |
| **Extension** | A declared process with a manifest: what it registers, which namespaces it writes | the user |

## Extension lifecycle

- `load(manifest)`, `replace(name, new_version)`, `rollback(name)`, `unload(name)`.
- Replacing or crashing an extension MUST NOT touch nodes or processes. Views it owned show a placeholder; layouts
  keep their structure.
- The previous version stays available for `rollback`. A failed health check after `replace` rolls back automatically
  (that default is a parameter, not hidden policy).
- Each extension's failure is isolated to its own namespaces (vqro: "Shared runtimes need explicit isolation design").

## How extension views render (**Open**, E0 decides)

1. **Declarative tree**: the extension publishes a widget tree (text, list, button, input, embed-view) as a document;
   clients render it. Portable across TUI and GPU clients, limited expressiveness.
2. **In-terminal program**: the extension runs as a terminal node; the view is that terminal. Unlimited expressiveness,
   no structure, client-agnostic only as far as a VT is.
3. **Hybrid**: an in-terminal program that emits semantic widgets inside its byte stream, with a cell fallback for
   clients that don't understand them (Zellij does this, `research/workspace-extension-models.md`).
4. **A mix**, chosen per view. Test S3 in `test-plan.md` decides.

## Agents as authors

An agent changes the workspace by calling commands and writing documents through the same core protocol as any
extension, under a grant. Changes are recorded with their author and are reversible (layout and document history,
extension rollback).

## Known problems (from `research/workspace-extension-models.md`)

- If an extension view is a terminal node, replacing the extension ends a process; needs a rule for extension-owned nodes.
- Rollback restores code, not state: namespaces need schema versions and a snapshot at `replace`; a load-time health
  check misses late crashes, so use a crash budget.
- One layout, clients of different sizes: pick smallest-client, owner-sets-size, or per-client rendering.
- Process per extension: measure the cost.
- Contextual bindings: context keys (VS Code `when`).

## Open

Grant model granularity; how clients negotiate unsupported view kinds; theme and styling; whether bindings can be
contextual on node state (e.g. "when the agent is blocked").
