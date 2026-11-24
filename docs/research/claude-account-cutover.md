# Automatic per-agent Claude account cutover

**vqro can restart only the selected agent with another configured credential and
request its original conversation.** Accounts owns selection, one protected cutover claim and
one NEW visible vqro-labelled AUTO continuation turn. It never resends old input.
NOTIFY never restarts anything. OFFER restarts only after the authenticated current
workspace owner clicks the explicit choice and does not send that continuation. The callback described here restarts
only the selected endpoint through the route-1 adapter; it is not another supervisor.

## Integration status

This slice integrates named headless helper startup, the callback, source-reference
binding and native `claude.restart`, including synthetic owned-process and whole-CLI
A→B regressions. The callback fails closed when the agent/provider do not declare
the operation. Fake tests do not certify real credential acceptance, billing or
native cross-account session readback.
The pinned Claude SDK does not establish a complete known-empty native queue witness;
route-1 therefore keeps `queue:null`. The approved AUTO eligibility seam instead uses
vqro's own epoch-bound input ledger and a complete native background level, below.
User-only queued counts, partial interrupt receipts and idle alone are never replaced
with native `[]`. Runtime/authentication qualification remains separate from the
synthetic producer/restart integration tests.
No real keychain, Claude login, transcript or model request is used by the tests.

## Credential boundary

The human mints a token once with native `claude setup-token`, then imports it into
the execution host's helper through hidden terminal input. Registry fields contain
only the opaque reference, account metadata and execution-host binding, never a token.
See `accounts-keychain.md` for store/delete ambiguity and OS adapter limitations.

The token is kept in the system keychain and passed to the Claude process only in
its environment when vqro starts it; it is never written to files, logs, workspace
data or command lines, but programs running as your user (or an administrator) can
read a running process's environment.

The private helper uses `--kind claude_subscription --provider anthropic`, retrieving
only on the execution host and setting `CLAUDE_CODE_OAUTH_TOKEN` in the directly
executed Claude child. No secret value goes through `resource.spawn.env`, public
RPC or the bridge. Existing API-key mode remains distinct and is not an automatic
fallback from subscriptions. This slice adds no provider refresh-token ownership.

## Named headless startup and process-scoped reference

`claude-code.new` pins `resource.spawn.provider` to the registered backend name.
The runtime authenticates its allocated host and verifies that exact host resource's
provider before helper preflight. Only child startup argv wraps the helper; saved
canonical native argv never contains A's helper, so B cannot nest/reapply A.
The bridge publishes `_claude.credential_binding:{account,host,ref,epoch}` for the
selected child. This configured reference is not an authenticated native identity.
Old-child limits/percentages stay attached to the old reference after registry
rebinding; a rebound source refuses cutover, without resetting charges, clocks or
tombstones. On Windows, named credential startup refuses before bootstrap/helper
work and `claude.restart` is not declared; ordinary shared login is unchanged.

## Callback and ownership

`accounts.cutover.claude` accepts only nonsecret selection/guard arguments:

```json
{"resource":"r2","episode":"<common source-episode hash>","account":"b",
 "_account_revision":7,"automatic":true,"_claim":"<protected common claim>"}
```

The common accounts extension MUST acknowledge its protected claim before calling
this command and is the SOLE claim/continuation owner. The callback reads the bound
claim through `accounts.claim.read {claim:_claim}`; it never registers/writes the
common operational domain or owns its internal journal schema. It
checks source identity, quota episode, current session/account, target registry
revision, matching claimed source/target and declared native restart support. BOTH
claim reads must bind `source_incarnation`, `source_epoch`, `source_account`, native
`session` and `native_episode` to the source actually dispatched. Common's episode
hash alone does not distinguish two native limit results within the same epoch.
AUTO requires protected `claim.authorization`
`{mode:auto,principal:null,owner_revision:null}`; absent disclosure defaults false ONLY for AUTO.
An explicit nested false has the same meaning; true/malformed values are refused.
OFFER requires protected `claim.authorization`
`{mode:offer,principal:<core-authenticated owner>,owner_revision,background_loss_disclosed:true}`;
there is no duplicate root disclosure flag and a caller flag supplies no authority;
the callback re-reads `doc/principals/owner` and compares its exact value/version at
both checks. The protected disclosure means the pane showed **background work may be lost**
before that human click; no caller-supplied boolean supplies authority. No NOTIFY restart is accepted. Common policy
alone owns cross-epoch native-episode dedup and the rolling-hour automatic budget.
Repeated events/uncertain dispatches are never automatically replayed by the caller.

Registry/credential `host` is the **backend provider name**, the keychain execution
domain (one backend provider per host, `docs/spec/provider-v0.md`), NOT the per-agent
runtime host resource ID. The runtime's authenticated `provider.host` must still
match the agent's top-level `host`; an exact public `resource.list` lookup of that
immutable resource supplies its owning backend `provider`. Match credential `host`
to THAT backend. It is never inferred from `data.agent.host`, which may identify a
native child/server. Routing/claim member hosts stay per-agent. Revalidate before
the native operation; legacy per-agent-host registrations refuse with explicit
re-registration guidance, without silently remapping references or clearing claims.
No handle is exported to another execution host.

## Provider-specific native operation

This is a coordinated route-1 extension seam, not a stable cross-runtime standard:

```json
{"resource":"r2","op":"claude.restart","args":{
  "v":0,"epoch":"e1","session":"native-session","account":"a","allow_background_loss":false,
  "episode":"<native ended limit result UUID>",
  "target":{"account":"b","ref":"<opaque32hex>","host":"backend-name","revision":7}
}}
```

Under its admission fence the adapter must compare those source/session/account
and process-generation guards, stop ONLY its owned Claude child, observe exit,
start its preconfigured private helper once with `claude --resume EXACT_SESSION`,
and report native evidence. It knows nothing about common claims, account policy
or continuation text. It must not silently attach another process, fork history,
rerun an interrupted turn, replay queued input or retry uncertain lifecycle effects.

Required result data:

```json
{"started":true,"requested_session":"native-session","resume_confirmed":false,
 "account":"b","epoch":"e2"}
```

The adapter and callback require positive retained native subscription-limit evidence
associated with an ended `blocking_limit` result and the same native session. Claude's
availability `blocked` state is turn-scoped and clears on the terminal result; an idle
agent with that retained episode still qualifies. The callback does not require
`availability.state == blocked`, and an idle agent with no positive episode never
qualifies. `eligible_episode(row, automatic=True)` is a pure shared scheduling predicate
returning the native episode UUID or null. Both callback source/claim boundaries use it.
It requires explicit `state:idle` plus `data.agent._claude.input_ledger`
`{v:0,source:"vqro-ledger",epoch:<same epoch>,complete:true,pending:0}`. This ledger is
vqro's own input reservations not terminally correlated, including unresolved old epochs;
unknown completeness/pending or an epoch mismatch refuses. It is not a native queue snapshot.
AUTO additionally requires `background:[]`, published ONLY after a complete actual native
`background_tasks_changed tasks:[]` level. Its provenance is bound by
`_claude.background_level {v:0,source:"native:background_tasks_changed",process_incarnation,revision,complete:true}`:
positive integer revision, exact current `data.agent.host.incarnation`, native source and complete flag are required.
This native level is process-scoped across conversation epochs; the producer generation-fences its continuous
observer and invalidates completeness on observer loss or process replacement. Before a level is known, source
is null/revision0; unknown/nonempty/unprojected levels remain incomplete. Missing/nonempty/malformed background is unknown
and refuses AUTO. Required cut/malformed `incomplete` evidence also refuses; unrelated cut
fields do not disable the operation. Native `queue:null` remains unchanged and is not read
as empty. With protected OFFER disclosure the callback passes `allow_background_loss:true`
and may accept unknown/running background work; AUTO always sends false. The adapter with
false independently refuses unknown/incomplete/running background work with a reason such
as `background_work`, while true permits its restart. The adapter never interprets claims.
The producer MUST invalidate the retained `availability.native` episode on ANY newer
native turn start or terminal result, an allowed-rate observation, or epoch/incarnation
change. Thus presence denotes the latest qualifying terminal result, not a historical
limit. A start/cancel/idle sequence must not resurrect an earlier episode. This is a
required producer guarantee, not a new inferred field; native restart admission also
fences latest work. Native producer qualification is separate from these fake tests.
The native limit result UUID is distinct from common policy's source-episode hash.
Generic auth errors, overload or an availability label alone do not qualify.

An accepted/observed generic outcome alone is not evidence of process start: the
explicit `started` boolean means successful control initialization, not merely an
allocated pid. `requested_session` is the requested id; optional `session` is emitted
ONLY for native observation and must match that request. Account is configured B
selection, not invented authenticated readback.
A new epoch is required. A mismatch/malformed/uncertain result raises with the
original effect evidence and never returns fabricated success. Native RPC errors
retain their original data and wire evidence. Target selection also uses the shared
registry route validator and rejects known equal credential references under different
public account IDs; labels or distinct IDs are not evidence of independent quota.

The callback normalizes an `affected` member with `resource`, authenticated `host`,
`requested_session`, configured `account`, new `epoch`, `process_started`,
`session_confirmed`, `resumed` (true ONLY when confirmation is true), and optional
native-only `session`. It preserves the original effect result and a safe display notice.
Without native same-session confirmation the notice is:
`resumed session <id> on account B (not confirmed)`.
Per the explicit product decision, AUTO may submit its one NEW continuation after
successful process start even while same-session confirmation remains unknown.
Common policy owns that send and the subsequent `continued on account B` notice;
this callback does not send input or reinterpret requested session as proof.

A rejected restart, keychain failure, unobserved old-child exit or uncertain process
start is not a reason to start another process or repeat a continuation. The pane
stays available for native recovery. Session organization restrictions are reported,
never bypassed by transcript modification or a new conversation.
