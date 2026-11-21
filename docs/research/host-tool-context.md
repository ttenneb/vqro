# G11 opt-in host tool context (2026-11-20)

Implements batch7's shared-host branch only: two endpoints' tool CLIs bind one verified core-assigned host
principal, never guessed endpoint principals. No `agent.tool_principal: env`, endpoint rights, native delivery,
security boundary, token persistence or replay guarantee is created. Providers own launching tools and must NOT
export the map to human shells/services. Ordinary SDK/UI calls remain undeclared unless deliberately configured.

## Small mechanism over existing primitives

- Runtime `provider.register {host_context:true}` opts in. Default false; backends rejected. A fresh opaque256-bit
  credential is returned only to that registration, never core snapshots/history/state. It is NOT the native
  bridge bootstrap/resume credential; only the existing stateless randomness helper is shared.
- `hello {host_context:token}` verifies registration generation, observed host identity/resource lifetime and
  managed-extension lifetime; core derives host:<rid>. Cannot combine another selected identity or drop the
  lifetime on a bound connection. Raw hello.identity host assertions remain refused.
- Reuses health/resource watches and the existing extension Caller lifetime lane, generalized as extension OR
  host. Admission, serialized commit preparation/irreversible writer admission, callback/native dispatch all
  fence it. Operational namespace ownership remains extension-only. Once writer/native work is admitted,
  retirement does not relabel success or replay anything: report the existing truthful outcome lane.
- Rust/Python provider SDK opt-in +tool_context_env returns secret+explicit socket, cleared socket marker and
  principal=undeclared, removing inherited human-owner identity. Explicit host_context_hello helper; only CLI's
  tool entry selects that route absent another configured principal. No generic SDK ambient token adoption.
  Core-start/extension/native backend services scrub the variable; tmux services already use env_clear.

## Meaningful retained evidence (not acceptance yet)

1. Baseline4df8 native runtime opt-in returned no credential (red), corrected private native core test passes.
2. A pre-tool-CLI image (built16:43 with the core prototype but unchanged generic CLI) SHA256
   4740cad7494ab64a9feff26d21e5cb7e43a4eaf35a0359919813eef36bb1efd5 audited actual CLI writes undeclared (red).
   New public Rust SDK environment +REAL CLI children for2 declared endpoints audit the same host:r1 (green),
   both endpoint env capability and undelegated endpoint rights absent. This isolates CLI adoption, not a claim
   that the partial baseline image was an accepted release.
3. Provisional admission-only guard allowed a queued pre-writer mutation after host retirement (real red).
   Carrying the common lifetime through commit Plan fences it; the regression also confirms a writer already
   admitted before retirement is adopted/reported normally, and new registration rotates the credential.
4. Wrong/raw/stale tokens, old bound generation, opt-out export, ordinary SDK, human-shell spawn env, public
   snapshot/workspace secret absence, and no implicit endpoint delegation are checked. A real PRIVATE herdr-core
   auto-start/user shell additionally proves ambient host credential is scrubbed (native opt-in test passes).
5. Rust276library+22focused integration tests (2ignored in those suites), plus the selected native scrub test;
   Python3 explicit helper/export/handshake tests pass, zero warnings. Fixture/API typos and compile failures
   retained separately, not counted as functional reds. Original tokens are fixture-only, no live mutation.

## Post-merge clarification

The bridge connection, when it adopts the default host:<rid> identity, and its tool children share ONE principal
undo scope. Default undo may therefore select the bridge's write or another tool's, as the shared-host contract
requires; actual_author distinguishes connections, not endpoint ownership. Explicit transaction recovery remains
available. No new tool actor or per-endpoint env capability is invented. After rotation/re-registration old maps
fail forbidden and the producer exports a fresh map for NEW tools; it never replays an uncertain call.

All scratch evidence under prim-slice1-host-context. Whole final-candidate gate, independent review and K1 PartB
reprobe remain pending; no harness gap closure or live install is claimed by focused tests alone. Names/semantics
sent to spec owner for batch7 consistency review. No install/network/model calls or live sessions touched.
