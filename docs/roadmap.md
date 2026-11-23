# Roadmap

**vqro is a working prototype, and fluid.** This page is a living priority list: where we are and what comes next.
It is revised as we learn, not a commitment, and there is no "finished" release cut planned. Detail of what was
built and when: `build-plan.md`.

## The aim (2026-11-20, confirmed 2026-11-23)

Switch agents and multiplexers without the interface changing. The first useful version means all three together:
1. a dependable daily workspace for terminals and agents;
2. the agent standard working on Pi and on another genuinely different runtime;
3. the same setup across at least two multiplexers and two runtimes.

These are quality **milestones**, each shown by a proof (below), not a release gate. Building continues on
everything in flight and more.

## Where we are (2026-11-23)

Used daily over herdr-core on Linux:
- **Core:**
  - durable transactional store and event log;
  - command routing;
  - extension supervisor;
  - bounded streams;
  - live view content.
- **Terminal engine:** libghostty is the default terminal engine (vt100 selectable).
- **Backends:**
  - out-of-process `vqro-herdr` (default) and `vqro-tmux` (off by default);
  - the optional Rex provider.

  A same-setup test compares one workspace on herdr and on tmux.
- **Agents:**
  - Rust Pi RPC bridge;
  - Codex app-server adapter with a daily launcher;
  - Claude Code hooks and tier-1 launchers;
  - the `agents` and `coord` extensions.

  The standard covers customization (slice 1), child agents, background tasks, plans and goals (agent-v0 section 4).
  Capability subsets differ per runtime; qualification on the current build is pending.
- **TUI and bundled extensions:**
  - tabs, panes and workspaces;
  - sidebar, menus and settings;
  - themes, icons and keys;
  - the agent pane;
  - setup checks for agent changes (Unit, Integration, Performance).
- **Windows native:**
  - named-pipe transport, durable store and Python helpers;
  - providers with console-less background processes;
  - a development installer and native check script.

  It runs on the human's machine; its acceptance checklist is pending.
- **macOS:**
  - the build, `check.sh` and the tests run there;
  - the core finds Python 3.11 or newer outside a login shell.

  It has been verified on Linux with macOS conditions simulated; its acceptance checklist on the human's Mac is
  pending.

## Milestone proofs

| Milestone | Proof | State |
|---|---|---|
| Daily workspace | A clean install of current main; several days of daily use without regressions; empty start opens Scratch with a shell | in progress |
| Agent standard | Conformance run on the current build for Pi and Codex, failures fixed or named (last full run, an older build: 69 pass / 10 fail / 1 n/a) | to run |
| Same setup | Automated herdr/tmux comparison (exists) plus a daily trial on tmux | trial planned |
| Platforms | The native Windows and macOS acceptance checklists pass on the human's machines | pending on the machines |

## Priorities now (in flight, revisable)

1. **Accounts for capacity:**
   - one shared setup; swap only the login;
   - continue the same conversation on another account;
   - Pi first, then Codex, then Claude by keychain token;
   - Pi limit switching: known messages automatically, a click otherwise.
2. **Claude Code through its headless stream-json interface** (route 1) as the main integration, with subagents,
   tasks and plans first-class.
3. **Unit check required for agent changes to a vqro setup**, once it is clean on the human's setup.
4. **The milestone proofs above:** conformance run, tmux trial, platform checklists.
5. **Daily UI feedback** as it arrives (empty start, table sorting, menus).

**Later, on purpose:**
- WSL;
- multi-machine;
- Alacritty as a second engine;
- a Claude watch-only mode (route 3);
- macOS CI;
- a `timeout` replacement.

**Not doing:**
- herdr-core changes;
- Plane;
- per-account profile folders.

Legal and licensing questions never block building; they are listed for a later review.

### Architectural constraints for multi-machine (apply now, in every stream)

- No backend-specific remote transport in contracts; a backend's own channel may only be one optional transport behind
  a neutral contract (D23).
- The core endpoint stays local and owner-only (D22); anything remote reaches it by forwarding that terminates locally.
- Resource identity never depends on a host's alias or hostname: provider names and refs use permanent ids.
- A bridge never derives how to reach the core; the provider that places it on a host supplies a usable value or
  refuses.
- Facts and spawn arguments name machines neutrally (`machine`, `placement.machine`), never by parsing refs.

Kill / rethink questions: does any primitive assume one host or one OS? Does a provider need the core to know its
transport? If yes, the contract is wrong. Freeze candidates are only pieces that survived two backends, two
runtimes and two operating systems.

Rules: one trunk, short branches or worktree streams; small verification per change; record surprises in
`research/` and binding choices in `decisions.md`.
