# vqro backend adapter contract, v0 (draft)

> **Historical draft; superseded as a public adapter contract (2026-11-17).** External providers are implemented:
> use `provider-v0.md` section9 and `core-protocol.md`, not the illustrative messages below. Herdr-core, tmux and
> the Pi runtime bridge are shipped implementations; in-process channels remain test/embedder machinery.
> This document is retained only as a capability/research catalogue; it is not the current wire schema.

Status: draft (D14). A backend adapter maps one multiplexer onto vqro's primitives and **declares** what that backend
can do. vqro exposes the declared capabilities and never compensates for missing ones. Adding a backend should take
days, not weeks: if a backend needs vqro to build something, the capability is simply absent.

## Shape

An adapter is a process (any language) speaking the vqro core protocol. On connect it sends a manifest:

```json
{
  "backend": "herdr-core", "backend_version": "0.1.21", "adapter_version": "0.1.0",
  "capabilities": { "terminal.spawn": {}, "terminal.output": {"raw": true}, "status.osc7501": {}, "...": {} }
}
```

Capability names are reverse-DNS-free in core (`terminal.spawn`) and namespaced for backend-specific extras
(`dev.herdr.machines`). Absent means unsupported. Unknown capabilities are ignored by the core.

## Core capabilities

| Capability | Meaning |
|---|---|
| `terminal.spawn` | start a program in a new terminal (argv, cwd, env, size, placement hint) |
| `terminal.input` | send bytes or keys to a terminal |
| `terminal.output` | stream raw output (`raw: true`) and/or read the rendered screen (`screen: true`) |
| `terminal.resize`, `terminal.kill`, `terminal.list` | |
| `terminal.exit` | report exit (and code, if known) |
| `status.osc7501` | report agent status from OSC 7501, parsed by the backend or the adapter |
| `meta.labels` | key/value metadata on terminals |
| `store.docs` | versioned documents (could hold vqro layout/state) |
| `layout.native` | the backend has its own layout the adapter can read or drive |
| `persist.detach` | terminals keep running when no vqro client is attached |
| `remote.machines` | terminals on other machines |
| `events.resync` | the event stream can report missed events |

## What each backend offers (from `research/backends.md`, review-1)

| Capability | herdr-core | tmux 3.8 | RMUX 0.10 | Rex (preview) | Zellij 0.46 |
|---|---|---|---|---|---|
| `terminal.spawn` / `input` / `kill` / `list` | yes | yes (control mode) | yes (control mode, SDK) | yes (CLI/API) | [U] (library/plugin API) |
| `terminal.output` raw | attach stream | `%output` | `%output` | `data.connect` [U] | [U] |
| `status.osc7501` | yes (store + events) | adapter parses `%output`; lost under load | as tmux | yes (events) | yes, since 0.46 [U] |
| `meta.labels` | key=value | `@options` | pane options | single string | [U] |
| `store.docs` | yes, CAS | no | no | no | no |
| `layout.native` | no | yes | yes | yes | yes |
| `persist.detach` | yes | yes | yes | yes | yes |
| `remote.machines` | yes | no | no | Tailscale | no |
| `events.resync` | yes (`missed`) | no | no | no | [U] |

[U] = not yet checked. Zellij capabilities come from its changelog only (`research/workspace-extension-models.md`).

## Rules

- **Declare, don't emulate.** An adapter may translate (control-mode text to events) but must not invent behaviour the
  backend lacks (no polling loop pretending to be events, no buffering pretending to be persistence).
- **Report honestly.** If a backend can lose events (tmux under load), the capability says so (`lossy: true`), and the
  core surfaces gaps rather than hiding them.
- **Thin.** Backend policy stays in the backend; adapters don't add waits, retries or delivery rules.
- **vqro's own state** (layouts, extension state) lives in vqro's store, or in `store.docs` when the backend offers it,
  never in another client's document (`herdr-app`).

## Agent bridges (D11, D14)

A headless agent runs inside an ordinary backend terminal through a small **agent bridge**: the bridge is the terminal's
program, starts the runtime with clean pipes, connects to the vqro core as an agent adapter, and draws a minimal fallback
view in its own terminal. The backend still owns the process; vqro hosts nothing. PTY-backed agents (native TUI) need no
bridge. Proposed in `provider-v0.md` (section 6, D21): a local socket with a one-time token passed in the terminal's
environment, and for remote hosts a relay through the host's provider; not through the terminal itself.
