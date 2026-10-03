# Tasks legacy prompt-reply transition

Status: **accepted, transitional**
Decision: owner approval2026-10-03, conveyed by the coordinator with the core-neutrality design approval.

## Decision and exact scope

The Tasks Pi consumer temporarily preserves the existing automatic final-reply behavior formerly implemented in `extensions/pi-reply-routing`: a run started by the existing self-declared `[agent NAME · reply expected]` prompt label may send its final textual reply through `vqro agent prompt NAME ... --no-reply-expected`. Explicit successful sends suppress the automatic reply; absent final text or failed identity/delivery retains the existing no-reply observation. The existing bounded reply and no-recursion behavior remain.

This is the recorded approved policy for **that existing legacy reply path only**. It does not authorize automatic replies to every durable claimed message, activation from facts/receipts/status transitions, or a new routing path. Durable message pickup and idle activation continue to obey Tasks' existing policy; activation off does not wake an agent for a waiting durable message.

Workflow interpretation, recipient selection, explicit-send detection, reply limits, polling and no-reply labels belong to Tasks. The neutral Pi adapter only reports bounded observations and performs explicit SDK input delivery; it neither recognizes reply expectation nor routes replies.

## Limits and authority

The prompt label is self-declared and spoofable in the cooperative same-user boundary, not authenticated sender identity or an exact execution lease. Legacy direct prompt delivery is not a durable Tasks message record or evidence of accepted work. Existing name-lookup lifetime limitations are not repaired or strengthened by moving this path. Transport/admission, agent execution and accepted outcome remain distinct.

This named exception preserves compatibility during the core-neutrality move; it does not redefine text injection as the message of record. It adds no host method, caller-classification exception, capability grant, task store or frozen-wire change. Generic session/PTY recovery remains host-owned and independent of Tasks availability.

## Exact removal condition

Remove this exception and the legacy label-driven forced-prompt reply path when routed replies are recorded through **Tasks `message_send` records with exact recipient bindings**, and delivery/activation is governed by an **explicitly approved recorded policy** rather than unconditional prompt injection. The replacement must preserve operation/caller uncertainty recovery, independently test activation-off behavior, stale execution and explicit-send suppression, and distinguish durable recording from queued input and accepted outcome. That behavior-changing replacement requires a separate owner-approved design.
