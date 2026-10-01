# Slice2A owner-dependent freeze candidate

Freeze candidate deadline:2026-10-01 00:15UTC. Actual accepted commit/hash is recorded in the TPM exit report. **Contract only, not implemented APIs. Owners/2B wait for PM's OK after the delivered report.** Final review question: can each owner build against this without guessing?

## Normative owner inputs

1. Actor, caller, action/query, task/thread/entry and receipt shapes: `work.schema.json`. Actor authority comes from immutable host binding, not client class/role labels. Work owns graph/transitions; host does not.
2. Called neutral method names/parameters/results: `api-shapes.mjs` and generated `api.schema.json`; broker payloads in `broker.schema.json`. Existing generation1 fields/enums/methods remain unchanged. All proposed methods are separately advertised; missing means feature-local unavailable, never pane typing.
3. Decision request/result ABI: `decision.schema.json`; async `evaluate(request,{signal,remainingBudgetMs})` returns proposal, never authority. Six deterministic defaults in `decision-defaults.mjs`; hard actor/subtree permissions independent of provider/mode.
4. Authority: own-assigned-subtree agent create/assign/message/report/block; accept/reject/root/free-standing/cross-subtree agent mutations require human confirmation. Human overrides recorded. Explicit exact-recipient queues; no automatic wake/owner launch. Thread graph and task ancestry separate.
5. Output: successful exact-source persisted final reply in its thread as default **target selection**; posting still requires actor/subtree rights, so free-thread agent reply is suggest/confirm, not automatic posting. Neither turn_end nor agent_end nor settlement notification alone is final proof. Human output requires public `work.output.route.v1` consent/CAS; exact source/task/execution/recipient binding, never an adapter flag. Report remains explicit; prose is not acceptance.
6. Shared UI/action path: composer and Pi use registered Work actions above. Draft/focus/paste state stays client-local; public mutation submission durable. Public reads use `extension.query.invoke.v1` and bounded Work read/receipt results with immutable caller/read-only lease; no mutation tunnel or client journal grant. No privileged TUI/first-party namespace/private shortcut.

`docs/next/api/v1-work-contract.md` and `v1-decision-contract.md` explain these inputs. Extended failure/retention/bootstrap explanations in `v1-neutral-mechanics.md` are qualification guidance, not evidence of implementation. Do not expand2A further to specify all failure cases.

## Approved additive amendment: proposal preview

`proposal.schema.json` defines separately advertised `work.proposal.read.v1` request/result contracts, called through existing `extension.query.invoke.v1`. Operator/human actor + actual read grant, read-only, scope-filtered, <=64KiB, generation/snapshot-fenced. Existing `work.read.v1`, original Work schemas and all generation1 host codecs/methods stay unchanged. Exact chosen action/current preconditions are visible before existing human confirmation; this query grants no mutation/auto-accept/wake. Unknown proposal lookup is not an operation-receipt absence assurance. Producer→preview→valid confirmation and rejection of changed/unauthorized confirmation need independent QA before availability. Free-thread later task attachment remains an explicitV1 limit.

## Short2B/3 qualification obligation list (not2A implementation)

- Actual ordinary non-reserved service grants/invocation/recovery; peer identity/actor ticket/credential fences, unsupported platform behavior.
- Real durable append/CAS/attribution/audit/dedupe/outbox; crash/ACK-loss/cancel windows, domain retirement, immutable stamped delivery, pins/quotas/retention/export/reset repair.
- Strict production duplicate-key/canonical parsing and full Draft2020 schema validation; reference parse/metadata tests are not substitutes.
- Exact claim/run/source/execution and restart/uncertain-model-start handling; no automatic replay/wake, owner narrow receipt access.
- Work hard scope/revision/acceptance and provider timeout/revoke/stale/override races with same-CAS audit.
- Native Pi queued/retry/abort/context-edit/settled ordering, persisted final selection, tool-vs-observer dedupe, explicit human-route and stall/resume.
- Composer modal/draft/paste/direct-input behavior; rendering remains cached/pure, fixed1/15 populated panes if layout widens.
- Known possible existing flake/hang: `service_fabric::tests::provider_queue_saturation_fails_immediately_without_an_unbounded_waiter` hung in final fullCI repeat; isolated bounded rerun PASS. Logs retained;2B triage, no2A core fix.

All full production/dogfood/auth/model/cutover proof remains later gated. This freeze is the build-against surface, not every possible failure specification.
