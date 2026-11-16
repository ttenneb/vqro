# Protocols: declarative UI, core protocol design, shared state (2026-11-16)

Source: subagent (web + herdr-core `schema/README.md`). [U] = unverified.

## Declarative / agent-generated UI

| | Shape | Actions back | Licence / adoption | Terminal? |
|---|---|---|---|---|
| **A2UI** ([spec](https://a2ui.org/specification/v0.9.1-a2ui/), [v1.0](https://a2ui.org/specification/v1.0-a2ui/)) | flat adjacency list with `root`, separate JSON-Pointer data model, catalogue IDs negotiated by renderer; basic catalogue of 18 components | `action{name, surfaceId, sourceComponentId, context}`; inputs stay local until an action | Apache-2.0, ~17.5k stars ([repo](https://github.com/google/A2UI)); renderers React, Lit, Angular, Flutter, SwiftUI, Compose; v1.0 changed the wire format | no official TUI renderer ([roadmap](https://a2ui.org/roadmap/)); third-party ratatui crate v0.3.1 ([crate](https://crates.io/crates/a2ui)) |
| **MCP Apps / MCP-UI** ([spec](https://raw.githubusercontent.com/modelcontextprotocol/ext-apps/main/specification/2026-01-26/apps.mdx)) | `ui://` HTML resource in sandboxed iframe | JSON-RPC over postMessage | Apache-2.0; official MCP extension; ChatGPT, Claude, VS Code, Goose | **no**, HTML only |
| **OpenAI Apps SDK** | MCP Apps plus `window.openai` [U] | as MCP Apps | proprietary [U] | no |
| **AG-UI** ([events](https://docs.ag-ui.com/concepts/events)) | typed event stream; state snapshots + RFC 6902 deltas; carries A2UI/MCP-UI as payload | new run with `resume` | MIT | transport only |
| **Vercel AI SDK** | tool parts rendered by React | chat request | Apache-2.0 [U] | no |
| **Jupyter widgets** ([messages](https://github.com/jupyter-widgets/ipywidgets/blob/main/packages/schema/messages.md)) | comm per widget; model/view module IDs; `update` with buffers | `update`, `custom`, `request_state`; `echo_update` to all frontends | BSD-3 | no standard TUI [U] |

**Recommendation:** no standard is adoptable as-is. Define a small prim catalogue (text, styled spans, list/table,
button, input, tabs, embed-view), borrowing A2UI's adjacency list, data model, catalogue IDs and action shape;
Jupyter's model/view split, `request_state`, `echo_update`; AG-UI's snapshot + JSON Patch deltas. Keep "both, per
view" with a terminal node as escape hatch. An A2UI bridge can be an extension later.

## Core protocol design rules (from LSP, DAP, MCP, Jupyter, Neovim, herdr-core)

1. Schema generated from Rust types, served at runtime; CI fails on breaking changes vs the released schema
   (herdr-core `schema/README.md`); `since` / `deprecated_since` per method (Neovim [api](https://neovim.io/doc/user/api/)).
2. Capabilities, not version branching; absence = unsupported; ignore unknown fields, enums, messages
   ([LSP](https://microsoft.github.io/language-server-protocol/specifications/lsp/3.17/specification/), [Jupyter](https://jupyter-client.readthedocs.io/en/latest/messaging.html), [DAP](https://microsoft.github.io/debug-adapter-protocol/specification)).
3. Breaks only in a deliberate new generation, with a deprecation window ([MCP versioning](https://modelcontextprotocol.io/specification/versioning)).
4. Version and caller on every request; no connection state other features depend on; explicit handles. MCP broke
   compatibility in 2026-07-28 to remove sessions ([MCP blog](https://blog.modelcontextprotocol.io/posts/2026-07-28-release-candidate/)).
5. Every request gets a response, even when cancelled; progress tokens for long work.
6. Side effects broadcast to all clients on one stream with parent correlation ID and monotonic seq; `missed` on lag.
7. Reverse-DNS extension namespaces, versioned independently; errors carry a stable `kind`.

## Shared state for layouts

| Option | Concurrency | Undo / rollback | Fit |
|---|---|---|---|
| CAS-versioned docs (herdr `doc.set`) | stale write fails with `conflict` | not built in; write back old value | simple, conflicts visible to agents |
| Yjs | CRDT | `UndoManager` by origin ([docs](https://docs.yjs.dev/api/undo-manager)) | Rust support weaker [U] |
| Automerge | CRDT; tree move status [U] | no native undo | weak for tree moves |
| Loro | CRDT with movable tree ([docs](https://www.loro.dev/docs/advanced/undo)) | per-peer undo | best CRDT candidate (Rust) |
| Event sourcing | ops appended with author, seq | revert any op | matches D0 change log |

Inference (subagent, uncited): layout edits are rare and structural; silent merges of concurrent tree moves are
unpredictable for humans. Prefer CAS snapshots + append-only op history with author; undo/rollback as forward writes.
Loro as fallback if conflicts prove frequent.

## Tests proposed

1. Render A2UI sample surfaces through a 7-component prim catalogue in ratatui; measure fallback rate.
2. Have Claude Code author 5 views as catalogue JSON and as terminal programs; compare success, tokens, updates, portability.
3. Concurrent layout: 1 human + 2 agents, 200 scripted ops, CAS vs Loro; conflict and retry rates, per-author undo.
4. Compatibility gate: OpenRPC for 3 methods; confirm a released-schema check catches a rename.
5. Fan-out: two TUI clients + one agent editing view state, one disconnects; convergence after `missed` + `request_state`.
