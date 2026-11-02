# Read-only Tasks pane

Status: **experimental Linux trusted executable consumer**. The existing plugin
pane, real TUI mouse, exact-owner navigation and legacy-provider failure path
have been independently audited within this Linux/trusted/cooperative-human
scope. Dual-shape acceptor display and the merged graph-provider status/list/
parent-only debt refresh have separate behavioral and real endpoint/reader
composition audits. This does not claim a repeated full graph-backed TUI audit,
a frozen presentation ABI or sandboxed package admission.

The Tasks extension owns tree meaning, filters and terminal presentation. It
reads `vqro.tasks.core` through [generic action invocation](extension-action-invoke-v1.md),
never SQLite directly. The host owns the ordinary tab/pane, PTY, identities,
rendering and recovery. No task state enters the host or its client snapshots.
Python 3 stdlib `curses` is the pane's runtime dependency.

After explicitly admitting the trusted Tasks extension, open its pane:

```sh
vqro plugin pane open --plugin vqro.tasks --entrypoint tree --placement tab
# Existing split placement is also available.
```

This command does not install/admit code. Linking/enabling trusted code on a
human installation still requires appropriate approval.

## Presentation and observation

- Parent/child tree with owner links, durable state, whose move, owed action,
  recipient and UTC debt time. `owed_to` remains the debtor. Report recipient is
  the delegator, otherwise parent owner; acceptance recipient is reporting owner;
  decision-answer recipient is decision creator. The pane reads both the legacy
  debtor shape and graph `{role:acceptors, bindings}` shape. It displays only the
  provider's acceptor set, never infers one from creator/delegator/parent. Waiting
  on me means this UID's human debtor/in that set or an unanswered decision.
  UID comparison is presentation only, never authorization; bindings are not
  rewritten. Non-current agent acceptors are marked stale here, missing proof
  unknown. An empty acceptor set shows unknown; `acceptor_missing` remains visible
  as blocked attention. Global status changes refresh complete derived debts even
  when an individual child revision is unchanged. Acceptance stays provider-owned:
  current graph policy uses independent delegator/parent acceptors; legacy receipt
  policy is not rewritten. Decision answers remain human-only; human classification
  remains cooperative/unverified.
- Seven toggles: waiting on me, stalled, blocked, working, idle, done/accepted,
  old. Default attention includes human debt, stalled, durable task blockers or
  verified current owner prompts, positively unavailable owners, and acceptance
  debts with a nonempty entirely positively stale-here acceptor set (marked stale
  acceptors). Unknown/mixed-current sets are not classified stale. This adds no
  blocked taxonomy or fabricated durable blocker. Returning
  the category toggles to that default also restores unavailable-owner rows.
  A live owner prompt does not rewrite the task's debtor/action. All-tasks mode also exposes unknown owners. Ancestors stay
  dimmed as context; expansion, scrolling and age are local presentation only.
- Stalled means open/in-progress, no unfinished children, exact owner observed
  idle for at least 60 seconds. Idle age is a **view-local sampled lower bound**,
  not durable history or proof between samples. State-sequence changes, evidence
  gaps and unknown identity reset it. API status `done` means idle-but-unseen,
  not durable task completion. Accepted/done come only from Tasks records.
- Old hides only final tasks after the selected age (default seven days), never
  unfinished debts or necessary ancestor context.
- The extension's read action `{"action":"status"}` returns `revision`, the
  maximum committed transition sequence (zero for an empty store). Reads and
  receipt replay do not change it. The pane polls no faster than two seconds,
  fetches task pages on revision changes, checks revision again before replacing
  its cached tree, and backs off to 30 seconds on read failure. Pages are bounded
  by the existing Tasks contract; the pane caps snapshots at 10000 tasks/64 MiB.
- Painting uses cached facts only. Neutral agent observations are refreshed once
  per poll and expire locally after five seconds; failed/stale observations mean
  unknown, not idle or dead. Source failure labels cached data unavailable/stale.
  There is no additional durable store or host timer.

## Optional neutral execution observation

Explicit `agent.list`/`agent.get` API responses can include optional
`AgentInfo.execution`, the process-lifetime `boot-id:pid:birth` key used by Linux
and macOS caller attribution. It is present only when original owned-root birth,
known-agent and current foreground-leader ancestry/currentness can be proved.
Unprovable/handoff/platform-unsupported, budget-exhausted or failed observations
leave it absent. Absence never proves process death. A positively missing agent
or a proved different execution means only **not current/available here**.

These reads capture narrow host facts/PTY readers on the App actor, then perform
process capture and foreground/currentness checks off actor. They are limited to
eight concurrent observation jobs, 128 candidates/read and a nominal two-second
probe budget set before blocking-job scheduling; expired jobs and unprovable
roots/types skip process queries. A foreground query can finish after the budget
deadline. Normal API
response timeouts still apply. No work is added to render, detection, PTY parsing
or frame fanout, and no PID-only identity cache is introduced. Frozen generation-1
client codecs are unchanged. Older responses lacking the optional field remain
usable for durable task presentation, without live-owner navigation/filter proof.

An owner click checks the exact terminal/agent/execution binding afresh with
`agent.get` using its current pane location, then uses existing `agent.focus`.
Unknown/replaced owners do not navigate and are never silently rebound. Focus
can race after the check; it grants no task authority. Read observations and
payload bindings cannot substitute for immutable socket-peer caller attribution
on task mutations.

## Boundaries

No mutations/acceptance UI, messaging, wake, pane typing, agent activation, Pi
migration, Spaces badges or task sidebar. No generic document/widget framework,
conditional-focus method, task registry, identity credential or additional
journal/mailbox. Provider failure does not remove generic terminal recovery.
