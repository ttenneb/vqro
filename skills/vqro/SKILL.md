---
name: vqro
description: Control vqro terminal workspaces, panes, agents, repository Spaces, collections, and sessions through the CLI/API.
---

# vqro agent guide

vqro owns persistent terminal sessions. Terminals are canonical for execution/recovery, not shared work or message records. Use pane commands for ordinary processes and agent commands only after a supported coding agent is detected.

## Safety

- Read IDs from command output; never guess workspace, tab, pane, terminal, collection, or delegation IDs.
- Do not restart or stop the active session unless explicitly requested.
- Use one bounded subagent per task. Do not recursively spawn agents.
- Prefer waits and state reads over arbitrary sleeps.
- Keep Herdr and vqro routing separate. vqro uses `VQRO_*` environment variables and `vqro.sock`.

## Discover the installed interface

```bash
vqro --version
vqro --help
vqro workspace --help
vqro pane --help
vqro agent --help
vqro api schema
```

The installed binary is authoritative because alpha commands may change.

## Core workflow

```bash
vqro workspace list
vqro pane list
vqro pane split <pane-id> --direction right --cwd "$PWD" --no-focus
vqro pane run <pane-id> <command>...
vqro pane read <pane-id> --source detection --format text
```

For agents, start a bounded helper only when authorized. Exchange its own task binding first; then record assignments/results as described below. `agent prompt` is a notification adapter, not the task or evidence of acceptance.

```bash
vqro agent start <name> --kind <agent> --pane <pane-id>
vqro agent prompt <name> "Run vqro task whoami and print its caller binding."
vqro agent wait <name> --until idle --until done --timeout 300000
vqro agent read <name> --source detection --format text
```

Wait/read observes the agent, not task completion. Inspect the task and accept its evidence before closing a completed helper pane. Never close the active session.

## Shared work: Tasks, not Pi todo or chat

Pi todo stays **private scratch**; do not use its delegation/report features for shared Vqro work. Shared obligations use `vqro task`. Tasks is an experimental Linux trusted extension; if unavailable, ask for approved setup—do not silently install/link it or fall back to a private todo/chat as the shared record. The installed `vqro task --help` and [Tasks contract](../../docs/next/api/tasks-core-v0.md) define available commands.

### Propose, approve, then run

The controller proposes a plan one or two levels ahead: parent/child for decomposition/accountability, `depends_on` for prerequisites, an exact owner at each level, work/wait estimates, required `done_when`, and optional brief path. The plan file contains only `{"nodes":[...]}`; each node has a unique local `key`, `title`, `done_when`, optional local `parent`, exact `owner`, prerequisites and budgets. Copy agent owner objects from their own `whoami`, never infer them from names/panes. Existing prerequisites are read-only; parent/ancestor completion cycles reject. Import returns local-key→durable-ID mapping; use those IDs thereafter.

**The human approves and imports ordinary root plans. Agents never create/import ordinary roots.** Agents may add children/import additions only beneath an existing active ordinary task they own, not a foreign descendant, closed task or decision. Re-plan through authorized active-task updates; import adds tasks, not replacements/re-parenting. Every new task/decision needs 1–3 distinct observable `done_when` lines (≤1024 UTF-8 bytes). A brief path is display-only, not evaluated. Never manufacture human classification via shell mediation, `pane run` into a human shell, or caller/actor flags.

Below, shell variables are placeholders populated from current outputs: `PARENT_ID` is your active task, `TASK_ID` the selected child, `REV` its inspected revision, `PARENT_REV` the attachment revision, `OWNER` the assignee's own caller JSON, `CALLER` the original operation binding. `PLAN_FILE` is the proposed JSON file, `CRITERIA` its observable lines, and `EVIDENCE` the quoted report below. Commands are individual examples, **not a batch to run against one revision**.

```bash
vqro task whoami
vqro task get "$PARENT_ID"
vqro task create "Produce proof" --parent "$PARENT_ID" --done-when "$CRITERIA" --brief-path "brief.md" --estimate-work-ms 60000 --estimate-wait-ms 30000
vqro task plan "$PLAN_FILE" --parent "$PARENT_ID" --rev "$PARENT_REV"
```

Human-only root approval/import (controller proposes the file, waits for approval; never run this on the human's behalf):

```bash
vqro task plan "$PLAN_FILE"
```

### Ready, owned claim, bounded work

```bash
vqro task ready
vqro task list --after "$CURSOR"
vqro task get "$TASK_ID"
vqro task start "$TASK_ID" --rev "$REV"
vqro task depends "$TASK_ID" --on '["PREREQUISITE_ID"]' --rev "$REV"
vqro task update "$TASK_ID" --estimate-work-ms 60000 --estimate-wait-ms 30000 --due-at "2026-10-03T12:00:00Z" --rev "$REV"
vqro task update "$TASK_ID" --actual-work-ms 45000 --actual-wait-ms 10000 --rev "$REV"
vqro task block "$TASK_ID" "Waiting for required input" --rev "$REV"
```

`ready` is an observation, **not a reservation**. Start only your own task; `start --rev` records the owned claim and rechecks prerequisites. There is no separate `claim` command or auto-start. Ask the delegator/parent owner/human to delegate a foreign-owned ready task to your binding; do not take it unilaterally. Accepted/done prerequisites satisfy edges; reported does not. Parent completion also waits for children accepted/done. Follow `next_cursor` with `--after` for bounded list/ready pages (`ready` also accepts `--after`); `get`/`tree` inspect debts. Inspect again before each write—never guess or automatically refresh a stale revision to force success. Resume a blocked task with `start` only after its cause and prerequisites are resolved.

`depends --on` replaces the dependent task's whole prerequisite list; `[]` clears it. Its owner/creator/delegator/current parent owner or cooperative human may edit it while active; prerequisite ownership alone grants no edit right.

Record estimates and explicit actual work/wait while active, including before reporting; never treat wall `elapsed_ms` as CPU/tool time. `started_at` is provider-set once. At your agreed budget/stop threshold without a result, block and file a decision rather than quietly expanding scope. No automatic attention notifier is promised.

### Delegate, notify, report, independently accept

After reading the helper's own `whoami` output, record its exact binding in `OWNER`. Assignment is not proof the helper is alive. Then:

```bash
vqro task delegate "$TASK_ID" --owner "$OWNER" --rev "$REV"
```

Send only the durable task ID/pointer as the work notification; the task contains the obligation:

```bash
vqro agent prompt <helper> "task $TASK_ID assigned; inspect vqro task get $TASK_ID"
```

The helper inspects, starts its owned task, works, updates actuals, and reports **every criterion exactly once** as `> criterion` with nonempty non-quote evidence below each. Example report body (use the task's actual criteria, not invented ones):

```text
> Proof file contains PASS.
Evidence: result.txt contains PASS; checked with the stated command.
```

Quotes + evidence must fit 4096 bytes. Any line starting `>` must be an exact criterion quote; indent/prefix quoted command output or shell prompts. Presence is checked, not truth: the acceptor judges the evidence. Criteria/metadata/dependencies freeze after reporting.

```bash
vqro task report "$TASK_ID" "$EVIDENCE" --rev "$REV"
```

Then notify the delegator/lead with the task ID only; they read current task state/evidence, not a chat substitute:

```bash
vqro agent prompt <lead> "task $TASK_ID reported; inspect vqro task get $TASK_ID"
```

**Delegate/report record, do not deliver.** Notifications, waits and final prose are not independent acceptance. A delegate/report receipt records that write, not independent acceptance; an accept/done receipt can authoritatively record its historical committed outcome. Use `get` for current state. The current parent owner or stored delegator may accept a report after checking each criterion, but **agents never self-accept**, even with a role match. Read the computed `acceptors`; there is no creator fallback or unrelated-human-admin accept right. A human-owned/delegated top level needs its authorized human acceptor. Human acceptance matches UID across panes, but original operation scopes remain exact.

```bash
vqro task accept "$TASK_ID" --rev "$REV"
vqro task done "$TASK_ID" --rev "$REV"
```

The first is for an independent authorized acceptor; the second closes accepted work **by its assigned owner or cooperative human**, not necessarily the accepting agent. For a never-handoff own-step child with the same exact owner as its parent, its owner may instead start then finish directly with quoted evidence:

```bash
vqro task done "$TASK_ID" --evidence "$EVIDENCE" --rev "$REV"
```

No open/blocked/root/decision shortcut. Self-takeover preserves existing independent oversight, never erases a prior delegator. Replacement executions inherit nothing: request explicit re-delegation. For a shared task with missing/stale-only acceptors, ask the human to re-delegate the child **before** report; **after** report the child is frozen, so ask for its active parent to be reassigned to an independent acceptor. Reported orphan roots need human-approved replacement work, not fabricated acceptance/reopening.

### Human decisions and turn-origin replies

If the human addressed you directly in your pane, ask them there (Pi `ask_user_question`, if available, is one example). Otherwise record a decision—even a root decision is allowed for an agent—with criteria stating what the human must settle:

```bash
vqro task decision "Which approach is approved?" --done-when "Choice A or B is recorded." --options '[{"key":"a","label":"Approach A"},{"key":"b","label":"Approach B"}]' --default a --reversible true --waiting-cost "Implementation waits on this choice."
vqro task decision "Which approach is approved?" --parent "$PARENT_ID" --done-when "Choice A or B is recorded." --options '[{"key":"a","label":"Approach A"},{"key":"b","label":"Approach B"}]' --default a --reversible false --waiting-cost "Implementation waits on this choice."
```

Human only (never answer your own decision or impersonate the human):

```bash
vqro task answer "$TASK_ID" a --rev "$REV" --note "A is approved"
```

Decisions need 2–4 options, a default, a reversible flag and a waiting cost; a reversible decision may also take `--auto-at` (see the decision policy). The answer selects an option key and is not quote-validated. Do not assume creating a decision notifies/wakes the human. Write your final reply for whoever started your turn: when an agent's message started it, the reply-routing adapter sends that final text back to that agent automatically and records it; a human-started turn stays a normal chat. A message you send with `vqro agent prompt` is auto-labelled from your pane (`[agent <name> · reply expected]`; `--no-reply-expected` drops the reply expectation); anything not labelled as an agent message is treated as human and never routed. To reach anyone else, record a report/decision and explicitly notify them with the task ID. Keep human-needed decisions in Tasks; final prose is neither delivery proof nor a committed task receipt.

### Uncertain outcomes: lookup, never blind retry

If a write prints `outcome unknown; op ...`, preserve its printed original binding, ID, intent and revision:

```bash
vqro task op "$OP_ID" --caller "$CALLER"
```

An absent receipt is a point-in-time observation, **not proof a queued invocation cannot still commit**. Do not mint a new ID, rebind to a replacement, or change intent/revision to escape uncertainty. An authorized same-execution retry uses the original values (fresh explicit IDs also require their `whoami` binding):

```bash
vqro task report "$TASK_ID" "$EVIDENCE" --rev "$REV" --op "$OP_ID" --caller "$CALLER"
```

Read current tasks for current state; operation receipts return historical outcomes. A replacement can inspect the original receipt via `--caller`, but cannot inherit its write authority.

## Spaces and collections

Repository identity and checkout grouping are shared server facts projected as Spaces. Non-Git workspaces remain valid. Use `vqro collection --help` and `vqro delegation --help` for orchestration. A collection is an ordered set of real panes with one selected preview. Its in-pane header exposes previous/next member controls, delegation provenance, and inspection. Splitting a selected member splits the collection leaf.

## Terminal inspector

Right-click a pane and choose **Inspect terminal**, or use a collection header’s inspect control. Telemetry is sampled only on request and excludes argv and environment data. Enabled `pane_inspector` extension services may return bounded declarative contributions; vqro validates and renders them as an embedded component inside the target pane. Services are trusted same-user subprocesses, not sandboxes; protocol grants only constrain host effects. Press `r` to refresh and `Esc` to return to the terminal.

## Named-session isolation

When controlling a named session from another shell, explicitly select it and clear stale overrides:

```bash
env -u VQRO_SOCKET_PATH -u VQRO_CLIENT_SOCKET_PATH VQRO_SESSION=<name> vqro pane list
```

Never substitute `HERDR_*` variables; those belong to a separately installed Herdr runtime.
