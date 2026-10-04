# Agent task and communication conventions

Status: **shipped instructions; experimental Tasks policy**. The agent-neutral
`skills/vqro/SKILL.md` is embedded in the binary and printed by `vqro --skill`.
[Tasks core and graph](tasks-core-v0.md) owns the command/policy contract; this
convention introduces no host API, Pi tool, task delivery or activation mechanism.

- Pi todo is private scratch, not shared delegation/reporting. Shared work and
  evidence are Tasks records. The controller proposes a bounded plan with exact
  owners, observable criteria and work/wait budgets. An agent creates/imports an
  ordinary root only after human direction/approval in conversation or a human-
  answered task decision; otherwise propose a decision first. This is a convention,
  not an API approval gate. Its exact creator/execution is recorded and only a
  host-classified human accepts its reported root. Child acceptance and owned
  active-subtree attachment rules remain unchanged.
- Ready is an observation, not a reservation. Inspect owner/revision/prerequisites;
  start only owned work. Request explicit delegation for foreign-owned ready work.
  There is no new claim command. Reports quote each criterion with evidence;
  independent authorized parent owner/delegator checks it and accepts. Never agent
  self-accept; own-step direct completion is the documented bounded exception.
- Delegation/reporting **record, do not deliver**. Obtain bindings from the helper's
  own whoami. `vqro agent prompt` is human-only: an agent-classified caller is refused,
  and agents exchange bindings and carry ID-only assignment/report notifications with
  durable `vqro message send`. Notifications point to the authoritative
  task; they do not replace work/results, prove delivery/acceptance, or become a
  durable message by themselves.
- Durable addressed messages have their own record: `vqro message send/list/claim/edit`
  over the same trusted Tasks extension, with the sender taken from the host-observed
  connection rather than a text label. Messages are editable until claimed, survive
  restarts, and reach Pi through the Tasks Pi consumer/neutral adapter at a turn boundary, never
  by pane typing. A claim is not a run, reply or acceptance. One recorded activation
  setting may start an idle recipient's turn; a fresh Tasks store defaults it on and only
  a human can change it. A stopped agent is never relaunched.
- Human-needed choices go in decisions unless the human addressed the agent
  directly in its pane. Pi's question tool is only an example for that direct case;
  the convention is agent-neutral. Creating a decision does not promise a wake or
  notification. Never exploit cooperative human classification through a shell.
- Final prose is written for whoever started the turn. A turn started by another
  agent's durable message records that final text as a durable reply message to the
  exact sender binding, with `reply_to` when one message started the run; a delivered
  reply is never re-answered automatically, so a thread continues with an explicit
  `message send --reply-to`. Nothing is typed into a pane. `vqro agent prompt` is
  human-only and its old auto-labelled reply routing is removed. A human-started turn
  is never routed. Remote action still needs the corresponding
  task record and an explicit agent notification. Final
  prose and delegate/report receipts are not independent acceptance; accept/done
  receipts may authoritatively record historical committed outcomes. Read `get`
  for current state.

The skill includes inspect-before-write revisions, quoted-evidence bounds,
computed acceptors/stale recovery, exact replacement/reassignment, budget updates,
paging and original-op/caller uncertainty lookup. It deliberately teaches no
automatic retry, caller rebinding, creator acceptance fallback or hidden routing.

## Assurance boundary

Example guards parse the shipped task command examples using the current CLI
parser; a no-server CLI test verifies `vqro --skill` prints the exact embedded file.
The existing private Vqro/two model-free Pi demo supplies fresh retained command,
identity, task-state and receipt evidence. This **$0 model-free proxy** proves
shipping, syntax and real task transport/authority, **not model understanding or
ID-only notification comprehension**. Real model use is deferred to dogfooding;
no model request/credential access or claim of model-driven acceptance is included.
Combined task-view/runtime integration remains separately accepted work.
