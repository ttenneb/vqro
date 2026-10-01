# V1 asynchronous decision-provider ABI candidate

**Slice 2A proposal, not runtime authority.** Work policy/providers are extensions. Host supplies generic authenticated invocation/deadline/cancellation and audit mechanics, not named work decisions or task interpretation. Providers cannot commit work merely by returning a proposal. The work provider validates current actor/revision and applies through the shared public action path.

## ABI

Request contract `vqro.decision.request.v1` has point, request ID, scope ID, input revision/digest, policy revision, explicit apply/suggest mode, bounded immutable context/input and budget milliseconds (1–5000, default250). `remainingBudgetMs` is a read-only function returning host monotonic remaining milliseconds; `signal` aborts on deadline/revocation/cancellation. It is not a client wall-clock or a budget extension. Provider registration advertises supported point contracts, provider ID/version/generation, capability and backend. Provider API is asynchronous `evaluate(request, {signal, remainingBudgetMs}) -> Promise<result>` from V1 onward; process/component protocols use versioned JSON with the same semantics. No raw private socket or model-specific field is required.

Result contract `vqro.decision.result.v1` echoes request/point/scope/input/policy identities and includes provider ID/version/generation, typed proposal, rationale code, bounded explanation (<=1024 UTF-8 bytes), evidence IDs (<=16), and outcome. Provider identities are host/registry-bound, not accepted solely from returned JSON. Unsupported point/result/version affects only that decision feature. Producer schemas describe known V1 values; consumers map unknown future output to unsupported/no-op rather than disconnect.

Work constructs context from canonical state + immutable host caller lease. Clients cannot supply actor class, assigned-root scope or source thread correlation as trusted decision context. A directly queried preview cannot become authorization. Before applying, revalidate source actor/execution, namespace/provider generation, source input/policy revisions and explicit mode; the authoritative journal CAS is the final fence.

## Points / deterministic V1 defaults

| Point | Proposal type | Deterministic default / rationale |
| --- | --- | --- |
| `recipient.routing` | exact recipient ID or null, delivery=`queue` | honor explicit registered exact actor (available, offline or execution-unknown held queue); missing/ambiguous/unregistered/retired → unresolved, never infer a pane/name; `explicit_recipient` / `recipient_required` |
| `message.triage` | priority + disposition | normal/FIFO queue; no interrupt/filter/wake; `queue_default` |
| `task.next` | task ID or null | oldest eligible active unblocked task assigned to actor, stable assignment-sequence then task-ID tie-break; otherwise none; `oldest_eligible` / `no_eligible_task` |
| `report.acceptance` | recommendation accept/reject/review + report reference | review only; no automatic acceptance/rejection; `human_review_required` |
| `action.confirmation` | auto_apply or suggest_confirm, action reference | human explicitly invoking own authorized action → current checked action; agent create/assign/message/report/block in assigned subtree → auto_apply; accept/reject, cross-subtree/root/free-standing/unknown/service mutation → suggest_confirm; `human_explicit` / `agent_own_subtree` / `confirmation_required` |
| `turn.output_route` | thread_reply / explicit_human / unrouted + exact target references | explicit verified human route when requested; otherwise exact claimed source thread/version and sender; absent/ambiguous/outcome-error/changed run → unrouted; `source_thread_reply` / `explicit_human_route` / `unrouted_output` |

Agent assigned roots and task ancestry come from current canonical work state, with acyclic/unique graph validation and bounded candidate counts; labels/role claims never grant rights. Own-subtree report/block are typed message operations, not acceptance. Approval policy is already human-approved. An action needing confirmation yields a durable suggestion; it is not executed under the provider's service identity to bypass the requester.

## Apply / override / timeout semantics

- `suggest`: may record proposal/provenance, but no consequential task/entry/delivery mutation. Human confirms by explicit public action with current revision/reason.
- `auto_apply`: proposal is still only a proposal. Authorized work provider revalidates current state and uses the same actor-recorded journal/action/outbox path once. A model/provider cannot directly write to mailbox/work or mark acceptance.
- Audit/provenance is included in the same authoritative journal CAS as any consequential effect/outbox intent. If audit storage is unavailable or quota-full, apply is blocked; no effect followed by fictional later audit.
- Hard approved actor/subtree policy is checked independently of provider choice; a structurally valid `auto_apply` answer cannot authorize agent acceptance/rejection/resume or cross-subtree reassignment. Queue-only is the only V1 triage disposition; there is no interrupt/filter confirmation path.
- Each resolution records point/request, selected provider/version/generation, mode, input revision/digest, policy revision, typed proposal, rationale/evidence, outcome/fallback cause, applying actor and commit/override reference. Failed provider selection is recorded too.
- Human override has its own operation ID, current revision and explicit reason. It supersedes a current suggestion; later provider completion cannot undo it. Policy changes increment revision and invalidate outstanding evaluation.
- Timeout, exception, cancellation, disabled/revoked provider, malformed proposal, wrong echoed identity, stale revision or unknown target: deterministic fallback (or no-op/manual review) and recorded cause. Evaluate off render and delivery critical paths; bounded invocation/queue quotas apply.
- Deadline is enforced by host monotonic invocation budget; provider wall-clock timestamp cannot extend it. Results are checked both on completion and before commit. Cancellation before commit prevents apply; cancellation after persistence begins yields receipt reconciliation, not fictional rollback.

## Future model compatibility, no model critical path

V1 contains deterministic provider only, with async ABI/registered capabilities. A future model-backed provider can evaluate asynchronously and return suggestions on the same interface; it never delays durable delivery to an explicit recipient, receipt/claim, direct typing or compatible servers. A late suggestion cannot reroute an already enqueued message or replace its consumed version. It may inform a future revision/action after explicit policy validation. Default always remains available if the provider is offline/slow. Model request/spend/auth policy is separate, not enabled by this ABI.

## Reference and required proofs

`contracts/work-v1/decision-defaults.mjs` is an inert reference provider: pure deterministic default computation, asynchronous return, no transport/model/storage side effects. `applicationPlan` and `currentResult` are inert shape/fence/policy examples, not committers. Their commit proposal still requires live host actor lease, actual current snapshot/CAS, same-transaction audit and matching source/claim validation in production. Reference fixtures do not implement broker auth/journal/mailbox or prove final Pi routing. Schemas/examples are consumer contracts, not production activation.

Required acceptance tests: all six point defaults; forged actor/subtree/context rejected by work authority; suggest causes no effect; auto_apply still CAS/actor checked; provider timeout/revoke/cancel/wrong request/input/policy/generation → deterministic recorded fallback; stale next-task/acceptance and human-override races; no-model delivery under slow provider; lookup unknown commit instead of resend; exact thread/run/source version final routing vs human explicit route; no source context/abort/error → unrouted; queued/multiple tool continuations and explicit reply tool dedupe. Own provider/package worker grants/generation tests apply identically to non-reserved trusted publishers.
