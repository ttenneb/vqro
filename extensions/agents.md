# Agents: runtime-neutral UI

`agents` observes every resource with `kind: agent`. It does not launch programs, select a runtime, or contain a runtime-specific bridge import.

- **Agents** on the tab bar lists observed agents. Selecting one invokes `agents.show` and opens/focuses its neutral `agents-rN` panel through the public `tabs.show` command. Sidebar, resource pickers and pane replacement use the same resource-kind routing: agents never become terminal views. With agents off, only an actual host terminal is used as the fallback.
- An agent with a terminal host has a **Bridge terminal** action. The sidebar groups that terminal as a collapsed child of the agent instead of showing it as an unrelated terminal.
- Ended resources and gone agent tabs are visibly marked. **Close ended tabs** removes wholly ended tabs (retaining the last tab and mixed/live panes); **Sidebar view → Close/hide ended items** also hides ended sidebar resources. **Show ended items** restores them. These actions never kill programs or erase backend data. Disconnected/stale resources are unavailable, not falsely called gone.
- On that panel, the pane menu (normally prefix `m`) offers only native operations declared by that resource's provider: prompt, steer, follow-up, abort, clear queue and answer. Direct commands also reject undeclared operations before sending anything.
- Agent settings…: for agents that declare `data.config` / `data.commands` (and the matching `agent.configure` /
  `agent.command` capability), the pane menu, a row on the agent panel and `agents.config` open a popup of select rows
  (model, thinking level, …) and command rows; `agents.configure {id, value}` / `agents.command {name, text?}` send
  only declared values and report the outcome honestly (`observed` vs not yet confirmed). Agents without
  declarations show nothing.
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
