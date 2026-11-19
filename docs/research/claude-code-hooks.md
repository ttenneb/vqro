# Claude Code as a tier-1 agent: hooks pull at safe points (A3, 2026-11-19)

Claude Code 2.1.329 (I0 install), measured with the real CLI against a fake Anthropic endpoint in a private network
namespace. No model calls.
- Plugin: `src/bin/vqro-claude-hook.rs`.
- Tests: `tests/claude_hook_e2e.py` (full gate; no Claude Code needed), `tests/claude_code_e2e.py` (`-p`) and
  `tests/claude_code_tui.py` (interactive PTY).
- Fixtures: `tests/fixtures/fake_inbox.py` (the pull surface of `coordination-v0.md` §7 as core commands) and
  `fake_anthropic.py`, whose request log is the oracle.

## Result

The plugin declares `pull {points: [turn, settle, idle], insert: context, evidence: entry}`.

| Point | Hook | Measured |
|---|---|---|
| `turn` | `PostToolBatch` → `additionalContext` | A message sent during tool 1 of a 3-step run is in exactly request 2. It is appended to the tool result as `<system-reminder>PostToolBatch hook additional context: …` |
| `settle` | `Stop` → `additionalContext` | The message appears only in the one continuation after the final answer, as a separate user text block, `<system-reminder>Stop hook additional context: …` |
| `idle` | `asyncRewake` waiter armed at `Stop` and `Notification` `idle_prompt` | It wakes an idle TUI in the next request. Claude Code frames the message as `[SYSTEM NOTIFICATION - NOT USER INPUT] … background-task event` |

The waiter is a hook process blocked on the inbox's counts-only wake (`inbox-wake-<rN>` live view):
- wake latency is 5-6 ms;
- a hook without mail costs about 2 ms (debug build, including process start);
- nothing polls.

A human's unsent draft in the editor survives a wake and is later submitted byte-identical. A new prompt retires an
armed waiter; a message sent while busy stays held until the next idle.

## Evidence (transcript entry types, 2.1.329)

| Entry | Meaning | Use |
|---|---|---|
| `attachment.type: hook_additional_context` | the context entry in the conversation | `inserted` |
| `type: user` (not `isMeta`) carrying the id | a rewake or peer message | `inserted` |
| `api-request-blob` carrying the id, then an `assistant` entry with a non-error `stop_reason` | the request payload that was sent, and a finished answer | `read` |
| `attachment.type: hook_success` / `hook_non_blocking_error` | a record of hook stdout/stderr | **not evidence** |

The hand-off itself is reported `supplied`. A later hook upgrades it from the transcript. Because the transcript is
written asynchronously, evidence usually arrives one hook later. A new consumer (`SessionStart`) runs
`inbox.reconcile`: an open claim with no model-visible entry becomes `unknown` and is never taken again (checked
with `--resume`).

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

## Open
- **Real-model smoke:** this needs a native Claude Code login (I0 helper), a cheap model and coordinator approval.
- **Windows:** the plugin is Unix-only (named-pipe client not written). Claude Code uses a named pipe there.
- **Agent identity:** `VQRO_AGENT` is read from the environment until principals by connection (C2) land.
