# Agents extension

Enable `agents` (now bundled in defaults) to start backend-owned Pi RPC agents without `prim call`.

- **New Pi agent** on the tab bar or prefix `a` (default `Ctrl-g`, then `a`) starts a bridge and opens its agent view in a tab. No automatic prompt.
- On the agent pane, open the pane menu (default prefix `m`, or the configured gesture): **Prompt**, **Steer**, **Follow-up**, **Abort**, **Clear queue**, **Answer pending request**.
- Prompt/steer/follow-up use the normal client input. Answers open a per-agent pending-request popup; confirm/select use choices, input/editor use text, and cancellation is explicit. Editor text uses the existing single-line UI input, not a new editor widget.
- **Agents** on the tab bar reopens hidden agent views and links runtime settings. Settings → Extensions also offers **Configure agent runtimes** while agents is running.

`doc/settings/agents` defaults to:

```json
{"runtimes": [{"name": "pi", "argv": ["pi1", "--mode", "rpc"]}]}
```

Runtime settings accept the JSON **list** of `{name, argv}` entries. Names must be unique; argv is a nonempty list of literal arguments, never a shell string. With multiple runtimes, New opens a runtime chooser. These are Pi RPC commands, not arbitrary runtime protocols. The bridge is launched separately with the extension's Python interpreter as `python3 -m pi_bridge -- <runtime argv>` and `PYTHONPATH` pointing at the installed extension directory. Backend primitives own all execution; agents.py never starts a child process itself.

The extension uses `resource.spawn` with `bridge:true`, waits for the hosted agent/view, and invokes the public `tabs.show` command. Existing agents keep running if their tabs close or settings change. If tabs is unavailable, a ready agent can appear as a popup. If registration is slow, the observed terminal is shown for diagnostics and Agents can find the agent later. A timed-out or failed UI action never automatically repeats a native effect.

Notifications preserve native outcomes: acceptance is not settlement; UI answers are dispatched without inventing acknowledgement. The agent panel shows semantic status badges, observed run/settlement, bounded queue previews, pending requests and a bounded transcript tail. Only native facts establish settlement.

`tests/test_agents.py` uses a fake core. `tests/pi_provider_e2e.py --run --agents-extension --prim ABS_PRIM --hc ABS_HC` drives all native operations through the actual extension. Add `--agents-ui` for physical prefix/menu/input/answer checks in a private tmux PTY. Both use fake Pi only, private state/services and network namespace; no real prompt/model/provider call. Evidence: `docs/research/agents-extension.md`.
