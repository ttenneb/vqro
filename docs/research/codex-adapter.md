# Codex app-server adapter: initial declaration (2026-11-19)

The initial declaration preceded implementation on `agent-standard`; the binding contracts are now `docs/spec/{agent-v0,coordination-v0,agent-v0-conformance}.md` on main. This is not a conformance claim. Adapter binary: `vqro-bridge-codex`; explicit runtime argv, no default installation or credential discovery. Shared SDK item observation is implemented; certification still requires current-head gates, conformance and separately approved native smoke.

## Protocol evidence

Source inspected at openai/codex `5bdb5e8b3c4b43e4a10e53f2f8a3813d318d1f9c` (2026-11-19), principally:
- `codex-rs/app-server/src/request_processors/turn_processor.rs:581–812`: `turn/start` uses `start_or_steer_turn`, so its name does **not** promise reject-when-busy.
- Same file `:1150–1292`: `turn/steer` requires `expectedTurnId`; mismatch/no active turn rejects. It does not establish an after-tool-calls delivery point.
- Same file `:1564–1659`: interrupt targets a specific turn; the inspected build replies when it completes/aborts. We initially declare only acceptance, and completion independently from `turn/completed`, for compatibility with the installed runtime to be tested.
- `codex-rs/app-server-protocol/src/protocol/v2/{turn,thread,item}.rs`: native thread/session IDs, foreground status, user inputs, item updates, approvals. Native request IDs are connection-scoped; standard request identities must be fresh within the observation epoch.
- Official app-server documentation: https://developers.openai.com/codex/app-server/ . Existing research snapshot: `agent-protocol/src-codex-app-server.txt` in coordinator scratch.

The installed I0 build may differ. A real no-model handshake and smoke must record its version/schema behavior before native precision is promoted.

## Modes and provenance

| Mode | Native / composed / unsupported | Busy edge | Idle edge | Evidence / reason |
|---|---|---|---|---|
| `admit` | native (`turn/start`, or guarded `turn/steer`) | native admission/steering, timing unspecified | native admission/start | acknowledgement is `admitted`, not insertion |
| `start` | unsupported initially | cannot promise native atomic idle rejection | — | inspected `turn/start` can steer; do not advertise a stronger contract from its name |
| `boundary` | unsupported | no evidenced standard boundary/batching | — | `turn/steer` is not proof of after-tools timing |
| `settle` | unsupported | no native settle queue declared | — | coordination may hold until idle under its own named policy |
| `preempt` | unsupported in adapter | — | — | compositions belong to coordination, never hidden here |
| `interrupt` (operation) | native `turn/interrupt` | targets current native turn | no-op if native facts prove idle | `scope:foreground`, `pending:unknown`, `waits:none`; acknowledgement is not endpoint end |

No composed delivery mode is declared. A caller-supplied run guard uses the native `expectedTurnId` path even for admit; an unavailable/unknown run rejects before writing. Session guard is adapter-serialized. There are no native insertion/idempotency key guarantees in the initial declaration.

## Endpoint / session / host

One explicitly launched app-server child serves a bounded number of logical endpoint slots (default one; configurable up to 200). Each slot is one stable resource/ref with zero or one current native thread. `agent.session new/resume` changes that slot's session and observation epoch, not its resource identity. One app-server host ID is shared by all slots. Native thread IDs are not placement IDs.

The adapter owns that app-server child: only an observed child exit or native endpoint-end notification provides end evidence. Losing the core connection does not stop the child; fresh provider registration resyncs current facts without replay. A bridge disconnection by itself never proves the app-server exited. Closing stdin is explicit bridge shutdown; it never substitutes an idle completion for endpoint end. Process startup and cleanup use portable child-process APIs, not signal-number contracts.

## Initial capability subset

- `caps.host`: `kind: server`, `endpoints: many`; facts qualify the actual app-server host with provider, instance and incarnation. All slots share that identity; a new child gets a new incarnation even when the user supplies the same provider label.
- `agent.send`: admit; `content {types: [text], shape: ordered}`; literal JSON text (`always`). Attribution includes the effective `principal` and `actor`, in a separately framed user-content block plus the untouched original text blocks, never system/developer authority. Only declared `attributed_user` rendering is available; incompatible `render` lists reject. Quoted authors are visibly separate claims. Native acknowledgement correlation only (`acceptance`); no insertion claim from matching content/queue order. Native keys `null`; session guard `adapter`, run guard `runtime` when requested. At least 256 bounded recent receipts; lookup misses return unknown, never authorize a retry. Qualified causal links remain asserted; no native provenance is manufactured.
- `agent.interrupt`: foreground only; kept/cleared pending input unknown; `waits:none`.
- Sessions: new/resume/list; no prompt on resume. Fork/close/delete initially undeclared until their lifecycle/history semantics are evidenced. Session changes reject while foreground admission/work or requests are in flight.
- Requests: representable command/file approvals as typed permission choices, and structured native user questions as a constrained form. Declared scopes are session/run, with `background_requests: true`; native pending requests survive unrelated foreground completion until the runtime resolves them. Responses are dispatched (JSON-RPC responses have no acknowledgement); first valid answer claims the request once. Unknown/incomplete requests are rejected/cancelled once, never approved.
- Observation: current facts and bounded item journal, fenced per-viewer snapshots/continuation through C3's item-stream API. Pending request/state entities survive transcript pruning. Unknown native item variants retain bounded native data rather than inventing an outcome. Large entities are explicitly incomplete; no blob service is claimed.
- Omitted: config, commands, native queue, history/fork positions, children controls, background cancellation, auth, native idempotency and stop. Optional omissions are visible, not claimed as implemented.

## Optional reported view metadata

The installed I0 Codex0.170 schemas (`v2/{ThreadStartResponse,ThreadResumeResponse,ThreadTokenUsageUpdatedNotification,TurnStartedNotification}.json`) establish these mappings:

- Native response `cwd`, `model`, `reasoningEffort` → read-only `session.cwd` and `session.model {id, effort?}`. Missing values stay absent; no configure capability is implied. Oversized identifiers/paths are omitted, not truncated into a different identifier.
- `tokenUsage.modelContextWindow` → `usage.context.window` only. No occupancy is inferred from `last.totalTokens` or session totals. Explicit cumulative `total.inputTokens/outputTokens/cachedInputTokens/cacheWriteInputTokens` map to the corresponding token counters; missing counters are not zero-filled. Native `inputTokens` includes cached input: `input` and `cache_read` overlap and must not be added as disjoint counts. Run `status` preserves the native vocabulary. No price, cost, local observation time or native format is invented.
- Native Turn `startedAt/completedAt` are Unix **seconds**; reported values become run `started_ms/ended_ms` with checked multiplication. Missing, negative or overflowing times remain unknown. Item timestamps are a different native millisecond field, never confused with these turn fields. An active timed run is protected from transcript eviction; completion can release that protection.
- Session changes clear old reported settings, usage and run metadata. Session facts are core-confirmed through the public sync acknowledgement before an observed new/resume response. The fence uses the original deadline: under load an already native-observed change may be indeterminate to the caller, never permission to retry.

Snapshot leases conservatively fund an epoch replacement before releasing the old cut. Under pressure a viewer can close as overloaded during a session switch; it is not a guaranteed-open promise. Worst-case framing charges allow about five maximum-size cuts in the provider's8MiB snapshot budget. Viewers must back off/report incompatible streams as the shared contract requires.

## Integration dependencies

C3 owns the generic SDK item-stream channel and shared bounded item journal/snapshot producer; this branch will not build a duplicate or modify `provider_client.rs`/core stream sites. SDK parity `6fc9de1` is integrated; the bridge reuses `rpc::Peer` and Provider registration/resume data. C2 owns generic principal/effect attribution. K1 owns the conformance runner. Tests here use a scripted fake app-server with a native input ledger, including core reconnect, late receipt, slots/session identity and child-exit behavior. Live-model smoke is separately approved/logged after I0 and native subscription login. This declaration is revised only with explicit evidence, not to make a fake pass.
