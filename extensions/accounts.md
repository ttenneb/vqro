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

### Add a Codex saved login without copying IDs

In **Accounts**, choose **Sign in… (shared codex login)** to open the configured
native `codex login` terminal. Follow the native flow; vqro does not inspect success
or promise that a second login was saved. Native sign-in can change the login for
external Codex CLI/IDE clients, which vqro cannot fence.

After sign-in, explicitly choose **Refresh saved logins** (or **Add Codex saved
login…**) in Accounts. Select the managed Codex agent and its setup/execution-host
context; discovery never silently picks a host or launches one. The host must be
available, declare `setup_id`, and advertise `_account.list`. If absent, configure
the Codex runtime and launch/restart an agent first. There is no automatic login
success detector or background refresh.

The picker displays only native `sessionId`, `accountId`, and `active`, with setup,
execution-host and native-host context. These are opaque references, not verified
email/workspace names. Choose a row, type your own display name, and submit once.
vqro allocates the registration ID. Existing references are shown as already
registered. No email labels are inferred and arbitrary native response fields are
never displayed. Native errors are not echoed with their private payloads.

Before saving, the host/setup/epoch/capability identity and native saved-login row
are rechecked; a changed/disappeared row or stale registry refuses. The write is
one expected-version registry transaction. No credential switch, bind, prompt or
retry follows discovery. A transaction conflict needs an explicit refresh. This
registers subscription metadata only, not proof of current login or quota.
**New agent on …** still requires that login already to be natively active.
Fake tests cover this flow; real saved-login/auth/billing interoperability remains
unverified. API-key launch/cutover restrictions below are unchanged.

Manual Codex registrations can still use the native saved-login reference:

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

## Named-launch binding audit

Named launch is allowed only where the launcher binds the credential route;
metadata alone never authorizes fallback to the shared native login. This table
covers every registry-consuming agent launcher (`pi.new`, `codex.new`,
`claude-code.new`, and the two `tier1` launchers). Raw backend spawn is not a named-account launcher. Normal
`account: normal` launches keep the shared native setup and do not claim a named
identity. Cross-runtime account IDs refuse in registry selection.

| Launcher | Runtime | Account kind | Bound / refused | Evidence |
|---|---|---|---|---|
| `pi.new` | Pi | subscription | Bound by host bridge's shipped account extension and explicit flag | `tests/test_accounts_login_swap.py::test_launch_passes_only_auth_reference_and_keeps_shared_runtime_settings`; `src/bin/vqro-bridge-pi/account_alias.rs::explicit_shipped_extension_carries_metadata_only` |
| `pi.new` | Pi | API key | Refused before launch; entry kept | `tests/test_accounts_route_integration.py::test_unbound_named_api_key_launch_refuses_instead_of_shared_login_fallback` |
| `codex.new` | Codex | subscription | Bound to exact native login reference; current native login readback required | `tests/test_codex_launcher.py::test_shared_login_maps_active_a_without_selecting_inactive_b`; `test_inactive_launch_selection_is_refused_before_empty_thread_or_prompt` |
| `codex.new` | Codex | API key | Refused before launch; entry kept | `tests/test_accounts_route_integration.py::test_unbound_named_api_key_launch_refuses_instead_of_shared_login_fallback` |
| `claude-code.new` | Claude | subscription | Refused before backend launch; shared native login remains available | `tests/test_claude_code_launcher.py::test_unbound_named_account_kinds_refuse_before_any_backend_launch` |
| `claude-code.new` | Claude | API key | Refused before launch; entry kept | `tests/test_claude_code_launcher.py::test_unbound_named_account_kinds_refuse_before_any_backend_launch` |
| `tier1.pi` | Pi | subscription | Bound by same shipped account extension before pull plugin, explicit `--vqro-account` | `tests/test_tier1.py::test_named_pi_launch_loads_native_alias_before_pull_with_explicit_flag`; `tests/pi_account_alias.mjs` |
| `tier1.pi` | Pi | API key | Refused before spawn; entry kept | `tests/test_accounts_route_integration.py::test_unbound_named_api_key_launch_refuses_instead_of_shared_login_fallback` |
| `tier1.claude` | Claude | subscription | Refused before hook creation/spawn: “account choice is not available for this launcher yet” | `tests/test_tier1.py::test_named_claude_subscription_refuses_before_any_hook_or_spawn` |
| `tier1.claude` | Claude | API key | Refused before spawn; entry kept | `tests/test_accounts_route_integration.py::test_unbound_named_api_key_launch_refuses_instead_of_shared_login_fallback` |

Tier1 Claude named binding is an approved follow-up; its ordinary shared-login
launch remains (`tests/test_tier1.py::test_claude_launch_wires_hooks_for_that_launch_only`).
The standalone keychain helper retains API-key operations and was extended with
an explicit Claude subscription-token kind; it is not wired into named new
launches. Its execution-host exec boundary preserves Windows CRT arguments.
Credential entry/sign-in is a separate explicit action, not evidence that a
subsequent launcher binds it.

`pi.resume` is not a free-form new launcher: it accepts only an acknowledged
subscription claim, consumes the protected launch intent once, and requires exact
native session/file/model/account/backend/work-principal readback. API-key resume
is unsupported. See `tests/test_pi_resume_consumption.py` and
`tests/test_accounts_pi.py::test_resume_requires_positive_exact_logical_work_principal_readback`.
The table records code/argv/environment and fake-control evidence, not live native
authentication, billing or cross-account interoperability certification.

## Capacity strategies

`doc/settings/accounts-policy` contains `new_agents`, `blocked`,
`api_key_overflow`, `continuation_text`, and `auto_cutover_max_per_hour` (0–10,
default 3; zero disables AUTO cutover). This **hourly cap remains 0–10**, while
one claim's ordered native target plan is independently capped at **three**
targets (the native fence bound). Invalid settings fall back conservatively.

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
plan (at most three targets, even with an hourly cap above three) reserves hourly
slots for **every affected native conversation**. Final projections include the
explicit current blocked mode: NOTIFY/missing mode refuses both AUTO and OFFER.
Pi/Claude owner OFFER checks sample mode again after the final blocking owner
lookup and refuse a changed mode. Shared owner continuation similarly compares
policy before/after owner verification. This closes revocation during that owner
read, not a global atomicity guarantee: ownership can change during the final
policy read, and either can change after its sample before native dispatch.
Registry/policy reads are followed by a journal revision check; a concurrent
observation or reservation invalidates the projection before native work. A definite
attempted prefix refunds unused slots; a definite pre-callback rejection settles
to zero attempts and refunds every reserved slot while keeping the episode
tombstone. Uncertainty retains the entire reservation.
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
usage limit**. Other Pi errors require an owner choice. After any eligible idle,
unqueued Pi native error, Accounts shows **Choose account for … (human-judged)**;
the agent pane menu also offers **Continue on another account… (human-judged)**.
Both open the same account choices. An unrecognised error explicitly asks whether
to switch anyway, without claiming a quota verdict or granting AUTO. The choice
is bound to the displayed native error episode; a changed error is refused.
Manual switching resumes the same conversation and then WAITS for the human's
next input: no new vqro turn and no earlier input replay. The **Account capacity
strategies** command now opens Accounts when invoked without save arguments.

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
preserves the logical tool principal/model and current thinking level. Alias
selection captures/restores the level with public native APIs rather than taking
a per-alias/shared default. Exact model/alias/API/effort readback is required by
the runtime callback and rechecked by common dispatch before input; mismatch is
unconfirmed and sends no continuation.

Current policy/authority is checked after native completion and before every new
turn intent/submission. The continuation is separately recorded and visibly
vqro/tool-origin, never attributed to the human. Operational uncertainty/full journals
refuse rather than silently forget protected attempts.

This is an experimental integration. Fake callback/worker tests do not certify
native cross-account acceptance, billing, keychain interoperability or release
readiness; those require their separate authorized native gates and reviews.
