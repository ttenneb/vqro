# Roadmap

Revised 2026-11-16 around D14. Each step has an exit test and a kill question. If a kill question's answer is bad,
stop and redesign. No irreversible commitment before the tests in `test-plan.md` report.

| | Step | Builds | Exit test | Kill question |
|---|---|---|---|---|
| **Tests** | `test-plan.md` | throwaway spikes: agent input routes, view models, extension lifecycle | written results; capability tables filled in | Do the primitives hold up against real runtimes and two very different workspaces? |
| **M1** | Core + first adapters | core (primitives, facts, log, effects), herdr-core adapter, one agent bridge (pi or Claude Code) | an extension builds a workspace with a terminal and an agent; the core restarts without disturbing either | Is any adapter more than thin? |
| **M2** | Second backend + second runtime | tmux or Zellij adapter; the other agent runtime | the same extensions run unchanged on both backends | Did anything leak backend-specific assumptions into the primitives? |
| **M3** | TUI, daily use | ratatui client; default workspace as replaceable extensions; native-TUI agent mode | the owner uses prim for real multi-agent work for a week | Better than the owner's current setup? |
| **M4** | Agents as authors | an agent changes the workspace through commands; delivery strategies, mailbox, notifier as extensions | an agent builds a meaningful workspace change; swap delivery strategy without restarting anything | Was anything missing from the primitives? |
| **M5** | Standard v1 | spec and protocol docs good enough for third parties; Codex and ACP adapters; Rex adapter if its API settles | someone could write an adapter from the spec alone | |
| **Later** | | GPU client, more backends, Windows, publish | | |

Rules: one trunk, short branches; a small verification plan per change; record surprises in `research/` and binding
choices in `decisions.md`.
