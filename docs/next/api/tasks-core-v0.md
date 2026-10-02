# Tasks core and graph

Status: **experimental, Linux trusted executable only**. Working extension policy,
not a frozen task ABI or a production package-admission claim.

`extensions/tasks/` owns persistence, graph/acceptance policy, budgets and derived
debts. The host owns terminal/execution identity and neutral
[action invocation](extension-action-invoke-v1.md). There is no host task registry,
planner, messaging, activation or render-time graph computation.

## Available consumer and CLI

Link/enable the trusted local `extensions/tasks/vqro-extension.toml` using the
existing explicit plugin mechanism. Its service is `vqro.tasks.core`, protocol 2,
capability `action_invoke`; Python 3/stdlib SQLite are prerequisites. Linking trusted
code on a human's real installation requires approval.

```text
vqro task whoami
vqro task create TITLE --done-when TEXT [--parent ID] [metadata options]
vqro task decision QUESTION --done-when TEXT [--parent ID] [metadata options]
vqro task plan FILE [--parent ID --rev N]
vqro task depends ID --on JSON --rev N
vqro task update ID --rev N [metadata options]
vqro task delegate ID --owner JSON --rev N
vqro task start ID --rev N
vqro task block ID REASON --rev N
vqro task report ID EVIDENCE --rev N
vqro task accept ID --rev N
vqro task done ID --rev N [--evidence TEXT]
vqro task answer ID ANSWER --rev N
vqro task list [--after ID]
vqro task ready [--after ID]
vqro task tree [--json]
vqro task get ID
vqro task op OPERATION_ID [--caller JSON]
```

Every write accepts `--op ID --caller JSON`. An explicit operation ID **requires
its original binding**; obtain it with `whoami` for a fresh chosen ID. Otherwise
the CLI mints an ID and captures the binding. Writes inspecting existing tasks
require the inspected `--rev`; an attached plan requires the parent's revision.
No automatic read-and-update, retry, activation or messaging occurs.

`--owner` is the assignee's own `whoami` caller binding, not a pane/name. Assignment
chooses an exact binding, not proof of liveness. Caller facts are host-derived:
agents have `class`, `terminal_id`, `agent`, process-lifetime `execution`; humans
have `class: human_unverified`, `terminal_id`, `uid`. Optional session metadata
is not ownership. Unknown callers can read, not mutate. `--caller` is a scope
selector/precondition, never an authority flag.

The additional read-only `{"action":"status"}` returns the maximum committed
transition `revision` for cached consumers; it changes neither tasks nor receipts.
Graph metadata, dependencies and parent reassignment advance that global sequence.
See the experimental [read-only task pane](tasks-view-v0.md).

## Hierarchy, dependencies and readiness

Parent/child means decomposition/accountability. `depends_on` means prerequisites
between any tasks, including decisions. `depends` **replaces** the dependent's
prerequisites with a JSON list of durable task IDs (empty list removes them).
Dependency/active-metadata edits use the dependent's current owner, creator,
stored delegator, current immediate parent owner, or cooperative human authority;
prerequisite ownership grants no right to edit another task.

Ready = ordinary task, **open**, all prerequisites **accepted or done**. Reported
still waits. Manually blocked/running/reported/final/decision tasks are excluded.
Open means not currently started; an explicitly reassigned task can be ready
again without resetting its first-start timestamp. Reads expose `depends_on`,
`waiting_on`, `ready`; list/ready are ID-cursor and byte-bounded pages, not atomic
multi-page snapshots. Readiness is an observation, not a reservation; start
rechecks it in the mutation transaction. `tree` projects hierarchy/debts, not a
critical-path UI.

Start (including blocked→start), report, answer and done reject unmet dependencies.
Parent completion also requires all children accepted/done. Cycle checks combine
explicit prerequisite precedence with **child-before-parent completion**; a child
cannot depend on its ancestor, nor can cross-branch dependencies form a completion
cycle. Parent edges do not themselves gate readiness. Checks are iterative and
bounded by store quotas; no render/process work or automatic scheduling is added.

## Authority, acceptance and own steps

Ordinary root **create/import is human-only**. A controller proposes a plan file;
the cooperative human running `task plan FILE` is its approval. Agents may create
children/import additions beneath an existing active ordinary task they currently
own, never attach to an existing foreign-owned/closed/decision parent. Newly
planned descendants may have explicit owners. Root decisions remain agent-allowed.
Parents cannot be changed. Replacement executions never inherit ownership or
acceptance; re-delegation is an explicit revisioned transition.

| Action | Source | Destination / debt |
| --- | --- | --- |
| create | — | open; owner owes start |
| start | open, blocked | in_progress; owner owes work/report |
| block + reason | open, in_progress | blocked; owner owes unblock |
| delegate/reassign | open, in_progress, blocked | open; selected owner owes start |
| report + quoted evidence | open, in_progress, blocked | reported; independent acceptor owes accept |
| accept | reported | accepted; debt discharged, prerequisites satisfied |
| done | accepted | done; debt remains discharged |
| own-step done + quoted evidence | in_progress | done; debt discharged |
| decision | — | open decision; human owes answer |
| human answer | open decision | done; debt discharged |

Acceptors are the **current immediate parent owner or stored delegator**; a human
may also accept human-owned work. An agent **never accepts its own task**, even
if another role matches. A human stored delegator may accept a root delegated to
an agent; an unrelated human has no global accept right. Human accept authorization
matches the same UID across panes; **receipt scope, expected caller, stored bindings
and transition callers remain terminal-exact**. Agent matches remain execution-exact.
Decision answer stays human-only, not ordinary accept/report/done.

An own step is an ordinary child whose exact owner equals its current parent's
owner and has no independent delegation. Its owner can finish directly from
in_progress with `done --evidence`; open/blocked/root/decision shortcuts reject.
Self-assignment/takeover preserves the prior delegator (including none for a never-
handoff step); it cannot erase existing independent oversight or make A→B→A
handed-off work an own step again. A shared task still reports for independent
acceptance. Reassigning an active parent can restore a replaced child acceptor.

Reported debts expose the acceptor binding set, not a fabricated waiting-on-human.
An empty set is `acceptor_missing`. Exact stale bindings are not automatically
replaced or declared alive; a nonempty set can still contain only stale bindings.
For a handed-off child, human re-delegation while active can establish a human
delegator. Once reported, the child is frozen: reassign its **active parent** to an
independent acceptor instead, without changing the child's owner/receipts.
Reported orphan roots with no usable acceptor have no
automatic reopen/adoption path; new work needs a human-approved replacement root.
Human administrative re-delegation of active tasks remains available. There is no
creator fallback for acceptance. A new independent handoff records its actual new
delegator, including at a root; it does not fabricate permanent human oversight.

**Human classification is cooperative and unverified.** Shell-mediated agents can
impersonate it; this is not a same-user security boundary or human-presence check.
The root/own-step/same-UID rules are reversible product choices, not authentication.

## Plain criteria, brief and timing

Every new ordinary task, decision and imported node requires `done_when`: **1–3
distinct nonempty observable lines, at most 1024 UTF-8 bytes**. Trailing whitespace
is trimmed per line; leading whitespace is significant. It is plain text, not an
executable test/schema. Optional `brief_path` is one line, at most 512 bytes, no
control characters: display only, never opened/resolved/executed by the extension.

Report (and own-step done) quotes each criterion **exactly once**, in any order,
with a nonempty non-quote evidence line before the next quote/end:

```text
> File contains the expected result.
Evidence: result.txt contains PASS; checked with the stated command.
> Regression passes.
Evidence: named regression completed successfully; log is at the brief path.
```

Evidence lines beginning with `>` must be known criterion quotes; prefix or
indent shell prompts/quoted output used as evidence instead.

The existing **4096-byte detail limit includes quotes and evidence**. Presence is
validated, not truth; the authorized acceptor judges the result. Decisions require
criteria describing what the answer must settle, but human answer text is not
quote-validated. Metadata/criteria/path/dependency edits freeze after reporting;
there is no reject/reopen action for changing reported criteria.

Metadata options on create/decision/update:

```text
--done-when TEXT --brief-path PATH
--estimate-work-ms MS --estimate-wait-ms MS
--due-at ISO8601 --actual-work-ms MS --actual-wait-ms MS
```

Milliseconds are explicit nonnegative integers ≤2^53−1 (not bool/fractional);
missing estimates/actuals are unknown, not zero. Due time needs a timezone; empty
CLI due/path clears that optional value. `started_at` is set once by the provider
on first successful start, retained across block/resume/reassignment. `elapsed_ms`
is derived wall time from first start to now, or final updated_at for accepted/done;
clock-skew negatives clamp to zero. It is not CPU time or inferred work/wait actuals.
No attention notifier or critical-path computation is included.

## Atomic plan import

A plan file contains only `{"nodes": [...]}`. Nodes have local `key`, `title`,
`done_when`, optional local `parent`, exact `owner`, `kind: task|decision`,
`depends_on` (local keys or existing durable IDs), and the metadata above. Local
keys cannot collide with existing durable IDs. Omitting parent attaches a node
to the CLI attachment parent, or makes a new human-approved root. Default owner
is the importer; decision owner is always the logical human role. Existing tasks
may be referenced as prerequisites, **never edited/re-parented by import**.

```json
{"nodes":[
  {"key":"root","title":"Verify result","done_when":"Child proof is accepted."},
  {"key":"proof","parent":"root","title":"Produce proof","done_when":"Proof is recorded."}
]}
```

One BEGIN IMMEDIATE transaction validates current attachment authority/revision,
whole-batch quotas, fields/refs/owners/cycles, inserts tasks/transitions/edges and
commits one compact **local-key→ID mapping + counts receipt**. Rejection rolls back
the entire graph. Commit/rollback uncertainty remains outcome-unknown, never safe-
to-retry rejection. Limits: **100 nodes/400 explicit edges per import**, 400
prerequisites per task, existing **64 KiB request** (also bounded by normalized
provider serialization) and **128 KiB result**. Counts are upper bounds, not a
promise that maximal text fits. No new host method/store/controller flag exists.

## Durability, migration and uncertain outcomes

The extension store is `$XDG_STATE_HOME/vqro/extensions/vqro.tasks/tasks.sqlite`
(or HOME's state directory): SQLite WAL, synchronous=FULL, foreign keys, one writer
transaction, task/transition/edge/operation outcome committed atomically before
success. Quotas: 10000 rows per table, 262144 database pages (normally 1 GiB;
not a WAL/disk quota), 1024-byte title, 4096-byte detail, 128-character op ID,
32 rows/page. Whole-batch/net edge quotas reject writes; receipts are never evicted.
Normal filesystem/SQLite durability is assumed; hardware power loss is not simulated.

Additive v1→v2 migration preserves task identities, owners, revisions, transitions
and **historical outcome JSON/hash**. Legacy criteria stay explicitly missing;
new reports need an authorized active-task update first. Legacy accepted rows
still close by accepted→done; reported rows remain inspectable. Do not invent
criteria/acceptors or rewrite receipts to repair orphan roots. Future DB versions
reject before migration. Old-binary rollback over v2 is not validated.

Replay is scoped to the **exact original caller**, before new policy/criteria/quota
checks, and returns the historical outcome, not current task state. Identical
intent returns the original mapping/result; changed intent conflicts. Explicit
receipt lookup is available from unknown/replacement executions and grants no
write authority. Absent receipt is a point-in-time observation, **not proof a queued
invocation cannot still commit**.

Lost/malformed/in-flight replies print `outcome unknown; op <id>`, original binding
and lookup command. Inspect `task op ID --caller JSON` first. A retry retains the
same op/caller/intent/still-valid revision; never mint a new ID or rebind to a new
execution to resolve uncertainty. CLI permits missing criteria only on an explicit
historical-op replay; a fresh missing-criteria write still rejects at the provider.

## Evidence / boundaries

`just task-core-test` covers migration/rollback, original receipts, quotas,
concurrency, combined completion cycles, evidence, budgets, readiness and exact
acceptance. `extensions/tasks/demo.py` drives a real private Vqro server + two
model-free Pi processes: human-approved import/replay, ready→independent reports
→accept→ready, own-step evidence/takeover, denied premature start/cycle/import/
self-accept/answer, decision answer, original-scope lost reply, provider/server
restart and explicit replacement, plus a separate public-CLI 100-node/400-edge
import measurement. It checks config canary and birth-checked cleanup;
no installs, credentials or model requests. Elapsed wall time is refreshed; durable
restart fields and historical receipts remain exact. `cli_fault_demo.py` uses fixture
replies for CLI mechanics only, not host/task acceptance.

The experimental [task pane/filter consumer](tasks-view-v0.md) is separate from
this core acceptance evidence. Not included: critical path/badges/decision queue
UI, skill conventions, Pi todo integration, secure human authentication,
non-Linux acceptance, package-fabric tasks, messaging/wake/planner infrastructure
or a frozen contract. Provider failure removes task authority only; host terminals
and generic recovery survive.
