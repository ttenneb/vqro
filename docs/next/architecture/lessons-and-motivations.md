# Lessons and motivations

Status: direction

This note records why Vqro exists, what earlier work taught us, and which directions those lessons point toward. It complements the [agentic multiplexer direction](agentic-multiplexer-direction.md). Like that note, it guides proposals and is never itself an implementation mandate. A lesson that becomes binding belongs in an accepted architecture decision.

## Motivations

- **Agentic work needs its own environment.** People now coordinate several agents, tools, and automations at once. Closed agent products each impose one workflow, one provider, and one hidden coordination model. Vqro should make that structure visible, replaceable, and owned by the user.
- **Terminals stay real.** Existing agents and tools already run in terminals. Vqro should organize them without wrapping them in reconstructed chats or opaque jobs, and the user should always be able to reach the real process.
- **The multiplexer participates.** Beyond hosting agents, Vqro should help direct work, context, communication, and action among them: a person addresses intent to Vqro, and Vqro helps decide where it goes. Intelligence is part of the experience, but it is replaceable and bounded by user authority.
- **Defaults are experiments.** A useful built-in workflow matters for daily use, but the durable shape of human-agent work is not yet known. Extensions should be able to replace the default workflow, not only decorate it.
- **The first user is a solo power user.** The measure of progress is whether one person can rely on Vqro for substantial daily multi-agent work.

## Lessons from building the extension platform

- **Authority can move out of the host safely.** Collections and Spaces moved to sandboxed packages through staged steps: frozen contracts, a shadow comparison against the built-in behavior, and then atomic cutover. Package failure closes that package's semantic authority without a native policy fallback; generic terminal access and recovery remain usable. Behavior-preserving handover beats incremental dual ownership.
- **Name every exception.** First-party packages currently receive reviewed exact-pin admission. That is acceptable only while it is named, documented, and paired with a removal condition. Hidden privilege is the failure mode.
- **Facts keep one authoritative owner.** When one package forwards host facts to another, the host must bind the forwarded value (for example by digest), not trust the forwarder by convention. Inventories derive from durable declarations, not from whichever runtime slots happen to be ready.
- **Volatile events are not state.** An event stream cannot promise an authoritative resnapshot unless the provider has a recoverable state model. Durable addressed messages and volatile observations need separate mechanisms.
- **A product consumer should precede new infrastructure.** Extension health diagnostics exposed requirements that generic service-call fixtures had not: preserving host observations through an untrusted consumer and counting declarations independently of runtime readiness. Its full product acceptance remains separate from those implementation lessons. Event infrastructure was paused because its proposed source could not reconstruct authoritative state after restart; a concrete product and recovery model must justify the next design.
- **Shared runtimes need explicit isolation design.** Sharing an execution engine across packages changes timeout and interruption semantics; it is a redesign, not an optimization.
- **Verification should match risk.** Exhaustive review of low-risk slices slowed user-visible progress. Freeze a small verification plan per change and spend depth where effects are hard to reverse.

## Lessons from agent-coordination prototypes

These come from deliberately rough experiments. Their designs are evidence, not code to port.

### Messages

- **Delivery is not acceptance.** Transport, queueing, pickup, execution, and accepted outcome are different states. Honest receipts that say only what happened were the most valuable property.
- **Messages need their own record.** A durable, per-recipient queue made waiting work recoverable across agent restarts, sleep, and server restarts. That alone does not prove delivery or exactly-once execution. Typing into a terminal is a delivery adapter, never the record.
- **Waiting messages should be editable.** Agents flood each other with stacked, outdated instructions. Keeping one waiting message per sender and topic, and updating it in place until pickup, removed most of that noise.
- **Every message records its delivery path and outcome separately.** Recording the method, reason, and editability made fallback visible, but did not make the paths equivalent. A message awaiting typed delivery must not already appear delivered. A crash between recording an attempt and observing its outcome leaves uncertainty; it does not justify automatic replay.
- **Fallback must preserve the requested guarantee.** A mailbox-only request must not later become typed input because an attachment timed out. Nor does a timeout prove that a busy terminal is safe for a restart command. An unavailable path should remain visibly unavailable unless a permitted alternative preserves the user's intent and authority.
- **Typed input is fragile.** Typing into an agent's editor risks colliding with a human's unsent draft and reaching a replacement agent. A pasted newline, Esc, or a history-navigation key cannot establish that an editor is empty. Cooperative editor-state reports help, but stale reports must not clear newer input. Held input must be bound to the exact recipient execution, not merely an agent kind or a pane.
- **Decide what a message is addressed to.** Addressing grew into a hybrid of per-pane queues, session pins that follow an agent between panes, and time-based fallbacks. Each rule had a reason, but together they were the most complex part of the system. Vqro should distinguish the input surface, the intended recipient, the context, and the permitted execution. A pane, a durable agent identity, and a specific execution have different lifetimes. Moving a pane or restarting an agent must not silently change whom a message addresses; previously claimed work needs explicit recovery rather than ordinary queue migration.
- **Connection is not readiness.** A process may connect for reporting or identity without consuming its inbox. An adapter must distinguish transport attachment, messaging support, and actual consumer readiness. Configuration or source changes do not establish that a running agent loaded them; verify the behavior at the receiving end.

### Activation and lifecycle

- **Wake is policy with one owner.** Users do want a message to wake a sleeping agent. What matters is that an agent turn starts only from a human action or an approved, recorded policy, with one waker, a durable record per attempt, and no replayed prompts. Receipts and status changes never start a turn.
- **Launch recipes must be conservative.** Relaunching an agent needs a recorded recipe that never replays an initial prompt and drops credentials and unlisted environment.
- **Keep lifecycle in-process and simple.** An external, platform-specific supervisor added a second wake model with opposite rules and was retired in favor of in-process sleep and wake.
- **Optional subsystems must fail soft.** Additional sockets and listeners became lifecycle liabilities: one broke live server replacement, and a derived socket path that exceeded the platform limit prevented the whole server from starting. A coordination feature should lose only its own availability, not terminal access or session recovery. Test the complete installed combination across startup, restart, upgrade, and rollback; a successful stop/restart does not prove PTY-preserving live handoff.

### Authority and evidence

- **Coordination authority is cooperative.** Grants, manifests, and permission files checked by owner and mode do not contain an agent that runs as the same user. Present them as a coordination protocol, not a security boundary.
- **Prefer simple visible states over unprovable proofs.** Elaborate machinery could not prove that a child never reported while unobserved delivery paths remained possible. A bounded observation such as "reported done, no report seen through this route yet" is useful without pretending to prove absence. Clearing that observation on admission still does not mean the parent read or accepted the report.
- **Agent-specific coupling in the host is debt.** Launch, readiness, session identity, and tool restrictions belong behind a neutral agent adapter, with at least two agents to keep the contract honest.

## Directions these lessons point toward

### Communication

- Explore a per-pane input surface owned by Vqro: submitted input becomes an addressed message, with queueing, editing before pickup, and visible delivery state. Unsent drafts remain distinct from submitted messages, and direct terminal interaction remains available.
- Hiding an agent's native input is optional and conservative: cooperative where the agent supports it, and screen-detected only when an idle input box is positively recognized. Prompts, menus, and unrecognized states always show the full terminal, and the full terminal is one action away.

### Routing

- Participants address intent to Vqro, which may route it to an agent, a derived context, another participant, a group, or a service. Destinations are a general concept, not only panes.
- The host provides routing mechanics: addressable destinations, a durable record of every message and routing decision, and receipts. Routing policy, whether rules or a model, is a replaceable extension. An explicit direct path is always available.
- Routing decisions are recorded, visible, and overridable. A router's authority is bounded, and a question is never escalated into an action.
- A derived context, such as a clone of an agent's history for a historical question, answers by default and inherits no permission to act. It shows the point in time it reflects, is attributed as a clone, and has a bounded lifetime.
- The core stays deterministic. Intelligent layers sit on top of an inspectable core; no model sits in the critical path of delivering input.

### Platform and process

- Coordination is a candidate next product area. Start with a useful vertical slice and explicit source, recipient, restart, and activation semantics. Determine which effects and durable state it actually needs before committing to new generic contracts.
- Keep one trunk with short branches. Long stacked branches and parallel release lines drifted and cost more to reconcile than they saved.
- Prototypes may move fast and messily; Vqro adopts their lessons through explicit design, not by importing their code.
