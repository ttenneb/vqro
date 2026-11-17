# Roadmap

**prim is a working prototype, and fluid.** This page says where we are and what we think comes next; it is
revised as we learn, not a commitment. Detail of what was built and when: `build-plan.md`.

> **2026-11-17: the human will revisit priorities after the current hardening and fixes.** Until then only daily
> use and hardening are active; the platform and multi-machine streams below are candidates, not an order. The
> multi-machine constraints (D23, confirmed) apply to all work now.

## Where we are (2026-11-17)

Working and used daily over herdr-core on Linux:
- Core: durable transactional store, event log with boot epochs, commands routed to owning connections, extension
  supervisor with a lifetime channel, bounded streams and queues, live view content, scale harness.
- Providers (`spec/provider-v0.md`, section 9): wire protocol and Python/Rust SDKs, out-of-process `prim-herdr` and
  `prim-tmux`, a Pi RPC bridge and the `agents` extension. The in-process herdr adapter is still the default.
- TUI client and the bundled extensions (tabs, panes, gestures, workspaces, sidebar, themes, icons, keys, settings).

Not yet: native Windows (compiles except the transport; runtime gaps in `build-plan.md`, "Windows work"), macOS
validation (compiles), WSL validation, any second machine.

## Streams (run in parallel)

| Stream | Priority (to be revisited) | Done when | Main risk |
|---|---|---|---|
| **Daily use** on this machine | active | Daily work runs on prim without regressions; scale budgets hold | Regressions while the other streams land |
| **Windows native** | to be set | Core, TUI and default extensions run natively (named-pipe endpoint, durable store, paths, stop request, Python helpers); a Windows backend provider drives ConPTY terminals | ConPTY output re-rendering; no Windows machine for runtime tests yet |
| **WSL** | to be set | Both modes work: prim inside WSL (Linux build), and native-Windows prim driving WSL terminals through a provider that owns the `wsl.exe` transport; agents in WSL reach the core through the neutral reachability contract (D23) | Bridges in WSL can't open a Windows named pipe: needs the reachability mechanism early |
| **macOS** | to be set | Core, TUI, `prim-herdr` and `prim-tmux` run on macOS; keys and mouse verified in Terminal.app and iTerm2 | Terminal key/mouse differences; provider naming (`/proc` hostname) |
| **Multi-machine** | backlog | prim on one machine drives another machine's terminals and agents through backend-neutral mechanisms | Remote agent reachability (options in D23); identity across hosts |

### Architectural constraints for multi-machine (apply now, in every stream)

- No backend-specific remote transport in contracts; a backend's own channel may only be one optional transport behind
  a neutral contract (D23).
- The core endpoint stays local and owner-only (D22); anything remote reaches it by forwarding that terminates locally.
- Resource identity never depends on a host's alias or hostname: provider names and refs use permanent ids.
- A bridge never derives how to reach the core; the provider that places it on a host supplies a usable value or
  refuses.
- Facts and spawn arguments name machines neutrally (`machine`, `placement.machine`), never by parsing refs.

## Next (in this order within each stream, revisable)

1. Contract wording before anything freezes: D23 rules into `provider-v0.md` (bridge reachability, `machine` fact,
   placement), Windows endpoint (D22).
2. Windows native: transport module, store replace, paths, stop request, Python named-pipe helper; `cargo check` for
   Windows in CI.
3. WSL: prim inside WSL smoke; then a Windows-side provider over `wsl.exe` and the bridge reachability mechanism.
4. macOS: smoke run of core, TUI, providers; fix provider default names and the herdr command line.
5. Second runtime (Claude Code) and undo for user-initiated extension changes, as daily use needs them.
6. Freeze candidates: only pieces that survived two backends, two runtimes and two operating systems.

Kill / rethink questions: does any primitive assume one host or one OS? Does a provider need the core to know its
transport? If yes, the contract is wrong.

Rules: one trunk, short branches or worktree streams; small verification per change; record surprises in
`research/` and binding choices in `decisions.md`.
