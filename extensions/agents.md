# Agents: runtime-neutral UI

`agents` observes every resource with `kind: agent`. It does not launch programs, select a runtime, or contain a runtime-specific bridge import.

- **Agents** on the tab bar lists observed agents. Selecting one invokes `agents.show` and opens/focuses its neutral `agents-rN` panel through the public `tabs.show` command. Sidebar, resource pickers and pane replacement use the same resource-kind routing: agents never become terminal views. With agents off, only an actual host terminal is used as the fallback.
- An agent with a terminal host has a **Bridge terminal** action. The sidebar groups that terminal as a collapsed child of the agent instead of showing it as an unrelated terminal.
- **Stop agent…** (agent pane menu, sidebar agent row menu, Agents list row menu) ends an agent the runtime-neutral way: it kills the agent's host (bridge) terminal, which ends the runtime process inside it. It first asks for confirmation in a popup naming that terminal; the confirmed action rechecks that the host is unchanged and reports the honest `resource.kill` outcome (accepted is not "ended"). Agents with no host terminal known to vqro are not stopped; the message says why (stop them where they run). Nothing is resent and undo can't revive the process.
- The agent pane (an agent with an item stream) keeps its chrome small (the human, relay-11): the frame title is
  name · folder, with the reported state at its right end (`title_right`). Nothing sits above the conversation. The only
  button is **⚙** at the bottom, beside the composer's Input button (a composer `button` control; a one-chip row at
  the bottom on clients without composer buttons) and `agents.pane` opens a popup with the view filters (thinking, tools, system, senders, layout, summary fields,
  make default; the pane keys t/T/s still cycle them), then Agent settings…, Bridge terminal and Stop agent… (also in
  the pane menu). The composer has one **Input** button (the delivery mode: auto, steer, follow-up) and Enter sends.
  A file a tool call names opens in an editor from that tool line's menu (`links.open`, only on a click).
- Ended resources and gone agent tabs are visibly marked. **Close ended tabs** removes wholly ended tabs and mixed/live panes stay; a tab group left with no tab gets one new shell tab instead. **Sidebar view → Close/hide ended items** also hides ended sidebar resources. **Show ended items** restores them. These actions never kill programs or erase backend data. Disconnected/stale resources are unavailable, not falsely called gone.
- A disconnected agent says which case it is: its bridge terminal has finished (it can't come back; the `retention` extension forgets it at once), or its state is **unknown since HH:MM** (when its provider disconnected, from the core provider registry). Unknown agents stay until they reconnect, are stopped, or are explicitly forgotten: **Forget agent** (agent pane menu; sidebar and Agents list rows while unavailable) calls `resource.forget_cached`, which drops only vqro's cached facts and sends nothing to the runtime. A bridge that later reconnects shows up again. Retention never forgets an agent by time (stopgap until bounded tombstones arrive with the agent standard).
- On that panel, the pane menu (normally prefix `m`) offers native operations declared by that resource's provider: prompt, steer, follow-up, abort, clear queue and answer. Direct commands also reject undeclared operations before sending anything. Other extensions may add their own public-command actions.
  Live `menu_context.agent_error` reports a currently available idle native error completion; `agent_app` is the reported status app name (or null). They are generic presentation hints, not permission to restart/send. Accounts uses them for a Pi error's human-judged account-choice menu; the command rechecks native state and owner authority.
- Agent settings…: for agents that declare `data.agent.config` / `data.agent.commands` (agent v0 6.3, with the matching
  `agent.configure` / `agent.command` capability), the pane menu, a row on the agent panel and `agents.config` open a popup of select rows
  (model, thinking level, …) and command rows; `agents.configure {id, value, rev}` / `agents.command {name, text?}` send
  only declared values (a choice from an outdated popup is rejected as stale) and report the outcome honestly (`observed` vs not yet confirmed). Agents without
  declarations show nothing.
- **Work** in the conversation shows native child agents, background tasks, goals and plan/todo lists when the adapter
  reports them. Child rows follow reported parent relationships, including nested children; missing/cyclic relations
  remain visible rather than being invented. Status, roles, usage, task owners/progress and list items come from the
  item stream. A Goal is a separate runtime objective, showing only reported budget/usage; it is never converted
  into plan steps or an invented percentage. Unknown states remain unknown, absent sections stay absent, and clipping is explicit. This is derived
  observation, not a second task store, coordination inbox or an editable todo list. Every plan entity is rendered
  as its whole installed current list; `update: items` describes the runtime's granularity, not a client-side merge.
  Native `incomplete` reports are labelled; an incomplete list with no items is not presented as an empty plan.
  **Open view** follows a child endpoint through ordinary agent navigation (its own capabilities apply), or opens
  an explicitly scoped child in a read-only popup using the parent's stream. Tagged child messages/tools are not
  flattened into the main conversation. Scoped tools stay inline; parent-only tool tables and a message composer
  are not offered there. Requests remain ordinary requests handled through existing parent/native controls.
  Retained activity is labelled possibly stale during reconstruction/disconnection or a resource/item epoch mismatch;
  the child display shares the inventory's full freshness predicate. Item-only updates refresh open
  child popups. Display-only children do not acquire invented endpoints or controls. Opening a child does not grant
  send/stop authority or imply that stopping its parent stops it. Pi gets no synthetic children/tasks/plans.
- **Customizations** in Agent settings is a read-only native startup inventory (skills, prompt templates/commands,
  extensions, plugins and MCP servers), including reported sources and incomplete lists. An open view refreshes on
  startup item revisions, removal and epoch changes. The frame warns when input may be transformed; each message's
  historical possibility note comes from its own receipt, never today's inventory. Native `message.invoked` alone
  supplies the transcript's “ran template/skill/command” label.
- **Explicit slash invocation in the message box:** the composer submits `agents.compose`, not the public literal-data
  command `agents.send`. Its vqro grammar is deliberately not a runtime parser: `/` immediately followed by an exact
  advertised invocation name; the first ASCII space separates arguments, and everything after that space is passed
  unchanged (including repeated spaces, tabs, newlines and Unicode). No trimming, short skill aliases or native
  whitespace emulation. For example, use `/skill:review notes` when `skill:review` is the advertised spelling, not
  `/review`. A template/command actually advertised as `review` still uses `/review`. A tab immediately following the
  name is not the argument separator. Unmatched text remains literal `agents.send` data; a native refusal is shown.
  The composer placeholder makes this command intent visible. Invocation requires idle **auto/start**; other selected
  delivery modes are refused with an explanation, not silently bypassed (`agent.command` has no delivery-mode field).
  Capabilities revision and startup epoch/revision plus advertised command declarations are fenced against the
  displayed composer; changed declarations send nothing and require reviewing the current message box again.
  The installed item epoch must match the current resource epoch when reported; loading, broken, disconnected or
  partial-snapshot inventory cannot authorize invocation. Each new stream attachment requests a full snapshot;
  invocation readiness requires a complete, error-free reconstruction on that attachment, not merely a stream ID.
  Any SDK decode/parse/control error revokes that witness until a subsequent clean snapshot completes. Retained
  entities remain available for display, labelled as possibly stale; non-lossy errors do not create a reset/retry loop.
  The displayed epoch is also passed to `agent.command`
  as its downstream admission precondition, protecting a subsequent native epoch change. A skill display name
  without an advertised invocation key remains visible but is not callable.
  `agents.compose` is explicit invocation intent, not an authentication boundary: callers wanting literal data must
  use `agents.send`, which never promotes slash text to commands. No new core/client primitive is involved.
- Prefix `P` (Shift-p) opens Agent settings for the agent in the focused pane (`binding/agents.config`, same `when`).
- Prefix `p` prompts the agent in the focused pane (`binding/agents.prompt`, `when: {"resource_kind": "agent"}`): on any other pane the key does nothing and says so, before asking for text. A keymap profile's `p` still wins when selected.
- Prompt-like actions use input. Answer opens a pending-request picker: confirm/select choices, input/editor text, explicit cancellation. Editor uses existing single-line input, not a new multiline editor.
- Notifications preserve native outcomes: acceptance is not settlement; dispatched answers do not invent acknowledgement. Effects are never automatically replayed.

The panel uses resource/provider facts and optionally references provider output read-only, never mirroring its
rows into a second publication. Runtime-neutral `data.views` may advertise up to eight ordered view names;
otherwise the existing `agent-rN` convention is used. Generic `embedded_views` client composition preserves the
extension's own controls and warnings while keeping native status/transcript publications independent.
Optional runtime-neutral `data.failure.message` and `data.retry` (`active`, `attempt`, `max_attempts`, `delay_ms`,
`success`) facts are rendered as bounded semantic error/retry rows, including alongside native output. Only
reported attempt counts are shown; the UI never implements retries. Providers remain responsible for mapping
native failed-turn/settlement events into resource status; settlement alone does not imply success. Without that optional native output, it renders generic status, availability and data; arbitrary runtimes need no transcript/run/queue fields. It stays bounded to32KiB, with explicit clipping and stale/unavailable warnings. Panel output is live content, not transaction history.

Native capabilities come from the public provider registry, cached by `vqro_ext` from snapshots and provider events. No capability is inferred from a runtime name or mode. Menus/panels are reconciled by ownership and lifecycle/capability changes, never on every status update.

Launchers are separate extensions. The `pi` extension supplies New Pi agent and Pi runtime settings (`pi.md`). Disabling a launcher does not stop its backend-owned agents or neutral controls. Disabling agents removes its UI/commands, not the underlying processes: real backend terminal resources remain available.

The former agents.new/settings/configure contributions move to pi.new/settings/configure and owner pi; activation reconciliation removes stale agents-owned launch entries. Pi performs the non-destructive settings migration.

Verification: `tests/test_agents.py` includes a non-specific provider with only abort; private E2E also uses an independent non-Pi wire provider, checks independent disabling and terminal fallback, and drives actual TUI keys using fake runtimes only. No real model call is needed. `tests/pi_provider_e2e.py --run --agents-ui --probe-terminal-invoke --vqro ABS_VQRO --hc ABS_HC`; the full release gate includes it.
