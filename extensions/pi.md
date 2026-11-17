# Pi: launcher and settings

`pi` owns **New Pi agent** on the tab bar and prefix `a` (normally Ctrl-g then a). It launches the installed `python3 -m pi_bridge -- <configured argv>` through public `resource.spawn` with `bridge:true`; the backend, not this extension/core, owns execution.

`doc/settings/pi` defaults to:

```json
{"runtimes":[{"name":"pi","argv":["pi1","--mode","rpc"]}]}
```

The default bridge launcher is host-resolved `python3 -m pi_bridge`, never the core's interpreter path or an OS
choice inferred from the core. A runtime entry may configure `bridge_argv` (for example `["py","-3","-m","pi_bridge"]`
for its Windows host, or a host-installed wrapper) and `bridge_env` (for example a host-side `PYTHONPATH`). These
arguments/environment resolve on the terminal host. A configured launcher does not inherit the core's module path;
the unchanged local default still supplies the locally installed extension directory. Remote endpoint reachability
is a separate, unimplemented concern; these settings do not make remote bridges supported.

Configure it through Settings → Extensions → Configure Pi runtimes, or the `pi.settings` palette command. The input accepts the JSON list of `{name, argv}` entries. Argument vectors are literal, never shell strings. Multiple entries produce a runtime chooser. Invalid settings fail before execution. Bridge terminals get unique `pi-bridge-<suffix>` names (short literal names without spaces); tabs/sidebar display `<agent name> <id> bridge` while the agent is observed.

Once the hosted agent appears, Pi invokes public `agents.show`: one launch opens one agent tab, not a separate bridge tab. The agent panel's **Bridge terminal** action opens the diagnostic terminal explicitly; it redraws a bounded screen rather than appending repeated state dumps. Status is readable, with queue counts only when nonzero. Explicit controls wake status publication without waiting for the transcript sampling timer. If agents is disabled/unavailable, it opens the real backend terminal through `tabs.show` instead. Disabling pi only removes its launcher/settings; existing bridges and agents continue running. Native provider output and bounded terminal fallback remain in the pi_bridge package.

On first activation, existing `doc/settings/agents.runtimes` is copied to `doc/settings/pi` only if the destination is absent. The source is preserved and existing destination settings are never overwritten. No process starts during migration/configuration.

Use the separate runtime-neutral agents extension for declared native controls, request answers and panels. No prompt is sent automatically by the launcher. Spawn/registration/tab failures report what happened without automatically repeating effects. Existing agents are unaffected by later settings changes.

Tests use fake Pi, an independent non-Pi provider, and private real core/backend/TUI services. Real provider-backed prompts, multiline editor UI, Windows and remote relay are not claimed.
