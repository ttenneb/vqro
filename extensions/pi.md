# Pi: launcher and settings

`pi` owns **New Pi agent** on the tab bar and prefix `a` (normally Ctrl-g then a). It launches the installed `python3 -m pi_bridge -- <configured argv>` through public `resource.spawn` with `bridge:true`; the backend, not this extension/core, owns execution.

`doc/settings/pi` defaults to:

```json
{"runtimes":[{"name":"pi","argv":["pi1","--mode","rpc"]}]}
```

Configure it through Settings → Extensions → Configure Pi runtimes, or the `pi.settings` palette command. The input accepts the JSON list of `{name, argv}` entries. Argument vectors are literal, never shell strings. Multiple entries produce a runtime chooser. Invalid settings fail before execution. Backend terminal names are left to the backend so multiple launches do not collide or guess native naming restrictions.

Once the hosted agent appears, Pi invokes public `agents.show`. If agents is disabled/unavailable, it opens the real backend terminal through `tabs.show` instead. Disabling pi only removes its launcher/settings; existing bridges and agents continue running. Native provider output and bounded terminal fallback remain in the pi_bridge package.

On first activation, existing `doc/settings/agents.runtimes` is copied to `doc/settings/pi` only if the destination is absent. The source is preserved and existing destination settings are never overwritten. No process starts during migration/configuration.

Use the separate runtime-neutral agents extension for declared native controls, request answers and panels. No prompt is sent automatically by the launcher. Spawn/registration/tab failures report what happened without automatically repeating effects. Existing agents are unaffected by later settings changes.

Tests use fake Pi, an independent non-Pi provider, and private real core/backend/TUI services. Real provider-backed prompts, multiline editor UI, Windows and remote relay are not claimed.
