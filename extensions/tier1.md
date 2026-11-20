# tier1: Claude Code and Pi agents with vqro's inbox plugin

`tier1` adds two launch commands (the command list; no tab-bar button):
- **New Claude agent** (`tier1.claude`);
- **New Pi agent (tier 1)** (`tier1.pi`).

Each starts the runtime in a new backend terminal tab, as you would by hand. It also wires vqro's tier-1 pull plugin
(coordination-v0 §7) **for that launch only**. Your own `~/.claude` and `~/.pi` are never read or written by vqro.

- **Claude Code:** `claude --settings <file>`. The file holds only the hooks block of `vqro-claude-hook settings`. It is
  written per launch into `<vqro state>/tier1/claude-<nonce>.json` (directory 0700, file 0600). It is removed once its
  terminal has ended.
- **Pi:** `pi -e <extensions dir>/tier1/vqro-pull.ts`. `scripts/dev-install.sh` installs the plugin there.

`vqro-claude-hook` is found in this order: `doc/settings/tier1.hook`; beside the running `vqro` (where dev-install
installs it); `PATH`. Without it, New Claude agent refuses before starting anything.

## What a launch does

1. **Spawn.** `resource.spawn` runs `tier1_run.py -- <runtime argv>` in a terminal named `<runtime>-<nonce>`, with
   `VQRO_PRINCIPAL=p:<fresh uuid>` and `VQRO_TIER1_LAUNCH=<nonce>`. This happens exactly once and is never retried. The
   core records that principal as the terminal's `principal`; the plugin binds the same one at its hello.
2. **Declaration.** In your command (attributed to you), `tier1` records `doc/agents/terminals/<rN>` = `{pull: <the
   plugin's own declaration>, launch: <nonce>, app}`. The declaration comes from `vqro-claude-hook declaration` or the
   Pi plugin's `PULL_DECLARATION`.
3. **Grant.** Through `coord.grants`, it grants the new principal `receive` on `agent:<rN>`.
4. **Resume.** It resumes the new agent's inbox (`coord.scope resume agent:<rN>`): starting the agent is your intent.
   If coord refuses (no coord extension, a missing grant, or a stale preview), the inbox stays paused, and the launch
   notice says why. Nothing is retried.
5. **Wrapper.** `tier1_run.py` finds its own terminal by name, waits up to 10 s for the declaration with its nonce and
   for the grant, and then execs the runtime with `VQRO_AGENT=<rN>`. On a timeout or a core problem it still starts
   the runtime without `VQRO_AGENT`, so the plugin stays inert, and prints one line saying why. It never blocks you.
6. **Tab.** The terminal opens in a new tab (`tabs.show`). No prompt is sent.

**Request routing (AS10c'):** nothing is recorded yet. Tier-1 plugins forward no native requests: you answer
permission prompts in the pane, so the route is `human` in effect. When coordination defines the route key, the
launcher will record `human` explicitly.

## Settings

`doc/settings/tier1` holds `{claude?: argv, pi?: argv, hook?: path}`. The default argv is `["claude"]` and `["pi"]`;
argv are literal, never shell strings.

## Off and recovery

- Nothing else depends on `tier1`. With it off, or in safe mode, already-launched agents keep running as plain
  terminals, and their plugins keep working while the core is reachable.
- With coord off, agents still start; the notice says the inbox is not open.

## Tests

- `tests/test_tier1.py` (fake core):
  - argv per runtime, the private settings file and its modes;
  - a fresh principal and nonce per launch;
  - the declaration, grant and resume order;
  - a refused resume;
  - a missing hook;
  - the settings-file sweep;
  - the wrapper's resolution, timeout and exec.
- `tests/tier1_e2e.py` (opt-in; real Claude Code and Pi, fake models, a private tmux backend through vqro-tmux):
  - both launches;
  - the principal, declaration and grant;
  - a boundary message reaching Claude Code at its next turn point;
  - a start message waking the idle Pi;
  - the private HOME's `~/.claude/settings.json`, `~/.pi/agent/settings.json` and `models.json` byte-identical
    afterwards.
