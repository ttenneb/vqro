# vqro: vision

Status: direction (revised 2026-11-16 around D14). Binding choices live in `decisions.md`. The implementation is a
working prototype, and fluid.

## The problem

Working with many AI agents at once is normal now, and the terminal is where they run. Every tool for it fuses three
things and hides the choice: where processes run (a multiplexer), how agents are driven (each runtime's own TUI and
protocol), and how work is coordinated (wait, notify, queue, route).

- Multiplexers bake coordination policy into their verbs: herdr-core's `send` waits for 3 s of quiet and `wait` means
  "not working" (`research/review-1-backends.md`, finding 3).
- Runtimes disagree on basic semantics such as what "steer" means (`research/agents.md`).
- Agents talk to each other by typing into terminals, which loses input (vqro lessons; `research/prior-art-tools.md`).
- Customisation exists only per tool: a Claude Code mod reshapes Claude Code, a cmux config reshapes cmux. Nothing spans
  the tools and multiplexers you actually use together.

## What vqro is

**One lean, standardized layer of workspace primitives over any multiplexer and any agent runtime.** Humans and
agents use the same primitives (nodes and views, layout, commands, bindings, input and messages, state, events,
extensions) to build and change their agent workspace. Coordination is not a separate system: messaging an agent,
queueing an interrupt, or routing a request is workspace behaviour built from the same primitives as a sidebar or a
keybinding.

vqro stays lean by not doing the hard execution work. Processes, PTYs, persistence, recovery and remote machines belong
to the multiplexer: herdr-core, Rex, tmux, RMUX, Zellij, whatever comes next. Those projects exist to solve that.
vqro adapts to each through a thin adapter that declares what the backend can do, and never compensates for what it
can't (D14).

## Principles

1. **Primitives are features.** If it can be built from primitives, it is an extension.
2. **No policy in core (D0).** Anything that decides *when* or *how* something is delivered, waited on or notified is an
   extension.
3. **Lean.** vqro hosts no processes and builds no guarantees the layer below doesn't provide.
4. **Standardized.** The primitives and the protocol are the product; they are documented so others can write
   adapters, extensions and clients.
5. **Quick to support anything.** Adding a backend or agent runtime is a thin adapter declaring capabilities. If that
   takes more than days, the standard is wrong.
6. **Capabilities, not assumptions.** Backends and runtimes say what they can do; vqro exposes exactly that.
7. **Honest reporting.** vqro reports what the layer below said happened (D10); restarts never resend input.
8. **Two of everything.** A contract is proven against two backends and two runtimes before it is called stable.
9. **Cooperative authority (D8).** Same-user processes can't be contained; permissions are a coordination protocol.
10. **The real process is one action away.** Agents can run headless or with their native TUI (D11).
11. **Portable contracts (D12).** No Unix-only concepts in the standard.

## Ambition

- An agent asked for "a review workspace for these three PRs" builds it from primitives, on whatever multiplexer you
  use that day, and you can roll it back.
- Dozens of agents from different runtimes, across the machines your backends reach, in one workspace.
- Any message (human, agent, scheduled) is addressed, visible, and routed by a policy you swap in minutes.
- Switching multiplexer or agent runtime means swapping an adapter, never rebuilding the workspace.
- Others build adapters, extensions and clients from the spec alone.

## Non-goals

- Not a multiplexer, process host or terminal emulator product.
- Not an agent runtime; vqro drives agents, it doesn't call models.
- Not a security sandbox.
- No built-in workflow; the default workspace ships as replaceable extensions.

## Honest risks

- **Lowest common denominator.** Backends differ a lot (`spec/backend-adapter-v0.md`). If capability declarations get
  too fine-grained, extensions become full of special cases. Mitigation: two backends early.
- **Owning the agent UI** means rebuilding diffs, slash commands and auth flows per runtime. Mitigation: native-TUI mode.
- **Runtime protocols are young**: ACP steering is unstandardised, Claude Code's mid-turn input is undocumented.
- **The workspace surface is the easiest part to over-build** into an extension platform, as vqro did. Every element we add stays generic and documented (`spec/core-protocol.md`).
