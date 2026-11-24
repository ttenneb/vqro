# Credential-only runtime accounts

Accounts select **credentials over one shared runtime setup**. Settings, skills,
extensions, plugins, trust, models and history are not split into account folders.
`accounts.show` manages nonsecret registrations in `doc/settings/accounts`.
Native runtimes own subscription login, token storage and refresh. vqro does not
open native credential/profile files, copy tokens or replay earlier input.

Example Pi subscription registration:

```json
{"id":"capacity-a","label":"Capacity A","runtime":"pi","provider":"openai-codex","kind":"subscription","native_provider":"vqro-account-a"}
```

Pi uses public native provider aliases in its single shared native auth store.
The shipped native extension delegates to the native OAuth provider; it never
falls back to ambient API keys. Sign in through the native `/login` command.
Virtual model routes remain native: account switching is explicitly unavailable
for those models, not privately reconstructed. See `docs/research/pi-account-cutover.md`.

Codex registrations use the native saved-login reference:

```json
{"id":"capacity-b","label":"Capacity B","runtime":"codex","kind":"subscription","login_ref":{"sessionId":"native-saved-session-id","accountId":"native-account-id"}}
```

Claude uses a same-execution-host opaque credential reference (`host`, plus a
32-character hexadecimal `secret_ref`) through its approved native host helper.
No subscription token is returned through workspace metadata or command replies.
Native credential ownership and refresh are not emulated by the common policy.

IDs identify registrations, not independently authenticated provider users. Known
identical references in the same runtime/provider/kind/host namespace count once;
all aliases of a source or known-blocked credential are excluded together. Missing
or contradictory routes remain visible but unselectable. No secret audit tries
to discover whether otherwise different references belong to the same user.

Legacy migration changes registry metadata only. It removes account-driven
configuration-folder linkage without inspecting, copying, moving or deleting any
native folders/files. Incomplete migrated registrations require explicit native
setup. `profile` and Pi `auth_file` overlays are not account mechanisms. Account
selection never changes `PI_CODING_AGENT_DIR`, `CODEX_HOME` or `CLAUDE_CONFIG_DIR`;
explicit shared runtime settings still control their one native setup.

## New launches and native sign-in

`pi.new`, `codex.new`, and the supported Tier-1 launchers offer registered accounts.
**Shared runtime login** preserves ordinary native login behavior (`account:"normal"`
is the legacy transport spelling, not a separate profile). Explicit choices win;
choices carry the registry revision and stale choices refuse before launch.

Sign-in opens exactly one backend-owned native terminal, never types into it and
never claims that login succeeded. The runtime owns its browser/device flow. The
configured executable prefix and recognized inert options are preserved; prompt,
resume, noninteractive or unproved argv shapes refuse before a sign-in launch.
A failed/uncertain terminal launch is never automatically repeated.

The standalone execution-host keychain helper and explicit `api_key_overflow`
setting remain. Named API-key launches currently refuse before spawn: their
execution-host/native credential binding is not verified; they never fall back to
the shared login. API-key accounts: launching is not available yet; the keychain entry is kept.
Wiring the helper into named launches (execution host and native auth precedence)
is an approved post-release follow-up, not part of this slice. **Continuing a conversation on an API-key account: not yet.**
They are not same-session cutover targets in this slice.

## Capacity strategies

`doc/settings/accounts-policy` contains `new_agents`, `blocked`,
`api_key_overflow`, `continuation_text`, and `auto_cutover_max_per_hour` (0–10,
default 3; zero disables AUTO cutover). Invalid settings fall back conservatively.

- `new_agents: ask` is the default. `most_remaining_capacity` ranks comparable,
  sourced account-scoped percentage reports. Missing, incomplete, stale, expired,
  conflicting or denominator-free reports stay unknown. Every live alias contributes
  its constraints; a positive subset never hides missing/negative evidence.
- `round_robin` rotates eligible credential groups, not aliases. Its transactional
  cursor records an attempted choice; failed/unknown launches consume that choice.
- Reports describe what the publisher said, not entitlement, independent identity
  verification, a quota reservation or guaranteed capacity.

## Same-conversation limit cutover

`blocked: notify` performs no native restart/input effect. `offer_restart` requires
an explicit choice by the core-authenticated current workspace owner. `auto_restart`
may target UNKNOWN capacity, but never a KNOWN-blocked credential. A native limit
observation stays known-blocked until its reported reset, or for one hour when no
reset is available. The source is marked at cutover, preventing account ping-pong.
Percentage/time hints alone are not native limit verdicts.

One protected claim binds the original native episode, conversation, incarnation,
epoch, source credential and exact prepared membership. A Codex claim can contain
an ordered bounded `authorized_targets` plan chosen by common policy. The full
plan reserves hourly slots for **every affected native conversation**. A definite
attempted prefix refunds unused slots; uncertainty retains the entire reservation.
A failed/unknown native effect or input submission is never replayed.

Codex switches only under retained native host fences and inspects B through native
`account/rateLimits` before any continuation. Only authoritative
`ordinaryUsageAllowed` true/false/null is used, never an inferred positive from
percentage/reset time. A next target follows only a definite blocked inspection
with no input sent. Exact conversations are restored and fences released on known
nonready results: unknown usage offers an owner-confirmed continuation on the SAME
claim (no second switch); all blocked notifies with zero continuation. Unknown
native outcomes remain unknown/fenced and are not retried. Consent revocation stops
further credential switches/input, not the necessary lifecycle restoration.

Claude and Pi have singleton plans: their one NEW visible vqro/tool-origin
continuation is the probe. A limit on that turn is a new episode, subject to the
hourly cap. Earlier/failed prompts, queues and transcripts are never copied into
a fresh agent. Pi's conservative versioned error-message inference is a documented
human-approved exception, labelled **switched because the error looked like a
usage limit**. Other Pi errors require an owner choice.

Claude AUTO requires latest idle native episode, a complete empty vqro-owned input
ledger, and a complete empty native background level at both checks. Native queue
remains null, never fabricated empty. A dedicated owner-confirmed warning may set
protected `authorization.background_loss_disclosed:true` for OFFER; it bypasses
ONLY background, never ledger-empty/latest/idle. AUTO's absent flag means false.
Claude's explicitly allowed unconfirmed-resume case is visibly labelled and never
gets a fabricated native session guard.

Pi refuses ephemeral/unpersisted history or unknown logical-work identity before
stopping A. Native graceful shutdown settles credential writes; a replacement starts
only after successful old-process exit. It resumes the native history reference and
preserves the logical tool principal/model, with exact native readback before input.

Current policy/authority is checked after native completion and before every new
turn intent/submission. The continuation is separately recorded and visibly
vqro/tool-origin, never attributed to the human. Operational uncertainty/full journals
refuse rather than silently forget protected attempts.

This is an experimental integration. Fake callback/worker tests do not certify
native cross-account acceptance, billing, keychain interoperability or release
readiness; those require their separate authorized native gates and reviews.
