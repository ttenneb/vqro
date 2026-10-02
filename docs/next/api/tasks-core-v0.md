# Tasks core

Status: **experimental Slice 1, Linux trusted executable only**. This documents
working code, not a frozen task ABI or a production package admission claim.

`extensions/tasks/` owns task policy, persistence and derived debts. The host
owns terminal/execution identity and neutral
[action invocation](extension-action-invoke-v1.md). No task state, task registry,
acceptance policy or activation policy lives in `AppState` or the TUI. Tasks use
that same public service path as independent trusted executables.

## Available consumer

Enable the trusted local `extensions/tasks/vqro-extension.toml` service using the
existing explicit plugin link/enable mechanism. Its service is `vqro.tasks.core`,
protocol 2, capability `action_invoke`. Python 3/SQLite are prerequisites; no
additional Python dependency is needed. Linking trusted code is an effect and
requires appropriate approval on a human's real installation.

Thin CLI commands:

```text
vqro task whoami
vqro task create TITLE [--parent ID] [--op ID]
vqro task decision QUESTION [--parent ID] [--op ID]
vqro task delegate ID --owner JSON --rev N [--op ID]
vqro task start ID --rev N [--op ID]
vqro task block ID REASON --rev N [--op ID]
vqro task report ID REPORT --rev N [--op ID]
vqro task accept ID --rev N [--op ID]
vqro task done ID --rev N [--op ID]
vqro task answer ID ANSWER --rev N [--op ID]
vqro task list [--after ID]
vqro task tree [--json]
vqro task get ID
vqro task op OPERATION_ID [--caller JSON]
```

An explicit `--op ID` on any write requires `--caller JSON` containing the
**original attempt's binding**. For a fresh explicitly chosen ID, obtain it with
`task whoami`; without `--op`, the CLI mints an ID and captures that binding.

Use the intended assignee's `task whoami` **caller binding**, not a pane ID or
agent name, as `--owner`. Assignment chooses an exact binding; it does not prove
that target is still alive. Every state/assignment write requires the inspected
revision. No automatic read-and-update, retry, activation, or messaging occurs.

The extension's action payloads correspond to these commands: `action`,
`operation_id` for writes, `id`/`revision` for changes, `text` for
block/report/answer, `owner` for delegation, and `title`/optional `parent` for
creation. Optional `expected_caller` is a **precondition**, never authority.
Reads use `after` for list and `lookup_caller` for explicit receipt scope.
Unknown actions/fields, including actor flags, reject.

## Authority and transitions

Agent bindings contain `class`, durable `terminal_id`, `agent` and process-lifetime
`execution`; optional session metadata does not own work. Human bindings contain
`class: human_unverified`, `terminal_id` and `uid`. Unknown callers can read, but
cannot mutate. Caller bindings are derived from immutable host observations.

Agents may create roots and children beneath their currently owned, active
ordinary tasks. They may start/block/report/done only their owned tasks.
Delegation/reassignment also permits the stored creator/delegator or current
parent owner. Cooperative human callers may administer tasks. Replacement
executions never silently inherit work; reassignment is an explicit revisioned
transition. Parent IDs cannot be changed. Cyclic ancestry and closed/decision
parents reject new children.

| Action | Source | Destination / debt |
| --- | --- | --- |
| create | — | open; owner owes start |
| start | open, blocked | in_progress; owner owes work/report |
| block + reason | open, in_progress | blocked; owner owes unblock |
| delegate/reassign | open, in_progress, blocked | open; selected owner owes start |
| report + text | open, in_progress, blocked | reported; human owes accept |
| **human accept** | reported | accepted; no outstanding debt |
| done | accepted | done; no outstanding debt |
| decision | — | open decision task; human owes answer |
| **human answer + text** | open decision | done; no outstanding debt |

Report requires every child to be accepted/done. Decisions use the same task,
transition and operation tables, with a logical human-role owner. They close
only by answer, never ordinary start/report/accept/done. Accept/answer reject
direct agent callers. **Human classification is cooperative and unverified**:
shell-mediated agent commands can impersonate it. This is not a same-user
security boundary or verified human-presence check.

Each task exposes immutable creator, parent/kind/title, current owner/delegator,
state/revision, creation/update times, latest transition caller/time and bounded
report/block/answer detail. `owed_to`, `owed_action`, `owed_since` are derived
from the current task facts; they are not separately persisted. Acceptance and
done both discharge debt. Older operation outcomes remain historical records,
not refreshed projections of current tasks.

## Durable outcomes and reads

The store defaults to `$XDG_STATE_HOME/vqro/extensions/vqro.tasks/tasks.sqlite`
(or the corresponding HOME state directory). It uses SQLite WAL,
`synchronous=FULL`, foreign keys, one writer transaction at a time, and
`BEGIN IMMEDIATE`. Task + transition + operation outcome commit atomically
**before** success reply. COMMIT/ROLLBACK uncertainty, including failed cleanup,
must not become a definite rejection. These guarantees rely on normal SQLite
and filesystem durability; power-loss hardware behavior was not simulated.

Operation IDs are scoped to exact caller bindings. Replay with identical intent
returns the original result; a changed payload conflicts. Receipt lookup without
an explicit selector uses the current caller. Absence is not authoritative for
an unselected scope if another caller's same ID exists. `--caller` explicitly
selects the original scope, including from an unknown/replacement execution;
it grants no mutation authority. Absence is a point-in-time store observation,
not proof that a previously queued invocation cannot still run.

The CLI captures a read-only `whoami` preflight and never silently retries.
Explicit operation IDs must carry the original `--caller`; a replacement caller
fails before any write instead of selecting a new dedupe scope. The original
binding is also sent as the extension's write precondition, catching drift
between preflight and dispatch. Lost/malformed/in-flight replies print
`outcome unknown; op <id>`, the original binding and its lookup command. Inspect
that scope first. A retry must retain the **same operation ID, same original
caller, same intent and still-valid revision**. Do not mint a new operation ID
or rebind to a replacement to resolve uncertainty. A preflight per invocation
is not permission to retry across principals.

Limits: 10000 records per table, a 262144-page SQLite database limit (normally
1 GiB at the default page size; not a WAL/disk-space quota), 1024-byte
title, 4096-byte detail, 128-character operation ID, 32 rows/page and 128 KiB
provider result budget. Quotas reject new writes; they never evict receipts.
List pages use ID cursors and byte bounds. CLI tree assembles pages and projects
extension-supplied debts, with bounded indentation and escaped controls; it is
not an atomic multi-page snapshot. A changed/malformed parent graph fails the
read rather than fabricating hierarchy.

## Evidence and boundaries

`extensions/tasks/demo.py` runs a real private Vqro server and two real,
model-free Pi processes: delegation/report, direct-agent acceptance rejection,
cooperative-human acceptance/decision answer, debt tree, disconnected reply
inspection/replay, exact provider/server restart reads and explicit replacement
reassignment. It requires already-installed tools and creates a new private
HOME/config/state/runtime/socket root, verifies its config canary and stops only
recorded owned processes. It never installs into a human session.

`just task-core-test` covers store policy, receipts, SQLite subprocess crash,
concurrency, cleanup uncertainty and byte bounds. Generic worker barrier tests
and `cli_fault_demo.py` characterize dispatch/CLI failure mechanics with fixture
executables/replies; those fixtures are **not** host/task acceptance evidence.

Not included: Spaces badges, task pane/sidebar/filter UI, Pi todo migration,
observation publishers, secure human authentication, non-Linux support,
credentials/tickets, separate journals/mailboxes, package-fabric task calls,
activation/wake policies, or a frozen task contract. Provider failure removes
only task authority; host-owned terminals and generic recovery remain usable.
