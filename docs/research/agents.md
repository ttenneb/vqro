# Headless agent runtimes and protocols

Status: research, 2026-11-16. Compiled from three subagent reports (source reads of shallow clones in
`~/Projects/{pi-mono,pi_agent_rust,xal}` plus web docs). "U" = unverified. Licence notes are names only;
obligations (rider, GPL, FSL, commercial terms) go to Legal Counsel.

## Question

prim owns the agent UI, so agents run headless and prim routes all input (human, agent-to-agent,
steering, queued interrupts) to them. prim must be agnostic to the agent runtime. What should prim's
agent model be?

## Runtimes

| Runtime | Licence / lang | Headless wire | Mid-turn input | Approvals to client | Sessions | ACP |
|---|---|---|---|---|---|---|
| pi (earendil-works/pi) | MIT, TS | `--mode rpc` JSONL on stdio, clean pipes required (`docs/rpc.md:14-56`) | `steer` (after current tool calls, before next LLM call), `follow_up` (after agent stops), `abort`, `clear_queue` (`docs/rpc-commands.md:46-128`) | none built in; extension UI round-trip (`docs/rpc-extension-ui.md`) | 1 per RPC process, many via SDK | third-party adapter (U) |
| pi_agent_rust | MIT + OpenAI/Anthropic rider (`LICENSE:12-46`), Rust | pi-compatible RPC and native ACP, stdio (`README.md:843-850`) | as pi, plus ACP ext `_session/steering` (`README.md:932-952`) | ACP `request_permission`; RPC `ask_request` (partly U) | many (ACP), fork/resume | native |
| xal (xal-sh/xal) | MIT, TS/Bun, beta | one-shot `xal run --format jsonl` only (`apps/cli/src/agent/cli.ts`) | in-process only: `send` queues, `steer` aborts the in-flight stream (`session.ts:977-1010`) | in-process only | 1 | none |
| Codex app-server | Apache-2.0, Rust | JSON-RPC JSONL on stdio; WS/socket experimental ([docs](https://learn.chatgpt.com/docs/app-server)) | `turn/steer` (needs `expectedTurnId`), `turn/interrupt` | `item/*/requestApproval` | thread start/resume/fork | via codex-acp |
| Claude Code / Agent SDK | proprietary | `claude -p` stream-json NDJSON ([headless](https://code.claude.com/docs/en/headless)) | queued messages + `interrupt` ([streaming](https://code.claude.com/docs/en/agent-sdk/streaming-vs-single-mode)); exact injection point U | `canUseTool` / `--permission-prompt-tool` | resume/continue/fork | via claude-agent-acp |
| OpenCode | MIT, TS | `opencode serve` HTTP+SSE ([server](https://opencode.ai/docs/server/)) | V2 (experimental) session inbox with delivery modes, steering ([api](https://opencode.ai/docs/api/)) | `permission/{id}/reply` | many, fork | native ([acp](https://opencode.ai/docs/acp)) |
| goose | Apache-2.0, Rust | `goose acp` stdio, `goose serve` ACP over HTTP/WS ([man](https://www.mankier.com/1/goose-serve)) | cancel; steer U | via ACP (U) | via ACP (U) | native |
| zerostack | GPL-3.0, Rust, ~30k LOC | `-p`, `--status-socket` ([repo](https://github.com/gi-dellav/zerostack)) | queue only | U | U | build feature |
| Gemini CLI | Apache-2.0, TS | stream-json, `--acp` ([acp](https://geminicli.com/docs/cli/acp-mode/)) | U | via ACP | U | native |
| Amp | proprietary | `--stream-json-input` ([docs](https://ampcode.com/docs/cli/streaming-json)) | queue, `steer: true` at next interruption point | not documented | continue | U |

Dropped: aider (dormant, no structured stream), Crush (no stable server/ACP yet).

**OSC 7501:** no headless runtime found emits it; only a Cursor feature request exists
([forum](https://forum.cursor.com/t/emit-osc-7501-program-status-in-cursor-agent-idle-working-blocked-done/177003)).
Status for agents must come from their event streams, not the terminal.

## Protocols

- **ACP** (Apache-2.0, [spec](https://agentclientprotocol.com/protocol/overview)). Stable v1: stdio JSON-RPC,
  sessions, `session/update` stream, `request_permission`, client-provided fs/terminal, MCP passed per session.
  ~60 agents listed ([agents](https://agentclientprotocol.com/get-started/agents)). Gaps:
  - v1 `session/prompt` resolves at turn end; no standard steer/queue.
  - v2 is Draft ([announcement](https://agentclientprotocol.com/announcements/acp-v2-draft)): prompt returns on insertion,
    queue/steer deferred to a later RFD.
  - PR #1261 proposes `delivery: "steer" | "queue"` on `session/prompt`, unchampioned
    ([PR](https://github.com/agentclientprotocol/agent-client-protocol/pull/1261)).
  - Fork unstable; no standard remote transport; Rust crate 3.6 → 5.3 in November
    ([crates.io](https://crates.io/crates/agent-client-protocol)).
- **AG-UI** (MIT): web-frontend event protocol, interrupt = end run and resume; no fs/terminal. Poor fit.
- **A2A**: remote opaque agent-to-agent tasks. At most an outer edge for cross-machine agent messaging.

## Findings

1. **"Steer" means at least three different things**, and no standard settles it:
   - *at boundary*: inject after current tool calls, before the next model call (pi, Amp, Codex `turn/steer` U on exact point);
   - *after settle*: hold until the agent stops, then start a new turn (pi `follow_up`, Claude queue);
   - *preempt*: abort the in-flight response and continue with the new input (xal `steer`).
   Plus `interrupt` (stop, no new input) everywhere.
2. **Every headless protocol needs clean pipes or HTTP**, never a PTY (pi `docs/rpc.md:56` is explicit).
3. ACP is the widest adapter surface but should not be prim's core model: its steer/queue is unsettled,
   v1/v2 differ in lifecycle, and the crate churns.

## Proposed agent model (draft, superseded in part by decisions.md D4)

Consistent with the no-policy-in-core rule (`memory: prim-no-policy-in-core`):

- **Core facts:** agent node → sessions → turns → items (message, thought, tool call, plan, diff); agent
  state (idle / working / blocked / settled) derived from the stream; pending requests
  (permission, question, elicitation) as facts with IDs.
- **Core effects** (neutral, each adapter declares which it supports natively):
  `deliver(session, input, mode: boundary | settle | preempt)`, `interrupt(session)`,
  `answer(request, choice)`, `session.new / resume / fork`.
- **Not core:** choosing the mode, holding queues when a runtime lacks a mode, emulating preempt via
  interrupt + re-prompt, mailboxes, agent-to-agent routing. These are extensions over the facts + effects.
- **Adapters:** ACP first (covers goose, OpenCode, Gemini, pi_agent_rust, Claude and Codex via adapters),
  with the delivery mode mapped to `_meta`/#1261 shape; native Codex app-server and pi RPC adapters where
  ACP loses steering.

## Open: hosting agents on a backend

Backends give processes a PTY, but agents need pipes. Candidate to spike: a small `prim-host` process
runs inside the backend terminal (so herdr-core/tmux/Rex still supervise, persist and place it on remote
machines), spawns the agent with pipes, connects back to prim over a socket, and draws a fallback
status view in its own pane (and emits OSC 7501 so backend-native UIs still see state).
