# Agent task and communication conventions

Status: **shipped instructions; experimental Tasks policy**. The agent-neutral
`skills/vqro/SKILL.md` is embedded in the binary and printed by `vqro --skill`.
[Tasks core and graph](tasks-core-v0.md) owns the command/policy contract; this
convention introduces no host API, Pi tool, task delivery or activation mechanism.

- Pi todo is private scratch, not shared delegation/reporting. Shared work and
  evidence are Tasks records. The controller proposes a bounded plan with exact
  owners, observable criteria and work/wait budgets; the human approves/imports
  ordinary roots. Agents add work only in their owned active subtree. Root decisions
  for the human are allowed without fabricating an ordinary root plan.
- Ready is an observation, not a reservation. Inspect owner/revision/prerequisites;
  start only owned work. Request explicit delegation for foreign-owned ready work.
  There is no new claim command. Reports quote each criterion with evidence;
  independent authorized parent owner/delegator checks it and accepts. Never agent
  self-accept; own-step direct completion is the documented bounded exception.
- Delegation/reporting **record, do not deliver**. Obtain bindings from the helper's
  own whoami. Existing agent prompt/read/wait can exchange that binding and carry
  ID-only assignment/report notifications. Notifications point to the authoritative
  task; they do not replace work/results, prove delivery/acceptance, or constitute
  a new durable message lane.
- Human-needed choices go in decisions unless the human addressed the agent
  directly in its pane. Pi's question tool is only an example for that direct case;
  the convention is agent-neutral. Creating a decision does not promise a wake or
  notification. Never exploit cooperative human classification through a shell.
- Final prose is written for whoever started the turn. The Pi reply-routing adapter
  sends an agent-started turn's final text back to that agent by default, records it
  in the session and labels a missing reply on the pane. `vqro agent prompt`
  auto-labels from the sending agent's pane (`[agent <name> · reply expected]`,
  `[agent <name>]`); anything not an agent label is treated as human and a
  human-started turn is never routed. Remote action still needs the corresponding
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
