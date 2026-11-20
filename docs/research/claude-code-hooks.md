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

The declaration is printed by `vqro-claude-hook declaration`, and launchers write it to
`doc/agents/terminals/<rN>`. It contains:
- `points` as a map: `turn` and `settle` are exact; `idle` is **approximate**.
- `insert: context` and `evidence: none`.
- `framing` per point: hook context arrives as a system reminder above the user; a rewake is a task notification
  labelled `NOT USER INPUT`.

Senders must accept `render: ["context"]`. Because idle is approximate, the inbox serves only boundary and settle
records there, never `start` (coordinator ruling, 2026-11-20). Starting an idle Claude Code session goes through the
native session socket (`admit`) or the human. An older inbox that ignores precision and hands a `start` record to
the idle waiter gets it back released, and the hook backs off.

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
| `attachment.type: hook_additional_context`, `hookEvent` PostToolBatch/Stop, carrying the hand-off's nonce | the plugin's own context entry | the anchor |
| `type: user` with `origin.kind: task-notification` carrying the nonce | the plugin's own rewake | the anchor |
| `api-request-blob` carrying the nonce after the anchor, then an `assistant` entry with a non-error `stop_reason` | the request payload that was sent, and a finished answer | `read` |
| tool results, human prompts, peer messages (`origin.kind: peer`), `hook_success` / `hook_non_blocking_error` | text anyone can put there | **never evidence** |

Each hand-off carries a nonce in its header (`[ref <32 hex>]`) that only the plugin knows. Senders cannot forge it,
because their content is rendered as a quote (`> …`) under the plugin's header. A tool result or a human prompt that
echoes the nonce does not count. Tests cover a forged header in content, and a tool result or human quote of a real
nonce.

The hand-off is reported `supplied` only after the hook's output is written and flushed; if the write fails, its
claims are released. `supplied` means context with no durable entry at that moment. `supplied` and `inserted` are
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
  Code hook cannot emit OSC 7501.** Tier-1 status therefore goes to K3's `agents.status {terminal, app, state,
  seq}`. The endpoint is the terminal; `seq` is epoch ms; it is lossy and advisory.
- Hook status is lossy:
  - Esc fires no `Stop`, so `working` lasts until the `idle_prompt` notification;
  - subagent hooks carry `agent_id` and are ignored.

  Herdr dropped hook-based Claude state for the same reasons (its CHANGELOG, #198 and #349).
- `Stop` continuations are capped at 8 in a row. The plugin stops taking at `settle` after 4.
- The `idle` point is approximate: a take can race a prompt the human is submitting at that moment. In that case the
  message shares the human's turn as a background notification. It is reported honestly as `supplied`. This is why
  only boundary and settle records are served there.

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

The P5 prototype, productised on the coordination-v0 surface. It exports its declaration as `PULL_DECLARATION`:
- every point is exact, because idle is Pi's in-process `isIdle`, and an idle take is a followUp that never steers a
  run that started meanwhile;
- `insert: user`, `evidence: entry`, framed with the plugin's sender header.

It runs inside Pi, holding one core connection.
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

## Robustness (review A3)

- **Lost take reply:** the plugin inspects that exact op with `coord.inbox.op` on a fresh connection. It returns the
  take's claims, never content, and they are released. A new take is never sent instead: if the window epoch moved
  on, it would be a different op and claim nothing back. Tested with an injected 4.5 s reply delay, with and without
  a window change (fake inbox), for both plugins.
  - **If the inspection or a release itself fails** (no answer), the op is kept: in the hook state for Claude, in memory
    for Pi. Nothing is taken until recovery succeeds, because an inbox refuses every take while a claim is in flight.
    The next point retries it, and so does the idle waiter or a 3 s timer when idle.
  - A reconciliation (a new consumer) clears the list. Tested with a failed `coord.inbox.op` after a lost take, for
    both plugins.
- **Record size (Claude Code):**
  - `take {max_bytes: 5000}` (compact UTF-8 JSON bytes of the content, K2's filter). Larger records stay held
    `too_large_for_consumer` and are never claimed, so they never block later mail.
  - Quoting adds at most 3 bytes per line and a line costs at least 2 JSON bytes. Header fields are clipped to
    120 characters. So any record taken renders within the 9,000-character budget, even one that is all line breaks
    (tested).
  - `max_bytes` is required (K2 has it from ab310ac). If an inbox ignores it, a record that can never fit is released
    alone and the records behind it are handed over at the same point: it never starves them.
  - While such a record is all a take yields, the plugin backs off: no take for 2 s, doubling to 60 s. That avoids a
    take plus a release at every point. The backoff ends at the first normal take; new mail waits at most until it
    expires.
- **Context budget:** whole records only, up to 9,000 characters per hand-off. The rest are released in order and
  taken at later points.
- **Content:** `take {accept: [...]}` (K2's filter). Claude accepts text; Pi accepts text and images. Other records
  stay held `unsupported_content`, never flattened.
- **Compaction and `/clear`:**
  - `SessionStart` with these sources keeps the consumer and its window; it is not a restart.
  - Claude Code 2.1.329 gives `/clear` a **new session id** (verified in the real TUI, `claude_code_tui` I6). The hook
    finds the previous session through a per-agent pointer file that also records the Claude Code process (the hooks'
    parent; it is also the pid in Claude Code's `cc-socks/<pid>.sock`). Only the same process carries the consumer
    over. Another Claude Code in the same agent starts its own consumer (which fences the first: one per agent).
  - An idle waiter armed before `/clear` stays alive and can wake the new session. Its hand-off is recorded in the old
    session, whose transcript no longer grows, so it stays `supplied` and is never upgraded to `read`.
  - Hand-offs open in the cleared session stay `supplied` (their context is gone, so they are never `read`).
  - Mail keeps flowing in the new session (tested).
- **Reconciliation pages:** a new consumer follows `state: reconciling` page by page (same consumer, one op per
  page, at most 64) until the inbox answers `reconciled`. Tested with a paged fake (Claude: 1 claim per page; Pi:
  extra pages).
- **Reconnects:**
  - The Claude idle waiter reconnects with backoff.
  - The Pi plugin reconnects and resubscribes after a core restart, and reconciles again when its consumer is
    fenced. Tested by restarting the core under a running Pi.
  - A connection whose hello or subscribe fails is dropped, never kept half-open.
- **Lifetimes:** no helper outlives what it serves.
  - The Claude Code hook (`hook` mode) is bounded by 3 s call deadlines.
  - The idle waiter (`wait` mode) exits when its parent Claude Code goes away (checked every loop, at most ~5 s
    apart).
  - The Pi plugin starts no processes. Its idle core connection and its reconnect timer never keep Pi alive; only a
    call in flight does, for at most 5 s.
  - After `session_shutdown` the Pi plugin cancels its reconnect timer and destroys the connection.
  - Pi 1.5.2 exits explicitly in RPC and print mode, so even the earlier plugin never kept it running. These are
    hardening for hosts that wait for the event loop to drain.
  - Tested: print mode exits after its answer. With the core gone and a reconnect pending, RPC Pi exits promptly when
    its input closes and never reconnects afterwards.
  - Test harnesses must drain Pi's stdout: Node writes to a pipe synchronously, so an undrained pipe blocks Pi mid-run.
    That was the cause of an intermittent "lost take, window moved on" failure.
- **View facts in tier-1 status:** `agents.status {…, facts}` (coordination-v0 §7.1) carries only what each runtime
  reports. Claude Code gives `cwd` and the transcript's model. Pi gives `cwd`, model, label, thinking level and its own
  context window and figure. Session token totals and cost are not available to either plugin, so they stay absent,
  never summed. A refused fact never costs the state report: it is resent without facts.
- **Report order (Pi):** `read` is sent only after its `inserted` report has completed.
- **Evidence nonce:** 128 bits from the OS. If randomness fails, nothing is handed over and the claims are released.
- **Op ids:** report op ids are deterministic per claim and stage, and the command prefix is configurable
  (`VQRO_COORD_PREFIX`).
- **Hook state:** private (0700/0600; symlinked directories refused), with a rotated 1 MiB log.
- **Test faults:** fault injection lives only in a test wrapper (`tests/fixtures/pi/vqro-pull-faults.ts`). The
  shipped Pi plugin reads no fault setting.

## Open
- **Real-model smoke:** this needs a native Claude Code login (I0 helper), a cheap model and coordinator approval.
- **Windows:** the plugin is Unix-only (named-pipe client not written). Claude Code uses a named pipe there.
- **Agent identity:** `VQRO_AGENT` is read from the environment until principals by connection (C2) land.
