# prim: architecture

Status: revised 2026-11-16. The shape below is the intended design; **much of it is now built as a working
prototype, and fluid** (see "What exists now"). The exact, current protocol is `spec/core-protocol.md`; open items
are in `open-questions.md`.

prim is one lean, standardized layer of workspace primitives over any multiplexer and any agent runtime. Humans and
agents use the same primitives to build and change the workspace; coordination (messages, interrupts, routing) is
workspace behaviour like layout or commands. Backends own execution (D14).

## Layers

```
┌──────────────────────────────────────────────────────────────┐
│ Clients: render the workspace (TUI first; others later)       │
├──────────────────────────────────────────────────────────────┤
│ Extensions (processes): layouts, views, commands, bindings,   │
│ routing, mailboxes, delivery strategies, notifications ...    │
├──────────────────────────────────────────────────────────────┤
│ prim core: the primitives · facts + change log · effects      │
├───────────────────────────────┬──────────────────────────────┤
│ Backend adapters (thin)       │ Agent adapters / bridges      │
│ herdr-core, tmux, RMUX, Rex,  │ pi RPC, Claude Code, Codex,   │
│ Zellij ...                    │ ACP agents                    │
├───────────────────────────────┴──────────────────────────────┤
│ Backends: processes, PTYs, persistence, remote (not prim)     │
└──────────────────────────────────────────────────────────────┘
```

Everything above the backends talks the prim core protocol. The core is a small, restartable process; it owns no
child processes.

## What exists now (prototype, 2026-11-16)

| Area | Built | Where |
|---|---|---|
| Core | JSON-RPC over a local socket; workspace store (validated transactions, history, revert, persisted); events with seq and `missed`; commands routed to owning connections; live view content (`view.publish`, not persisted); client tracking | `src/core.rs`, `src/server.rs`, `src/store.rs` |
| Adapters | herdr-core via `hc connect` (framed attach streams, OSC 7501 status, cwd); a fake backend fixture. **In-process**, behind channels | `src/adapter/` |
| Extensions | supervisor with crash budget, live replacement, safe mode; first-run defaults; contributions (`menu/*`, `binding/*`) with `owner` | `src/ext.rs`, `extensions/` |
| TUI client | layouts (tabs, splits, drag resize, zoom), terminal views (vt100), panels (pages, trees, toolbar, chips, select, badges, icons, filter), menus, popups, prompt line, mouse, themes (palette tokens), keys (rebindable prefix, priority, profiles), self-restart on reinstall | `src/tui/` |
| Bundled extensions | tabs, panes, gestures, workspaces, sidebar, themes, keys, settings | `extensions/*.py` |

Not built: headless agent bridge and agent views (agents run as terminals; status via OSC 7501), out-of-process
adapters, a second backend, remote machines, Windows. Order: `roadmap.md`.

## The primitives

| Primitive | What it is |
|---|---|
| **Node** | an addressable thing: a `terminal` (from a backend) or an `agent` (from an agent adapter) |
| **View** | something renderable: a terminal view (via VT), an agent view (from agent facts), an extension view |
| **Layout** | a versioned document placing views in a tree of containers |
| **Command** | a named, typed action registered by core, an adapter or an extension |
| **Binding** | input in a context mapped to a command (context keys, like VS Code `when`) |
| **Input / message** | input addressed to a node: keys and bytes to a terminal, or a message to an agent using the agent's native capabilities |
| **State** | namespaced documents and facts, one writer per namespace |
| **Event** | every fact change, in a cursored log: read from a cursor to poll, stream from it for push |
| **Extension** | a declared process that registers views, commands and bindings and owns namespaces |

Spec: `spec/workspace-v0.md`. These are the standard; everything else is an adapter or an extension.

## Capabilities everywhere

prim never assumes what a backend or runtime can do. Adapters declare capabilities, the core exposes them, and
extensions and clients use what is there.

- **Backend capabilities**: `spec/backend-adapter-v0.md` (spawn, input, raw output, OSC 7501 status, labels, docs,
  native layout, detach persistence, remote machines, event resync).
- **Agent capabilities** (D4):

| Capability | pi RPC | Claude stream-json | Codex app-server | ACP v1 |
|---|---|---|---|---|
| `prompt` (idle) | `prompt` | user message | `turn/start` | `session/prompt` |
| `inject_at_boundary` | `steer` | unverified (E1) | `turn/steer` | no (ext. only) |
| `enqueue_after_settle` | `follow_up` | queued message | no | no |
| `interrupt` | `abort` | interrupt | `turn/interrupt` | `session/cancel` |
| `answer_request` | extension UI only | `--permission-prompts host` | `requestApproval` reply | `request_permission` |
| `fork` / `resume` | yes | `--fork-session` / `--resume` | `thread/fork` / `resume` | resume; fork unstable |

Sources: `research/agents.md`; Claude flags from local `claude --help` (2.1.325). Claude Code also has a native
cross-session inbox socket (delivered / held / refused; `research/prior-art-tools.md`), a candidate route for E1.

## Agents

An agent node is either:
- **headless**, through an agent bridge running in an ordinary backend terminal (`spec/backend-adapter-v0.md`); prim
  renders its UI from structured events; or
- **PTY-backed**, the runtime's native TUI in a backend terminal; prim sees bytes and OSC 7501 status.

Switching means ending one and resuming the session in the other; resume restores history, not a turn in progress (D11).

## Facts, log, effects

- Facts are observed, not authoritative (D6). The log is bounded; a reader that falls behind gets `missed`.
- Effects report what the backend or runtime said happened: `dispatched`, `accepted`, `observed`, or
  `indeterminate` (D10). prim adds no guarantees beyond what the layer below provides.

## Clients and UI

A client renders workspace primitives; it holds no workspace behaviour. TUI first (ratatui, D7). Terminal views use
an embedded VT on the backend's raw output stream. Extension views: declarative, terminal program, hybrid, or a mix
(**Open**, test S3).

## Core protocol rules (candidate, `research/protocols-and-ui.md`)

Generated schema with a CI compatibility gate; capabilities instead of version branching; unknown fields ignored;
version and caller on every request; no hidden session state; side effects broadcast with seq and `missed`;
reverse-DNS extension namespaces; stable error kinds.

## Trust (D8)

Cooperative authority; user-only local sockets with short paths; every effect records its caller; answering an agent's
permission request is reserved for humans unless explicitly granted.
