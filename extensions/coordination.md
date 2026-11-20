# Default coordination extension (`coord`)

Implements the coordination-v0 policy on public primitives; Python stdlib and `vqro_ext.py` only. This is a replaceable extension, not native agent/runtime policy. No terminal typing, process supervisor, automatic uncertain-effect replay or physical suspension.

## Dependencies and recovery

- C1 `operational.register/get/snapshot/apply`: one owner-bound domain `coord`, requested 4 MiB (C1 qualified 9,000/9,000 commits at 100 txn/s at this size; 8 MiB did not pass that load), no workspace fallback. Existing domains may grow but are never silently shrunk.
- C2 flat core callback fields `author, actor, principal, initiator, label, invocation`. Authority comes from the callback's effective principal, never arguments or a label. The live `invocation` proof propagates on writes/effects; it is never an `on_behalf_of` principal.
- C3 `resource.attach` item streams are optional read-only receipt observation: one bounded viewer, four opens per window, persisted cursor before continuation. Gaps reset this viewer, never replay input. Model text, coarse completion and other entities never become delivery receipts.
- C4 `resource.forget_cached` is cache-only, never native stop. Its actual identity-cache capacity remains marked unknown until exposed by the core.

Commands and subscriptions register before asynchronous operational bootstrap. Missing core support leaves commands visibly unavailable. A readable old snapshot remains stale/read-only even while registration or recovery mutation is blocked. Raw recovery remains usable. Owner/incarnation restart closes admission gates and turns unresolved claims into unknown; no native effect is repeated.

## Commands

All commands are `command.invoke` / `vqro run coord.<name>`. Every mutation needs a caller-chosen `op` prefixed by the current `coord.inbox.window` epoch plus `:`. `inbox.send` also needs a similarly prefixed `submission` scoped to that principal. Identical retries inspect original results, including well-formed validation/grant/quota refusals; changed intent conflicts. Retrying after a new grant needs a new op. Malformed/undeclared calls consume no slot. With no reply slot the window closes and returns busy before evaluation. Outside-window IDs never create new mutations.

- `inbox.send`, `edit`, `withdraw`, `get`, `list`, `peek`, `op`, `resolve`.
- `scope {action: inspect, scope}` returns preconditions; `scope {op, action: pause|resume, scope, expect}` atomically selects gates. Scope names: `workspace`, `project:<id>`, `team:rN`, `agent:rN`. Results contain at most64 targets; follow `scope {action: result, receipt, page}` over the frozen selection. Capacity256, including200-agent scopes. A newly discovered unassigned endpoint invalidates an old workspace preview too.
- `control {op,target,action,args}`: declared native interrupt/respond/queue/session/configure/command/object/stop or administrative retire. No content FIFO. Stop closes admission first; host kill requires an exact declared provider route and explicit host confirmation.
- `names`, `grants`, `membership.set` produce ordinary attributed configuration writes. Handles resolve once; public grant issuance pins aliases to exact principals/targets. Membership is an acyclic project/reporting forest; no parentage grants rights. Undo changes membership, not earlier scope authorization.
- `handover` freezes an inspected pending-delivery list (maximum58) and recorded ordinary stages. `action:stage, plan, stage` advances each checked CAS stage; `action:complete, plan` releases only after every stage succeeds. Claimed/unknown work stays with the predecessor; no destination gate opens automatically.
- `inbox.archive {op,agents,destination,deliveries?}` freezes a bounded inspected set (maximum48 records). Then explicit `action:export, archive` writes and syncs a new content-hashed file without overwriting any existing different bytes; `action:free, archive` frees only unchanged source versions after readback. Late changed evidence stays in place. `action:inspect` shows the plan. Source evidence is never silently evicted on quota; missing/tampered archived files are unknown, not no-effect. An active claim cannot be archived before it settles or becomes unknown.

A gate is inbox admission state only. Native turns, queues and retries may continue while “inbox paused.” Pause/resume have fresh explicit opposite controls, outside global undo; they cannot undo external effects. Unknown predecessor ordering blocks only its sender/target/lane, not every queue. Native stale rejection latches review-required even for follow-current-session input; resume, new session facts and retries cannot clear it. An explicit versioned edit can.

## Tier-1 pull

The endpoint declares `pull {points,insert,evidence}` in its facts (or a designated terminal marker's `pull`). Launcher/configuration grants its actual plugin principal `receive` for the exact endpoint. A plain terminal without a plugin remains manually usable and holds its mail.

1. Get a current operation epoch.
2. At consumer load, call `coord.inbox.reconcile {op,agent,consumer,records:[{delivery,entry?}]}`. A new consumer fences the old one. An actual durable entry gives inserted evidence only if that integration declares `evidence:entry`; otherwise old claims become unknown.
3. At a real declared `turn|settle|idle|tool` point, call `coord.inbox.take {op,agent,consumer,at,limit,accept:[block types]}` (limit1–16). `accept` has1–16 distinct bounded names; omission is conservative text-only. Unsupported records stay unclaimed with `unsupported_content` for that consumer; eligible later input may progress. Missing/unknown points reject. Paused gates return no content.
4. Insert each returned record, framed with its `origin`. Context insertion requires the sender to accept `render:["context"]`; it is not silently a user message.
5. `coord.inbox.report {op,delivery,claim,consumer?,stage}` advances the real pull claim only. `inserted` needs declared entry evidence; `supplied` is context/no-entry; `released` is only an uninserted pull claim. Native dispatcher claims cannot be released or reported through this API.

Lost take replies are inspected with `inbox.op`: repeats return claim history, **never content to insert again**. The protected record preserves the content and attempt for explicit reconciliation. Wake publication is count-only, tagged with control epoch/revision; a hint is never authority to start work or bypass a new gated take.

## Bounds and evidence

Default domain4 MiB; sender-target quota8 open/128 KiB; sender and target quota64 open/512 KiB each. Content32 KiB. At least10% domain control reserve, plus explicit pending outcome and tombstone reservations. Outcomes persist as compact per-step evidence≤1 KiB (kind/stage/operation/size/hash and `incomplete:true`); recipes reserve up to3 steps. Full native results appear unchanged only in the live caller's reply; a historical lookup is explicitly compact. Pages≤64 items/256 KiB, scopes≤256, pull batches≤16. `coord.status` publishes the effective limits, not an unqualified throughput claim.

Two-KiB tombstone summaries are reserved from first coordination state and paged so200-target scope selection remains one atomic write. Known-ended identity is sticky. Native input has at most eight independent target workers and one in-flight attempt per endpoint. Final history retains256 compact records plus a bounded1024-entry settled index; current-window submissions stay indexed even after history compaction. Unresolved attempts, incomplete pull claims and frozen archives are not silently pruned. Operational evidence and gates remain outside global undo.

Focused tests:

```sh
env -i HOME=$HOME PATH=$PATH python3 -m unittest discover -s tests -p 'test_coordination_*.py'
```

These fake-store/native-ledger and RPC-glue tests are component evidence. They are not public-core Part B qualification, real runtime certification, combined-load control latency or the full release gate. C1/C2/C4 integration and K1 Part B remain required before merge. The contract stays draft until different real implementations qualify it.
