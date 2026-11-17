# herdr-core provider, provider side (2026-11-17)

`src/bin/prim-herdr.rs` runs herdr-core as an out-of-process provider against `spec/provider-v0.md` section 9. It
wraps the existing in-process adapter (`prim::adapter::herdr`, unchanged) and bridges `AdapterCmd` and
`AdapterEvent` to `prim::provider_client` (the same helper as prim-tmux, `tmux-provider.md`). Tested against an
in-test fake core and a private herdr-core 0.1.21; not yet against the real core's section 9 side.

## Behaviour

- **Command line.** `prim-herdr [--hc PATH] [--session NAME] [--name NAME]`, connecting to `$PRIM_SOCKET`. It reaches
  herdr-core with `hc connect` (`--session` is passed through). It honours the extension lifetime channel.
- **Identity.** The instance is herdr-core's `core.id`: stable across core and provider restarts, and new after
  herdr-core's state is wiped. Refs are terminal addresses (`t3`). The incarnation is the terminal's `created_ms`
  (the adapter's choice).
- **Registration.** There is one core registration per herdr-core connection. The adapter's snapshot becomes a full
  sync, its upserts become `resource.upsert`, and its removals become `resource.gone {closed}`. When herdr-core is
  lost, the provider closes its core connection, so the core marks it disconnected (D21). When the adapter
  reconnects, the provider registers again (new generation, full sync). Nothing is resent.
- **Capabilities:**
  - `terminal.spawn {env: false, cwd, name}`, `terminal.input`, `terminal.output {raw, history: true, repaint: "none"}`;
  - `terminal.resize {owner: "latest_viewer"}`, `terminal.kill`, `terminal.list`, `terminal.exit {exit_code: true}`;
  - `status.osc7501 {stored: true, lossy: false}`, `events.resync`, `persist.detach`, `remote.machines`;
  - `identity {refs_survive_backend_restart: true}`.
- **Effects.**
  - **`terminal.spawn`:** argv, cwd and name, using `terminal.run` through the adapter. An empty argv uses the
    provider's `$SHELL`, else `/bin/sh`. `env` is refused as `unsupported`: the adapter's `SpawnRequest` has no env
    field.
  - **`terminal.kill`:** herdr-core's reply is the terminal record. When it says `finished`, the provider replies
    `observed` with finished facts (`exit {reason, exit_code, signal}`) and upserts them.
  - **Outcomes:** herdr-core error replies and lost replies are JSON-RPC `backend` errors (outcome unknown), as the
    in-process core treats them (`AdapterError::Backend` is indeterminate there). A wrong incarnation is
    `not_found`.
- **Streams.** herdr-core attach channels, with `initial: history` when `history_lines > 0`. Bytes are sent through
  `provider_client` (window and offsets), and only then is CREDIT returned to herdr-core. Endings:
  - herdr-core's close reason (for example `{reason: killed, exit_code, signal}`) becomes `stream.closed`;
  - a lost backend becomes a `backend_dropped` gap;
  - an adapter buffer overflow becomes a `viewer_slow` gap.

## Findings

1. **No `finished` event after a kill.** After `terminal.kill` with an attach open, herdr-core skipped the `finished`
   event for this subscriber (the event seq jumped). `hc events` without an attach does get it. The cause is
   unverified. The kill reply carries the finished record, so the provider publishes it from there.
2. **Events-channel credit.** herdr-core's events channel is credited ("JSON lines, with credit", schema
   `SubscribeResult.channel`), but the adapter never returns events credit. More than a kilobyte of events still
   arrives (regression test), so the window is larger than that. Whether it ever runs out on a long connection is
   unverified. This belongs to whoever owns `src/adapter/herdr.rs`. I first misreported this as a 1 KB stall and
   then corrected it.
3. **herdr-core status semantics.** herdr-core coalesces status reports and clears a terminal's status (`null`) at
   times, for example after a command finishes. Tests must not wait for one particular status value.
4. **Wiping a core.** There is no `hc state wipe`: stop the core and delete `$XDG_STATE_HOME/herdr-core/<session>`.
   `hc schema` auto-starts a core like any other `hc` command.

## Reproduce

```sh
T=$HOME/.local/share/herdr/scratch/prim-scale-target
env -i HOME=$HOME PATH=$PATH CARGO_TARGET_DIR=$T cargo test --test herdr_provider -- --ignored --test-threads=1
```

The tests need `~/.local/bin/herdr-core`. Each test sets up a private session:
- a short `/tmp` root with its own `HOME`, `XDG_STATE_HOME`, `XDG_RUNTIME_DIR`, `XDG_CONFIG_HOME` and `HC_SESSION`;
- an empty `PATH`, so the core can't reach `loginctl` or `systemctl`; programs in its terminals need absolute paths;
- teardown with `hc server stop`, then a kill by pid if needed. Only that private core is stopped.
