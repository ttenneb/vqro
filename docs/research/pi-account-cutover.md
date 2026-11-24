# Pi credential-only cutover: integration notes

Implementation in progress; not a native interoperability certificate.

## Native history and logical work continuity

Restart uses the native public `get_state.sessionFile` reference (projected as
`data.agent.session.native.file`), not a guessed UUID lookup. The native runtime
opens its own history; vqro never reads/copies transcript bytes. A missing reference,
including an in-memory session, refuses before stopping A. Native quiesce checks
that exact reference plus session/model/error timestamp. External or legacy history
locations are history references only, never account-selected configuration folders.

The bridge reports experimental `data.agent.tool_principal` only when its existing
`caps.agent.tool_principal: "env"` proof succeeds. Resume preserves that non-owner
logical work principal rather than creating a fresh undo/request scope. The
protected claim binds both references. The frontend finishes one already-admitted
lifecycle restore on the original execution backend, with no prompt; common checks
current standing policy before the separate NEW visible turn.

Accounts use native provider aliases in **one shared native auth store**, not
per-account configuration folders. Pi owns login, credential reads and refresh.
vqro neither copies tokens nor imports old profile credentials. The alias forwards
the public, fully composed provider and preserves later extension overrides and
the complete model catalog. Private virtual routes remain owned by their native
provider: account switching is unavailable for those models, not emulated.

The shipped native extension must load first and select the alias before input
admission. Setup failure explicitly requests shutdown and guards all input sources,
even if the UI is broken. The bridge requires native alias/model readback before
registering an endpoint. Native sign-in uses a separate login-only registration;
the human runs `/login` and chooses the labelled native alias. No prompt is sent.

## Explicit human-approved Pi-only inference exception

Pi 1.5.2's public assistant-error DTO does not retain an authoritative quota type.
Its broad retry classifier is not a usage-limit classifier. Ordinarily vqro must
not classify error text. **The human explicitly approved an exception for Pi:**
AUTO may match only the small, versioned table in
`extensions/pi_accounts/limit_messages.json`. This is inference, not a native
quota fact. Successful switches must visibly say:

> switched because the error looked like a usage limit

Each entry cites its pinned native program source or provider documentation.
The first recognizes the exact native OpenAI/Codex usage-limit renderer. The
Anthropic entry recognizes only the complete SDK `429 {JSON}` serialization with
the documented `error.type = rate_limit_error`, not arbitrary English keywords.
Its English example is synthetic; no private user error was read. Unknown shapes,
wrong providers, successful assistant content, incomplete/truncated records,
duplicate JSON keys and near-misses do not match. They fall back to the explicit
owner-click **Continue on account B** path, labelled as human-judged.

The matcher and all message classification live in the Pi integration, never
common code. `tests/test_pi_account_limits.py` covers every table entry and
negative cases. Source-only evidence does not establish authenticated acceptance
by a provider; that still needs separately authorized native checks.

## Shared policy and no replay

NOTIFY has no restart effect. OFFER requires the core-authenticated effective
caller principal to equal the current configured workspace owner; the protected
claim records that principal and owner-entry revision. AUTO is once per native
limit episode per conversation across adapter epochs, with a rolling-hour cap
`accounts.auto_cutover_max_per_hour` (integer 0–10, default 3; 0 means OFFER).
Unknown/stale/incomplete/blocked target capacity never means headroom. API-key
overflow remains a separate opt-in; unsupported native transitions must refuse.

Read-only native preparation may wait for busy siblings without consuming an
intent. A protected setup claim precedes native effects. Native episode, source
account, session, incarnation and epoch are checked again before execution.
Unknown or partial effects retain their fence. A graceful old-process exit must
be observed before starting another native owner of that conversation.

After an eligible AUTO result, common policy submits exactly one **NEW visible
vqro/tool-origin continuation**, with a separately journalled per-conversation
intent. It never copies earlier input, a failed prompt, history or queued input,
and never retries an uncertain submission. OFFER waits for the human's next turn.
