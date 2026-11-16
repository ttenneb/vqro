# Adversarial review of the plan (2026-11-16, second review)

Reviewer: fresh subagent, given the plan and pointed at backends.md (then research.md), review-1-backends.md, agents.md and vqro docs.
Evidence references are the reviewer's; REASONED = argument only.

## Findings (ranked)

1. **Four products at once** (supervisor, agent protocol, UI framework, VT host). vqro's own lesson:
   "a product consumer should precede new infrastructure" (`~/Projects/vqro/docs/next/architecture/lessons-and-motivations.md:21`).
   Fix: slice = one headless agent over pipes + one log + one ratatui view. No backend, VT or chrome docs.
2. **Backends lose their main job once agents are headless.** Agents need pipes (agents.md), emit no 7501, so
   backends give agents neither I/O nor status; only supervision, persistence, remote (herdr-core only).
   Fix: prim supervises agents itself; backends host *terminal* nodes; herdr-core optional for remote.
3. **prim-host repeats a retired vqro pattern** (external supervisor = second wake model; sockets as lifecycle
   liabilities, `lessons:44-45`). Adds a process per agent, reconnect protocol, unauthenticated socket, durable buffer.
   Fix: only if remote needs it; spike reconnect (kill prim mid-turn, no lost events, no replayed prompt).
4. **boundary/settle/preempt are not neutral and mostly not native.** ACP v1 has none; Zed says steering works only
   for its own agent "since Zed can't detect turn boundaries for external agents"
   (https://zed.dev/docs/ai/parallel-agents.html). Emulation needs a prim-side queue = policy.
   Fix: core exposes adapter-native capabilities only; extensions name modes.
5. **ACP first risks vqro's coupling mistake** (crate 3.6→5.3 in a month, v2 draft). Fix: pi RPC or Codex
   app-server first (native steer + approvals), ACP second; two agents keep the contract honest (`lessons:51`).
6. **Runtime-only state** (compaction, in-agent approvals, config, resume fidelity). Label prim facts "observed",
   not authoritative (vqro: "volatile events are not state", `lessons:20`).
7. **No-policy-in-core breaks on log retention, backpressure, ordering.** REASONED. Fix: name defaults as mechanism
   parameters now: bounded log, `missed` resync like hc, per-node ordering only.
8. **Trust:** an agent with prim access can answer another's permission prompt. Same-user authority is cooperative,
   not a boundary (`lessons:49`). Don't route `answer(request)` from agents by default.
9. **Owning agent UI = rebuilding native TUI features** (diffs, slash commands, extension UI, auth), reversing vqro's
   "PTYs are canonical". Fix: keep a raw-PTY mode per agent.
10. **Prior art:** Zed parallel ACP + terminal threads (https://zed.dev/blog/terminal-threads); OpenCode one server,
    many clients (https://opencode.ai/docs/server/). prim's real difference: cross-runtime input router + own UI.

Sound: separate `prim` doc (not `herdr-app`); "design for swap, build for one" (wrong "one" chosen).

## Assumptions most likely wrong
1. Backends are needed for agents once headless.
2. Delivery modes are neutral cross-runtime primitives.
3. ACP is stable enough as first coupling point.

## Cheapest experiment (~1 day)
~300-line Rust binary: spawn `pi --mode rpc` and Codex app-server, append both event streams to one JSONL log,
stdin commands steer / follow_up / interrupt. Script: steer mid-tool-call, queue while working, interrupt + re-prompt;
record when each input took effect. Then kill prim mid-turn and see what is recoverable.
