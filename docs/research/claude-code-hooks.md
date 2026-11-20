# Tier-1 pull plugins: Claude Code hooks and Pi (A3, 2026-11-19)

Claude Code 2.1.329 (I0 install), measured with the real CLI against a fake Anthropic endpoint in a private network
namespace. No model calls.
- Plugin: `src/bin/vqro-claude-hook.rs`.
- Tests: `tests/claude_hook_e2e.py` (full gate; no Claude Code needed), `tests/claude_code_e2e.py` (`-p`) and
  `tests/claude_code_tui.py` (interactive PTY).
- Fixtures:
  - `tests/coord_harness.py` with `--inbox fake|k2`;
  - `tests/fixtures/coord_k2.py` runs the default coordination extension (K2, branch coord-ext) unchanged on a
    real core. It stands in for the two core pieces not yet on main: K2's MemoryStore for C1, and principal by
    connection author for C2;
  - `fake_inbox.py` is the same `coord.*` dialect, minimal;
  - `fake_anthropic.py` and `fake_openai.py` are fake models whose request logs are the oracle.

Both plugins speak `coord.inbox.window/reconcile/take/report` through `command.invoke`. Every mutation's `op`
carries the window epoch, retried once after `expired_operation`. The wake is the `coord-status` live view's
`wake.pending[<rN>]`. The launcher writes the endpoint's declaration in `doc/agents/terminals/<rN>` and grants the
plugin's principal `receive`. A new coordination owner starts with the gate closed; the harness resumes it.

## Result

The declaration is `pull {points: [turn, settle, idle], insert: context, evidence: none}`. Senders must accept
`render: ["context"]`.

| Point | Hook | Measured |
|---|---|---|
| `turn` | `PostToolBatch` → `additionalContext` | A message sent during tool 1 of a 3-step run is in exactly request 2. It is appended to the tool result as `<system-reminder>PostToolBatch hook additional context: …` |
| `settle` | `Stop` → `additionalContext` | The message appears only in the one continuation after the final answer, as a separate user text block, `<system-reminder>Stop hook additional context: …` |
| `idle` | `asyncRewake` waiter armed at `Stop` and `Notification` `idle_prompt` | It wakes an idle TUI in the next request. Claude Code frames the message as `[SYSTEM NOTIFICATION - NOT USER INPUT] … background-task event` |

The waiter is a hook process blocked on the counts-only wake:
- wake latency is 5-6 ms;
- a hook without mail costs about 2 ms (debug build, including process start);
- nothing polls.

A human's unsent draft in the editor survives a wake and is later submitted byte-identical. A new prompt retires an
armed waiter; a message sent while busy stays held until the next idle.

## Evidence (transcript entry types, 2.1.329)

| Entry | Meaning | Use |
|---|---|---|
| `attachment.type: hook_additional_context` | the context entry in the conversation | located (not reported) |
| `type: user` (not `isMeta`) carrying the id | a rewake or peer message | located (not reported) |
| `api-request-blob` carrying the id, then an `assistant` entry with a non-error `stop_reason` | the request payload that was sent, and a finished answer | `read` |
| `attachment.type: hook_success` / `hook_non_blocking_error` | a record of hook stdout/stderr | **not evidence** |

The hand-off is reported `supplied` (context, no durable entry at that moment). `supplied` and `inserted` are
separate evidence branches in K2, so the transcript only upgrades a hand-off to `read`. Because the transcript is
written asynchronously, that usually happens one hook later. A new consumer (`SessionStart`) runs
`coord.inbox.reconcile` with no entries: a claim taken but never handed over (a crash between take and hand-off)
becomes `unknown`, is never taken again, and blocks the sender's later same-mode input until `coord.inbox.resolve`.
Checked with `--resume` against K2.

## Behaviour the docs do not state

- A `SessionStart` `asyncRewake` hook delays a `-p` prompt until the hook exits. Its exit-2 stderr is recorded as
  `hook_non_blocking_error` and never reaches the model.
- In non-interactive sessions (`CLAUDE_CODE_ENTRYPOINT=sdk-cli`), a `Stop` `asyncRewake` hook keeps the process
  alive. The waiter therefore runs only when `CLAUDE_CODE_ENTRYPOINT=cli`.
- Hooks have no controlling terminal, and `terminalSequence` allows only OSC 0/1/2/9/99/777 and BEL. **A Claude
  Code hook cannot emit OSC 7501.** Tier-1 status therefore needs a status call. The plugin uses the placeholder
  command `agents.terminal_status`.
- Hook status is lossy:
  - Esc fires no `Stop`, so `working` lasts until the `idle_prompt` notification;
  - subagent hooks carry `agent_id` and are ignored.

  Herdr dropped hook-based Claude state for the same reasons (its CHANGELOG, #198 and #349).
- `Stop` continuations are capped at 8 in a row. The plugin stops taking at `settle` after 4.
- The `idle` point is approximate: a take can race a prompt the human is submitting at that moment. In that case the
  message shares the human's turn as a background notification. It is reported honestly as `supplied`.

## Native per-session inbox socket (`admit` route)

`CLAUDE_CODE_MESSAGING_SOCKET` (under `$XDG_RUNTIME_DIR/cc-socks/` or `/tmp/cc-socks/`) takes an optional
`{"type":"auth","token":…}` line, then `{"type":"user","message":{"role":"user","content":…}}`. The format comes
from the binary's own help text.
- While idle, a post starts a turn framed "Another Claude session sent a message … treat it as a teammate's
  request".
- While a tool runs, the post is read before the next request, appended to the tool result. That is boundary-like.
- There is no reply, so the stage is `dispatched`. The transcript shows `queue-operation enqueue` (queued) and then a
  user entry (inserted).
- After posting, vqro can no longer edit or withdraw the message. Claude Code's `crossSessionInbound` policy may hold
  or refuse it.

Pull keeps the message editable in vqro until the safe point. The socket is a declared, weaker `admit` route with
better framing.

## Pi plugin (`integrations/pi/vqro-pull.ts`)

The P5 prototype, productised on the coordination-v0 surface. It declares
`pull {points: [turn, settle, idle], insert: user, evidence: entry}` and runs inside Pi, holding one core connection.
Its tests pass against K2 and the fake. K2 has no take content filter, so a record with a block Pi cannot insert is
released uninserted. K2 then offers it again at each safe point until it is edited; it is never flattened.
- **Points:** `turn_end` (turn) and `agent_before_settle` (settle) return `custom_message` drafts with
  `continue: true`. Idle uses a counts-only wake, then `sendMessage(triggerTurn, followUp)`.
- **Stages:**
  - `inserted` from the `context` hook (the entry is in the conversation about to be sent; boundary drafts emit no
    `message_end`);
  - `read` only after an assistant message that finished normally.
- **Reconcile:** on every `session_start`, from the session's `custom_message` entries carrying the delivery id.
- **Content:** text, image (as Pi `ImageContent`) and resource_link (as a text line). Audio stays held
  `unsupported_content`.

`tests/pi_pull_e2e.py` (RPC) and `tests/pi_pull_tui.py` (PTY) pass on Pi 0.87.0 and 1.5.2:

| Scenario | Measured |
|---|---|
| boundary | in request 2, right after tool 1 |
| settle | only in the continuation |
| edit while held | reaches the model; after take it is `too_late` |
| idle start | wakes Pi |
| image | arrives as `image_url` |
| crash after take | `unknown`, never sent |
| lost report | reconciled `inserted` with the entry id |
| abort after take | inserted but unsent, `read` only after the next answered request |
| human draft | byte-identical |

17 take calls in the whole RPC run: none without mail.

**The core drops an unchanged `view.publish`** (`src/server.rs`, live view `unchanged`). A wake whose pending count
did not change (an edit of a held record) would then never arrive. The wake content carries a revision counter.

## Open
- **Real-model smoke:** this needs a native Claude Code login (I0 helper), a cheap model and coordinator approval.
- **Windows:** the plugin is Unix-only (named-pipe client not written). Claude Code uses a named pipe there.
- **Agent identity:** `VQRO_AGENT` is read from the environment until principals by connection (C2) land.
