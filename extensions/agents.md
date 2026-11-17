# Agents: runtime-neutral UI

`agents` observes every resource with `kind: agent`. It does not launch programs, select a runtime, or contain a runtime-specific bridge import.

- **Agents** on the tab bar lists observed agents. Selecting one invokes `agents.show` and opens its neutral `agents-rN` panel through the public `tabs.show` command.
- On that panel, the pane menu (normally prefix `m`) offers only native operations declared by that resource's provider: prompt, steer, follow-up, abort, clear queue and answer. Direct commands also reject undeclared operations before sending anything.
- Prompt-like actions use input. Answer opens a pending-request picker: confirm/select choices, input/editor text, explicit cancellation. Editor uses existing single-line input, not a new multiline editor.
- Notifications preserve native outcomes: acceptance is not settlement; dispatched answers do not invent acknowledgement. Effects are never automatically replayed.

The panel uses resource/provider facts and optionally displays the provider's `agent-rN` live content read-only. Without that optional native output, it renders generic status, availability and data; arbitrary runtimes need no transcript/run/queue fields. It stays bounded to32KiB, with explicit clipping and stale/unavailable warnings. Panel output is live content, not transaction history.

Native capabilities come from the public provider registry, cached by `prim_ext` from snapshots and provider events. No capability is inferred from a runtime name or mode. Menus/panels are reconciled by ownership and lifecycle/capability changes, never on every status update.

Launchers are separate extensions. The `pi` extension supplies New Pi agent and Pi runtime settings (`pi.md`). Disabling a launcher does not stop its backend-owned agents or neutral controls. Disabling agents removes its UI/commands, not the underlying processes: real backend terminal resources remain available.

The former agents.new/settings/configure contributions move to pi.new/settings/configure and owner pi; activation reconciliation removes stale agents-owned launch entries. Pi performs the non-destructive settings migration.

Verification: `tests/test_agents.py` includes a non-specific provider with only abort; private E2E also uses an independent non-Pi wire provider, checks independent disabling and terminal fallback, and drives actual TUI keys using fake runtimes only. No real model call is needed. `tests/pi_provider_e2e.py --run --agents-ui --probe-terminal-invoke --prim ABS_PRIM --hc ABS_HC`; the full release gate includes it.
