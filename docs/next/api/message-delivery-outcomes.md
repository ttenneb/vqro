# Tasks message delivery outcomes and original-record recovery

Available in the bundled Tasks provider/CLI and Pi adapter. This extends the
existing Tasks SQLite authority and operation receipts; it adds no host privilege,
mailbox service, session-derived authority, or parallel store.

## Boundary and crash cuts

`message_claim` changes waiting → claimed and inserts a random delivery attempt
with outcome `pending` **in the same SQLite transaction as its operation receipt**.
A claim receipt is not SDK submission, execution, understanding, or acceptance.
Neither reopening the store, observing a receipt, nor seeing attention starts a
turn. Existing recorded activation policy still gates idle pickup.

| Attempt outcome | Meaning | Delivery attention |
| --- | --- | --- |
| `pending` | Claim committed; no subsequent adapter outcome confirmed in Tasks. A crash can precede or follow an SDK effect. | Yes |
| `no_effect` | Adapter rejected/discarded this contribution before calling the SDK delivery method or returning hook context. | Yes |
| `submitted` | SDK synchronous queued-input method returned without throwing. No execution or acceptance claim. | No |
| `uncertain` | SDK delivery boundary was crossed and threw, or an older boolean adapter could not distinguish failure from uncertainty. | Yes |
| `context_prepared` | Adapter prepared an awaited before-run contribution. A crash after this write can still precede hook return; no SDK-consumption receipt exists. | Yes |
| No attempt on a legacy claimed record | Historical claim, with no delivery outcome evidence. Migration invents neither an attempt nor delivery. | Yes |

A later explicit resolution clears attention without asserting delivery. A
`submitted` record can still need an explicit reply or other product attention;
transport submission is not the task's accepted outcome.

Counterexamples handled conservatively:

- Claim commit → process death → reopen: the same original claimed record and
  pending attempt remain attention, outside automatic waiting pickup.
- SDK queues input → throws, or SDK returns → process death before outcome commit:
  uncertain/pending remains attention. No automatic replay.
- Outcome commit → lost CLI acknowledgement: the existing original caller-scoped
  operation lookup recovers that receipt. Never resend the SDK input to recover a
  database acknowledgement.
- Awaited context → execution changes: adapter discards it and attempts to record
  `no_effect`. If the outcome write is unavailable, durable pending/prepared
  attention remains. Preparation never becomes execution proof.
- Original recovery races a late outcome: outcome writes must name the latest
  attempt on an unresolved claimed record. They cannot overwrite a newer retry or
  resolution. Resolution itself is not cancellation of an in-flight SDK effect.

## Inspection and explicit recovery

`vqro message list` includes delivery outcome and attention on original records,
prioritizing attention within bounded history. Tasks' existing read-only Messages
panels count unconfirmed delivery from complete bounded headers and label its
previews, including old execution contexts. Placement does not establish current
execution or confer mutation authority. There are no new executable panel actions.

Inspect a specific original record independently of list/preview truncation:

```text
vqro message get ID [--json]
```

It returns original sender, full recipient binding, body, current revision,
latest attempt, all attempts, explicit recovery history and any resolution time.
Recovery history's actor role references the immutable full original sender or
recipient binding in the same record, rather than duplicating it in every event.
Read access remains restricted to the exact original sender or exact recipient.
A replacement execution never inherits these rights.

Those same original parties may explicitly choose:

```text
vqro message recover ID resolve --rev N
vqro message recover ID retry --rev N [--may-duplicate]
```

- `resolve` records attention resolution only. It neither sends input nor claims
  delivery, execution, acceptance, cancellation, or an answer.
- `retry` makes **the same original record** waiting again. It preserves ID,
  sender, body, reply reference, creation/first-claim times, full recipient binding
  and prior attempts. A previously claimed message cannot be edited even while
  explicitly requeued. It accepts no target parameter and cannot reach a successor execution.
  Only a `no_effect` latest attempt permits retry without `--may-duplicate`.
  Pending, legacy, prepared, uncertain, and even submitted attempts require that
  explicit acknowledgement because input may already have been delivered.
- Both require the inspected current revision and reject stale revisions.
  An old execution that no longer exists stays addressed to that old execution;
  recovery does not retarget it. No retarget operation is required or provided.
- At most 32 attempts per original record bound inspection and storage. At the
  limit, retry rejects; explicit resolution remains available. Recovery history
  is retained separately in the same Tasks database, bounded by these attempts.

Adapter reporting uses `vqro message outcome ID ATTEMPT OUTCOME`, or
`vqro message outcomes PAIRS_JSON OUTCOME` for 1–8 `{id, attempt}` pairs from one
claim batch. Batch reporting is one atomic transaction and one bounded CLI
deadline, with a compact outcome receipt rather than repeated message bodies.
Only the full exact addressed recipient can report; the original sender cannot impersonate its
adapter. Terminal outcomes cannot be overwritten by another fresh operation.
`context_prepared` may progress to a discard outcome. Every outcome/recovery
mutation uses the existing caller-scoped operation receipt transaction. Explicit
`--op` requires `--caller` from the original operation, as for other message
mutations. Receipt replay returns historical committed data, not current state;
inspect `get` before choosing a new recovery.

## Adapter and migration limits

The neutral adapter's optional `deliverOutcome` distinguishes definite pre-SDK
rejection, synchronous submission and uncertainty. Existing boolean `deliver`
retains its contract. An SDK throw is uncertain even when a test double knows it
threw before queueing: production has no proof of that SDK-internal cut. Optional
awaited context `prepared`/`discard` callbacks report preparation/discard only;
Tasks persists their meaning. Failed outcome writes leave existing durable
attention, never an automatic replay job. Pi session entries are diagnostics,
not the delivery record of truth.

Tasks schema v5 adds attempts, recovery history and resolution time atomically,
including migration from v4. Original rows, bindings, policy and operation
receipts remain unchanged. Future schemas fail closed; an old provider must not
open a newer store. Old-binary rollback over v5 is not supported.

Reply headers, full-binding store authorization and monotonic follow-up
revocation remain unchanged. Existing reply text correlation is not upgraded to
causal execution proof by these outcomes. Opaque SDK delivery/run IDs and reply
heuristic redesign are separate, unavailable work.
