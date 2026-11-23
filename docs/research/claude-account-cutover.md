# Automatic per-agent Claude account cutover

**vqro switches automatically by restarting the agent on the other account; the
conversation continues.** Accounts owns selection, one protected cutover claim and
one NEW visible vqro-labelled AUTO continuation turn. It never resends old input.
OFFER/NOTIFY do not send that continuation. The callback described here restarts
only the selected endpoint through the route-1 adapter; it is not another supervisor.

## Integration status

This slice implements the callback, host-helper credential mode and fake boundary
regressions. Native `claude.restart` is a required separate route-1 adapter seam.
The callback fails closed when the agent/provider do not declare it; fake tests do
not certify native process restart, subscription acceptance or session readback.
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
revision, matching claimed source/target and declared native restart support.
Repeated events/uncertain dispatches are never automatically replayed by the caller.

Registry `host` equals the source runtime provider's authenticated core-published
`provider.host` AND the agent's top-level `host`. It is not `data.agent.host`, which
may identify a native child/server. The exact binding is revalidated before the
native operation. No handle is exported to another execution host.

## Provider-specific native operation

This is a coordinated route-1 extension seam, not a stable cross-runtime standard:

```json
{"resource":"r2","op":"claude.restart","args":{
  "v":0,"epoch":"e1","session":"native-session","account":"a",
  "episode":"<native ended limit result UUID>",
  "target":{"account":"b","ref":"<opaque32hex>","host":"r1","revision":7}
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

The adapter must also require positive native subscription-limit evidence associated
with an ended `blocking_limit` result and the same native session. The native limit
result UUID is distinct from common policy's source-episode hash. Generic auth errors,
overload or an availability label alone do not qualify.

An accepted/observed generic outcome alone is not evidence of process start: the
explicit `started` boolean means successful control initialization, not merely an
allocated pid. `requested_session` is the requested id; optional `session` is emitted
ONLY for native observation and must match that request. Account is configured B
selection, not invented authenticated readback.
A new epoch is required. A mismatch/malformed/uncertain result raises with the
original effect evidence and never returns fabricated success. Native RPC errors
retain their original data and wire evidence.

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
