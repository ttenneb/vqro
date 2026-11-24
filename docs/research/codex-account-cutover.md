# Codex shared-login cutover

Codex switches automatically by restarting the agent on the other account; the conversation continues.
The native mechanism unloads/reopens the same thread through Codex's saved-login switcher and `thread/resume`,
not a new conversation, history injection, fork, config-directory swap or replay of a failed prompt.

## Ownership and authority

The accounts extension owns notify/offer/auto policy, one protected acknowledged claim for a whole shared setup,
and exactly one protected NEW continuation-turn intent per affected resumed conversation. Codex never claims or
sends the continuation independently. NOTIFY only tells the human. OFFER requires an explicit human-principal
click, recorded by the common claim owner. AUTO uses common native-episode and rolling-hour caps (default3),
and may select UNKNOWN target capacity, never common-known blocked capacity. Common freezes a unique ordered
`authorized_targets` plan bounded by every affected conversation's remaining hourly budget; every dispatched
target consumes a slot for every affected conversation. No earlier input or queue is appended.

`accounts.prepare.codex` is read-only: `{resource,episode,account,_account_revision,automatic}`. Its input episode
is the common hash. It returns `setup_id`, `membership_revision`, `automatic_allowed`, `native_episode` and exact
`affected` resource/host/session/epoch/incarnation/target-account rows. The native episode is ONLY the typed limit
error's `data.agent.availability.native.turnId`, with matching thread ID and nonretrying native quota/rate class.
It never equates that native identifier with the common claim hash. Running sibling turns or approvals defer
preparation without claiming or switching; the common worker can wait read-only.

`accounts.cutover.codex` takes the same fields plus `_claim`, but effect `episode` is the returned native episode.
It validates `accounts.claim.read` and all protected source/target/setup/membership witnesses, fences every managed
native host, waits for quiescence and revalidates before any saved-login effect. Common policy alone grants
restart authority; callbacks do not interpret a configured label as native account proof.
The callback rereads common's `max_switches`/`known_blocked_targets` projection before each credential step,
without confusing those live projections with the immutable plan. It never substitutes raw persistent blocked
facts for common's reset/hour policy. Every target is selected/read back on every host, then inspected before
any continuation. All-host positive native permission yields `ready`; all-host false may walk to the next frozen
target. Unknown, failed or contradictory permission yields `offer`, never another switch. Exhausted all-false
plans yield `all_blocked`. All confirmed nonready outcomes resume/release the exact threads on the last selected
login with zero automatic model input. Common alone offers one SAME-claim human continuation or notifies.
Actual dispatched prefixes and negative inspection observations are returned for common's budget/block ledger.

## Setup consent and external clients

Each Codex runtime launch can declare a stable nonsecret `setup_id` (1–64 safe ASCII characters). Use the SAME
identifier for managed app-server processes sharing one native setup. The id is not a path, saved-login ID or
user ID. No native config/profile/credential file is inspected to derive it. Legacy hosts without a declared
setup make group membership unknown; restart through a configured launcher before cutover.

`doc/settings/codex-cutover = {setups:{id:{automatic_login_switch:false}}}` stores explicit consent. The default
is OFF. **Let vqro switch this Codex setup's login automatically (also affects Codex used outside vqro).**
The consent UI explicitly explains this consequence. Off means notify/offer, not a silent AUTO switch.

All vqro-managed hosts sharing the setup are fenced; Codex CLI/IDE clients outside vqro cannot be fenced.
One-time consent acknowledges that switching the shared native login can affect those external clients too.
It does not claim exclusive process ownership, external-client quiescence or a global atomic transaction.
Every managed host performs its OWN native switch/readback; sibling auth reload is never assumed.

## Native bridge mechanism

The bridge opts into Codex's experimental API for the native saved-login methods. All endpoints advertise
read-only `_account.list` and `_account.bind`; setup-enabled endpoints add the fenced lifecycle commands:

- `_account.bind`: verifies the active saved-login/workspace reference read-only before mapping a registered
  account ID into public facts. It never changes credentials. Unverified launcher hints are private: limits keep
  native scope but no configured account ID. A new launch requesting an inactive account is refused with
  **Codex agents share one login; switch the group instead**, before provider publication or thread creation.
  Normal shared-login launches bind the current login when its reference matches a registered account.

- `_account.list`: read-only `accountSession/list`, projecting bounded opaque `sessionId`/workspace `accountId`
  and active flags only. No email, user ID, image, token, auth URL or unfiltered native response is published.
- `_account.fence`: exact all-slot session/epoch witnesses plus operation/setup ID; acquires ALL admission guards.
  It freezes `authorized_targets`, one to three unique native login/workspace references in order.
- `_account.prepare_switch`: complete all blocking native target discovery, return a one-use native-owned
  ticket bound to the unchanged local authentication revision and retained admission fence. The callback then
  reads protected common authority and refreshes private consent/owner authority. `_account.switch` consumes
  this ticket with local revision/quiescence validation, with no later discovery before credential IPC.
- `_account.inspect`: after a confirmed selection, read native `account/rateLimits/read` between stable active-login
  readbacks. Only explicit `ordinaryUsageAllowed` classifies permission; percentages/reset timestamps do not.
  Wrong-account scope or unavailable permission is unknown. A failed quota RPC can still return unknown after
  independently confirming the selected login. No causal reset is invented (`reset_at_ms:null`).
  Only confirmed false opens the next frozen target slot; true/unknown cannot advance. Every credential phase
  is consumed before dispatch. Read-only inspection uncertainty permits independently verified lifecycle cleanup,
  never credential replay. Source plus up to three target refs may authorize lifecycle-only restoration.
- `_account.status`: read-only quiescence evidence. Native terminal events, not elapsed time, end a running turn.
- `_account.switch`: typed saved-login reference, configured account metadata; consumes phase BEFORE native effect,
  validates membership, calls `accountSession/switch` with `allowBrowserLogin:false`, then confirms active saved
  login/workspace through a separate native `accountSession/list`. Browser/SSO/error/timeout stays unknown/fenced.
- `_account.resume`: consumes phase, resumes every exact original thread ID, rejects a different returned ID.
  No `turn/start` is issued. Native auth notifications drain before confirmation; newer auth revisions invalidate it.
- `_account.restore`: lifecycle cleanup after revoked authorization, only from a known pre-switch or confirmed
  switch phase. Reads the current native login under the same before/after auth-revision fence and accepts only
  the previously observed source or intended target. Resumes the exact original threads on that actual login;
  never selects credentials, rolls back, or retries an uncertain switch/resume. An unregistered source stays
  unlabelled rather than being called B.
- `_account.release`: only after all exact resumes; emits **continued on account B** as a visible system notice,
  then releases local admission guards. The callback waits for new epochs and revisioned account publication
  before returning affected targets for a separately authorized common continuation.
- `_account.abort`: releases local guards ONLY before a native switch was attempted. It cannot cancel, retry or
  roll back a native credential effect. Actual native uncertainty remains explicitly fenced, with zero
  continuation authorization. No uncertain native switch/resume is replayed.

Revoking automatic consent after a known switch stops every further credential switch, but does not strand
conversations: restore/release all evidenced hosts on B or their original native account, with no continuation.
The callback returns `restored_only:true`, `continuation_allowed:false`, and actual confirmed accounts/new epochs
for every member. Common orchestration validates restoration before its normal target-B validator, completes the
root and clears its setup group without creating continuation child intents. Failed/unknown restoration is not
blanket-cleared. A visible notice reports the revocation, actual restored account and **no continuation sent**.

There are at most64 consumed cutover-operation witnesses per native host lifetime; exhaustion refuses rather
than evicting uncertain intent and allowing replay. Common durable episode/continuation intent survives a host
restart and remains the policy authority. Native bridge commands are mechanism, not a second policy journal.

`data.agent.account.rev` is monotonic for confirmed in-process account projections. Native auth changes clear
old configured account projection before new usage can be labelled; unknown remains unknown. A new account fact
for a cutover is installed only after native saved-login/workspace readback, not merely from the requested
account's label. Initial launcher metadata remains a configured hint, not native login/authentication proof;
initial attribution uses native active-reference verification before it can establish a quota source.
`data.agent.account.native.login_ref` records that verified nonsecret reference; consumers must match the
current registry binding, not trust a launcher label or reuse an old binding after a registry edit.
An auth notification invalidates current usage and limit eligibility to **unknown**, not recovered/available.
Known old turn causes are retired; a new native turn is required before quota/rate errors can qualify in the new
auth generation. Read-only binding alone never reattaches A's episode to B. A same-auth metadata bind preserves
its current limit. Retirement is bounded at256 known causes per host endpoint lifetime; exhaustion stays unknown
rather than evicting old causes and risking reuse. Historical A block policy remains the common ledger's concern.

## Evidence and limits

Static Codex0.170 generated `AccountSessionsSwitchParams`, `AccountSessionsResponse`,
`AccountSessionsSwitchResponse` and `ThreadResumeParams` schemas establish the request/response shapes.
The cached upstream app-server “Workspace switching (experimental)” documentation says a switch stops loaded
threads and emits `thread/closed` and `account/updated`; selecting the same workspace may leave threads running.
The public app-server guide distinguishes `thread/resume` and `turn/start`:
https://developers.openai.com/codex/app-server.md

Fake-only tests exercise native JSONL, all-slot fencing, no input replay, active-turn deferral, SSO refusal,
independent account readback, same-thread rejection, auth-revision changes, opaque-only listing, claim/group
revalidation and separate continuation ownership. They do NOT establish actual cross-account authorization or
same-thread acceptance on installed Codex0.170. Native saved-login persistence/refresh and external-client behavior
remain runtime-owned and unverified until an explicitly approved real acceptance test. No native authenticated,
model, credential-store or profile reads were performed for this implementation.
