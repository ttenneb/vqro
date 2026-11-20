# vqro: architecture

Status: **implemented working prototype, and fluid**, revised 2026-11-20. This replaces the 2026-11-16
in-process-adapter architecture. The living contracts are `spec/core-protocol.md` and the implemented wire section
9 of `spec/provider-v0.md`. Earlier sketches are historical, not alternative APIs.

vqro is one lean, standardized layer of workspace primitives over backends and agent runtimes (D14). Humans and
agents use the same public operations. Coordination is workspace behavior composed from those primitives, not a
second authority system. No contract is called stable before two implementations exercise it.

## Execution and ownership

```text
Clients (built-in Rust TUI; Python text; optional OpenTUI trial)
  render terminal streams and declarative panels; own local focus/selection/scroll
                    |
vqro core: transactions/history/undo, commands/bindings, resource identities,
  provider registry, bounded events/live content, capability routing and effect outcomes
          |                                      |
supervised extension processes           external providers over core protocol
  tabs, panes, settings, sidebar, ...       vqro-herdr, vqro-tmux, runtime bridges
                                                 |
                                backends own terminals/programs/persistence
                                runtime bridges use backend-owned execution
```

The core **does supervise its own extensions** (D15): process start/stop, lifetime channel, crash budget and
replacement on declaration changes. It does **not** host workspace terminals, agent processes or their lifetime
guarantees. A core/provider restart reconnects to facts; it never replays agent input or invents a backend's
capabilities. Explicit in-process adapters remain for tests/embedders, not the normal `vqro serve` architecture.

## What exists now

| Area | Implemented | Main code |
|---|---|---|
| Core transport | Owner-only opaque local endpoint, bidirectional JSON-RPC, bounded framing/queues/pending calls, protocol shutdown | `src/rpc.rs`, `src/server.rs`, `src/shutdown.rs` |
| Workspace | Validated durable candidate-first transactions, history/revert, D24 original-invoker attribution, serialized bounded commit admission | `src/store.rs`, `src/commit.rs`, `src/invocation.rs` |
| Providers | External backend/runtime registration, generation/source fencing, inventory chunks, capabilities, forwarded effects and credited streams | `src/provider.rs`, `src/provider_wire.rs`, `src/provider_client.rs` |
| Identity/facts | Indexed active identity map with durable monotonic id counter; batched inventory replacement; observed availability and explicit retirement | `src/provider.rs`, `src/core.rs`, `src/resource_admission.rs` |
| Backend implementations | `vqro-herdr` over hc framing; `vqro-tmux` over its separate native server; fake backend/provider fixtures | `src/bin/vqro-herdr.rs`, `src/bin/vqro-tmux.rs`, `src/adapter/`, `src/tmux/` |
| Structured agent integrations | Backend-owned Rust Pi RPC bridge and Codex app-server adapter; declared native capability subsets and shared item streams; draft contracts remain unqualified | `src/bin/vqro-bridge-pi/`, `src/bin/vqro-bridge-codex.rs`, `src/provider_client/`, `src/bridge_tokens.rs` |
| Terminal-agent integration | Claude Code hooks/native admission and Pi pull plugin; per-launch tier-1 wiring without changing the user's runtime configuration | `src/bin/vqro-claude-hook.rs`, `integrations/pi/`, `extensions/tier1.py`, `extensions/tier1_run.py` |
| Coordination policy | Default replaceable `coord` extension: inbox, grants, pause/resume, handover and retained evidence over generic protected operational state; draft contract remains unqualified | `extensions/coordination*.py`, `src/operational.rs` |
| Extensions | Supervised declarations, static manifests, versioned defaults, live replacement/crash diagnostics and empty-start safe-mode recovery | `src/ext.rs`, `src/manifest.rs`, `extensions/` |
| TUI | Terminal VT rendering, layouts/tabs/splits, generic panels/list-detail, menus/popups, mouse, themes/icons/keymaps, recovery prefix and reinstall re-exec | `src/tui/` |
| Workspace opinions | Tabs/panes/gestures/workspaces/sidebar, theme/icon/key catalogues, Settings, capability-driven agent controls, Pi launcher, tmux server controls | `extensions/*.py`, `extensions/manifests/` |

The native tmux server's lifetime is independent of its provider/core; explicit confirmed controls stop it. It is
not the human's default tmux server. Herdr-core likewise owns its terminals. Backends declare exactly what they
natively support; raw input, exit codes, history/repaint and capability parameters are not emulated.

## State and observation

- Workspace entries are authoritative vqro-owned configuration/layout. Changes go through transactions, with
  validation, persistence and undo. User-triggered extension writes carry actual writer plus proven original
  invoker (D24); autonomous extension updates remain outside default user undo.
- Resource facts are backend observations. Identity is `(provider, instance, ref, incarnation)`. A durable
  monotonic `next` prevents id reuse after retirement or restart; retired records themselves are discarded.
  Views/layouts retain their ids and are never silently rebound to a new execution lifetime.
- An inventory/sync chunk admits its facts, writes one identity candidate, then publishes. A failed write exposes
  no new id; a replacement/closure observation can still fence old routes. Rename-with-unconfirmed-durability is
  adopted and reported, not replayed. Credential expiry follows the same retirement batch.
- Live view content is derived, bounded and not in history. Per-view charges and the total are cached; a publish
  does not re-encode every other live view. Different live authors cannot replace/clear each other's publisher;
  replacement connections of the same verified extension retain their namespace.
- Events use `(epoch, seq)`, count/byte-bounded replay and explicit `missed` recovery. Idle subscribers terminate
  on peer close without needing another event. Clients resnapshot after gaps rather than synthesizing history.

The provider registry holds up to 1024 names under an aggregate registration-metadata budget. Disconnected runtime
records leave when no active identities, retained resources or pending facts need them. Source tokens still fence
late old connections; host-bound resume credentials are independent of a registry record. Backends/disconnected
runtime resources remain visible while their facts are still relevant. See the protocol for exact limits/events.

## Commands, UI and recovery

The core/client keep mechanism and recovery; opinions live in extensions using public primitives (D0/D17).
Commands are connection-owned, extensions contribute menus/bindings/views, and clients render terminal or generic
panel data. Settings' searchable list/detail is the same optional capability another extension can use; manifests
are static descriptive data, not an installer, permission system or dependency solver (`extensions.md`).

Safe mode starts no extensions/providers automatically. The built-in chooser gives explicit session-only
allowances; normal enabled declarations are unchanged. The vqro badge/prefix still reaches commands, undo, reset,
safe mode and quit with every extension broken. Bad key/theme settings fall back to built-ins. Core shutdown stops
extension/provider links in order, not backend-owned terminal execution (D22).

## Trust, portability and what is not built

Trust is cooperative, not a security sandbox (D8/D24). Agents may change anything users can. Managed lifetime
credentials reserve `ext:<name>` authorship; unmanaged connections have connection-scoped authors. D24 callback
proofs bind original-invoker attribution to the receiving live command, not to caller-supplied labels. These are
collision/attribution mechanisms, not a new grant system.

The endpoint contract is platform-neutral and opaque (D12/D22); Unix transport ships today, native Windows
transport remains unimplemented. Runtime/provider execution placement and any future remote reachability must
stay backend-neutral (D23): the rejected herdr document relay is not an architectural dependency. Pi, Codex and
Claude Code integrations exist, but implementation and real-runtime smokes are not whole-contract qualification:
`spec/agent-v0.md` and `spec/coordination-v0.md` remain drafts, partially implemented and unqualified. Remote-machine
UX, a registry/installer and broader view vocabulary remain roadmap/research work, not shipped guarantees.
`roadmap.md` owns their order.
