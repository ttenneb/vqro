# Prior art: agent workspace tools and coordination (2026-11-16)

Source: subagent survey (web). U = unverified. Star counts from aggregators, approximate. Licence names only.

## Tools

| Tool | Primitives | Drives agents via | Programmable? | Licence / activity | Caveats |
|---|---|---|---|---|---|
| claude-squad | tmux session + worktree | PTY typing; open bug "prompt lost" before CLI ready ([issues](https://github.com/smtg-ai/claude-squad/issues)) | no | AGPL-3.0 | lost prompts, stdin race |
| Conductor | isolated workspaces, Mac ([site](https://www.conductor.build/)) | U | API in Pro (U) | closed (U) | |
| Crystal → Nimbalyst | worktree sessions, kanban; extension editors exposed to agents ([site](https://www.nimbalyst.com/features/extensions/)) | U | extension SDK | Crystal deprecated | |
| vibe-kanban | branch + terminal + dev server ([repo](https://github.com/BloopAI/vibe-kanban)) | U | MCP server | Apache-2.0; company shut down 2026-04-10 ([blog](https://vibekanban.com/blog/shutdown)) | |
| Sculptor | container per agent ([site](https://imbue.com/sculptor/)) | U | no | | |
| agent-deck | tmux sessions, groups, a "conductor" ([repo](https://github.com/asheshgoplani/agent-deck)) | tmux (U) | skills | MIT | |
| Superset | worktree + terminals per task ([repo](https://github.com/superset-sh/superset)) | terminals | CLI, SDK, MCP server letting agents create workspaces | MIT per README (ELv2 earlier) | |
| Claude Code agent teams | lead, teammates, task list, JSON mailbox per agent ([docs](https://code.claude.com/docs/en/agent-teams)) | in-process or tmux panes | hooks | proprietary, experimental | no resume, status lag |
| Codex app | projects, threads, worktrees, automations ([OpenAI](https://openai.com/index/introducing-the-codex-app/)) | app-server | skills, plugins | proprietary (U) | |
| Cursor Agents Window | local/cloud/SSH agents ([docs](https://cursor.com/docs/agent/agents-window)) | own runtime | U | proprietary | |
| Zed | agent + terminal threads ([docs](https://zed.dev/docs/ai/parallel-agents.html)) | ACP | settings, MCP | U | steering only for Zed's agent |
| Warp / Oz | tabs, agent management panel ([docs](https://docs.warp.dev/guides/agent-workflows/how-to-run-multiple-ai-coding-agents/)) | PTY | tab configs, Platform API | proprietary | |
| cmux (2026) | window/workspace/pane/surface ([guide](https://flaviocopes.com/cmux/)) | PTY + OSC notifications | socket + CLI let agents create workspaces, splits, send keys | GPL-3.0+, macOS | "sending input is not the same as running a command safely" |
| herdr / herdr-core | workspace/tab/pane, agent states ([docs](https://herdr.dev/tui/docs/preview/agent-automation/)) | PTY + OSC 7501 | socket API, plugins | Apache-2.0 | |
| Paseo (2026) | daemon spawning CLIs ([site](https://paseo.sh/)) | subprocess + ACP | experimental skills for orchestration | Apache-2.0 or AGPL (conflicting) | |

## Coordination in practice

- **Claude Code cross-session messaging** ([docs](https://code.claude.com/docs/en/cross-session-messaging.md)): read "between tool calls during an
  active turn"; idle session starts a turn; outcomes **delivered / held / refused**; caps (100 held, 50 queued), duplicate
  drop, burst refusal; messages cannot approve; per-session inbox socket (`CLAUDE_CODE_MESSAGING_SOCKET`).
- **Agent teams**: "sent" = mailbox file write succeeded.
- **MCP Agent Mail** ([repo](https://github.com/Dicklesworthstone/mcp_agent_mail)): agents poll; they forget, so a reminder hook exists.
- **herdr-core**: `agent prompt --wait` refuses blocked agents; `agent_prompt_stalled` after 5 s; "a timeout does not prove that no input was sent".
- **tmux send-keys**: paste/submit races and shell-readiness races are documented in Claude Code issues.

## Lessons for prim

1. Inject at runtime-native boundaries/inboxes; PTY typing is the main source of lost input. Test Claude Code's inbox socket in E1.
2. Polling mailboxes fail because agents forget; waking must be an adapter capability.
3. Bounds (caps, duplicate drop, burst refusal) belong to the mechanism, as named parameters (D6).
4. Worktree + session + status board is a commodity and a weak business; don't rebuild it as core value.
5. Honest receipts exist in fragments (herdr, Claude delivered/held/refused); nobody offers them across runtimes.

## Is prim's value real?

- Cross-runtime router with capability adapters and honest states: not covered.
- Agent-programmable workspace: covered in pieces (Claude Code mods, cmux socket, Superset MCP). The combination
  (cross-runtime, public primitives only, replace and rollback) is not.
- No policy in core: real; every tool fuses policy into verbs.
