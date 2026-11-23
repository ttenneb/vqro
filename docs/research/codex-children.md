# Codex child and plan observations

Codex 0.170 native observation, not a child supervisor. `experimentalApi` stays false.
The parent endpoint's bounded item stream carries native child facts and scoped
messages/tools/requests. This does not add a second inbox or permission to control
children. `agent.child` declares observation with `items:["scoped"]`, no actions.
Child data never grants coordination authority. No model call was used in tests.

## Mapping

- Native `thread/started.thread.parentThreadId` identifies a child whose direct parent
  is already associated with this endpoint. Native source depth, nickname, role and
  model are reported when present. A fork is not a child. An unrelated thread is not
  adopted. Conflicting/cyclic lineage does not rebind a child.
- `collabAgentToolCall` remains a normal tool with native payload. Receiver IDs and
  reported agent-state maps update child observations. Only `spawnAgent` establishes
  direct lineage; wait/send/list references alone do not prove direct parenthood.
  Completing a collab tool does not mean its child completed.
- `subAgentActivity` reports the referenced child/thread, path, kind and model when
  given; it is not fabricated into a background task. Activity without lineage leaves
  the child `parent` field absent. Unknown native states map to `unknown` and retain
  `native.status`. Idle loaded threads are waiting, not finished. Turn completion does
  not prove child-thread termination; a completed collab child is not undone by idle.
- All main and child items/deltas use a shared injective namespace: `n:` followed
  by URL-safe base64 of a JSON pair `[native_thread_id,native_item_id]`. Pairs too
  long for the 128-byte entity-ID bound use compact epoch-local `l:` IDs instead.
  Native strings cannot impersonate a generated child or root item ID. Child items
  carry `child` tags in the parent stream. Requests are ordinary requests with `child`
  where applicable, retaining the native thread in params; observation scope/response
  epoch is the parent conversation and subject references use the mapped tool ID.
  Responses use the original native request ID once. Explicit native resolution must
  match both the recorded native request ID AND thread before it changes a request.
  Parent epoch change or native host end fences them; no request replay.
- Native child token-usage reports stay on that child, not the parent's usage total.
- Whole `turn/plan/updated` maps to a per-turn `plan` list: `items`, `update:"whole"`,
  `source:"native"`, optional explanation, unknown status preserved on its item.
  Empty lists replace, not append. Child lists use `owner` and remain distinct from
  main-thread plans. No guessed step IDs or progress derived from text.
- Native `thread/goal/updated` maps a distinct `goal` entity (`current`, or child ID
  with `owner`), never a made-up plan step. Active/paused/blocked/completed/unknown
  retain native status; usage/budget limits map to blocked without erasing the reason.
  Native budgets, usage and update time are reported when present. `goal/cleared`
  replaces the goal with unknown status and `native.cleared:true`, not completion.
  These are event observations, not an initial goal query or goal-control capability.
- Session listing explicitly includes interactive and native subagent sourceKinds.
  It uses no experimental parent/ancestor filter and is not a complete descendant
  inventory or a promise to reconstruct child history after reconnect.

## Bounds and recovery

Associations live inside the parent's native-session epoch: at most 256 children and
4096 compact mappings for unusually long native-ID pairs, reset on session replacement.
Ordinary reversible IDs do not consume that map. IDs/threads are at most
128 bytes; native child payload fields are bounded. Whole plans accept at most 256
steps, bounded text and total entity bytes. Excess/unrepresentable observations are
omitted, not claims that native children are absent. The ordinary tail-coverage item
journal can evict old entities; active child facts are protected like active tools.
No history/session scan, credential inspection, synthetic child process or new
background polling exists here. Restart recovery never resends work.

## Deliberate exclusions

Queue APIs, experimental parent/ancestor discovery filters and canAcceptDirectInput
need `experimentalApi:true`; they are not exposed. Experimental proposed-plan deltas
are also deferred under that opt-in boundary (the exact 0.170 declarations describe
those deltas as experimental but lack the machine marker present on queue APIs).
There are no `agent.child` actions, no queue-to-task fiction, and no emulated child
endpoint: child items are scoped, not a made-up resource ID.

Protocol evidence: exact release rust-v0.170.0 / commit
29eb79e9866b3262a777fdf8195c1b9e63684c91 in openai/codex,
`codex-rs/app-server-protocol/src/protocol/{common.rs,v2/thread_data.rs,v2/item.rs,v2/turn.rs,v2/thread.rs}`.
Goal timestamp seconds are evidenced by `codex-rs/ext/goal/src/tool.rs:518–519`
(state DateTime converted via timestamp before protocol conversion), not inferred
from a field name.
Native capability gates are attributes, not guesses from method names. Tests
`codex_children.rs` and `codex_children_wire.rs` use synthetic JSON and an in-memory
Peer only; native availability and provider behavior remain unverified.
