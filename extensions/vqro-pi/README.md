# Neutral Vqro Pi adapter

`index.ts` bridges Pi SDK lifecycle observations and explicit queued input. It
contains no Tasks, message activation, recipient selection or reply policy.
Consumers register through the SDK-scoped version1 interface described in
[`docs/next/api/pi-adapter-v1.md`](../../docs/next/api/pi-adapter-v1.md).

The bundled Tasks consumer is `../tasks/pi.ts`; it records a delivered-message
turn's final text as a durable reply message. Adapter observations are not
durable messages, authenticated identity, accepted work or evidence of model
understanding. Ordinary host identity/admission checks remain authoritative.

`just pi-reply-routing-test` runs neutral-adapter and moved Tasks policy tests
without a model/server. `just pi-profile-demo` exercises the private actual Pi
resource profile; `just readiness-demo` runs the unchanged guide workflow.

## Explicit Pi selection

The shared Vqro config accepts two independent optional settings:

```toml
[extensions]
pi_executable = "pi1"
pi_source_dir = "~/.pi/agent1"
```

Neither setting auto-selects the other. Absent values retain `pi` and
`~/.pi/agent`; no source profile is installed or modified. The executable is one
name or absolute path, never a shell fragment. The source is an absolute or
`~/`-relative directory. Plain Vqro startup prepares a source-keyed shadow;
switching sources cannot reuse the prior source's resource/auth links, even if
settings match. Old shadows are left alone. Source settings remain byte-exact;
the shadow settings stamp is canonical JSON ignoring only the top-level
`lastChangelogVersion` and `extensionChangelogVersions` that Pi writes for
changelog display. All other values, nested names and array order are guarded.
Malformed/non-object/unreadable shadows are not ready. The private `settings-v2`
marker deliberately invalidates old raw-byte markers until startup preparation.

Presence of **either** setting makes managed Pi launch explicit: an unavailable
executable or invalid/unreadable/unprepared source fails with
`agent_profile_unavailable`, without sending a launch command or falling back.
After changing configuration, run plain Vqro again to prepare the selected
shadow. Each managed start resolves one config snapshot and binds its own argv,
profile environment and notice, overriding an old pane's inherited source.
Non-Pi kinds are unaffected. The separate role launcher continues to require its
explicit `--pi-bin`; it does not gain an automatic executable/source switch.

With both settings absent, the accepted default missing-source behavior remains:
the shadow can carry only bundled Vqro resources. If that default shadow is
unavailable, a warned managed launch **unsets** inherited
`PI_CODING_AGENT_DIR`, loading default Pi config, not a previous pane source.
`VQRO_NO_BUNDLED_DEFAULTS` disables default preparation and selects that ordinary
default config without the warning; it cannot bypass an explicit selection
(explicit managed launches fail). A hand-typed command still uses its pane's
birth environment; these per-start guarantees apply to managed agent starts.
Explicit source profiles require Unix symlinks; unsupported platforms reject
explicit managed choices rather than claiming readiness.

The existing Herdr package exclusions also drop exact normalized
`pi-system-prompt` package names/paths, preserving lookalikes. Shadows reference
human resources/auth by symlink: they are **not sandboxes**, and real Pi's own
credential refresh can write through those links. Tests instead seed private
sources and dummy auth; no human credentials or model calls are needed.

The existing demos accept `--pi /absolute/installed/pi-coding-agent/dist/bundle/cli.js
--pi1-profile` to select `pi1` and a private agent1 source through these actual
shared settings. Use the executable installed entrypoint, not a wrapper that
locates its SDK relative to `$HOME`: the demos deliberately replace HOME. They
keep all normal private-root/process-birth guards and image receipts.

## Installed Pi1 SDK check (offline)

`pi1-sdk.mjs` accepts an already-installed Pi1.5.0 package directory and a new
private scratch root. Run it with `env -i`, `HOME=<root>/home`, private XDG
config/state/runtime directories and a PATH containing Node; use the project's
shared heavy-run limits. It never installs a package or reads human credentials.
The real SDK loads `index.ts` and `../tasks/pi.ts`, creates persisted sessions,
and exercises awaited context claims, activated delivery, final replies,
execution-change discard and reload/shutdown. A deterministic in-process stream
replaces model access; network fetches reject. Identity and mailbox CLI responses
are explicit doubles: **this is not native principal/admission evidence**. The
separate real profile/readiness demos cover the native path. `evidence.json`
retains source/SDK hashes and the actual lifecycle facts, with the session log
and CLI calls beside it.

Observed on Pi1.5.0: `turn_end` adds actionable boundary metadata but retains
`event.message`; `before_agent_start` awaits returned custom context, and
`agent_settled` remains the final-run observation. Pi marks itself idle before
awaited settled handlers finish, so a new prompt within that dispatch is deferred.
Reload shuts down the old extension instance and invalidates its API contexts.
Bare SDK `AgentSession.dispose()` does not emit shutdown: embedders must emit it
first (the CLI's `AgentSessionRuntime` does). These checks require no production
adapter/Tasks byte change. The existing old-Pi behavioral tests remain required;
this optional harness's Pi1 ModelRuntime/provider APIs are not an old-Pi SDK
compatibility promise. Profile availability is not SDK or native-platform proof.
