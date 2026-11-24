# vqro

One lean, standardized layer of workspace primitives over any multiplexer and any agent runtime. Humans and agents use
the same primitives to build and change their agent workspace; coordination is workspace behaviour too. Backends
(herdr-core, Rex, tmux, Zellij, ...) own execution; vqro adapts to them through thin adapters that declare capabilities.

Status: **a working prototype, and fluid.** It is usable daily over herdr-core, but protocols, extension APIs and
UI will keep changing as we learn (`docs/roadmap.md` for direction, `docs/build-plan.md` for the build log).

## Licence and contributions

All original vqro material—core, bundled extensions, SDKs, documentation and protocol specifications—is licensed
under **Apache-2.0**. See [LICENSE](LICENSE) and [NOTICE](NOTICE). Dependencies and other third-party material keep
their own terms; [THIRD_PARTY_LICENSES.md](THIRD_PARTY_LICENSES.md) is the offline-generated Cargo notice inventory.
Contributions use [DCO sign-off, no CLA](CONTRIBUTING.md). Apache-2.0 grants no trademark rights: the vqro
name/branding, official builds and maintained extension repository identify the project, not every modified build.
The final name is **vqro** (D27); see [TRADEMARKS.md](TRADEMARKS.md).

## Install and run

Requires stable Rust, Python 3.11+, git and **Zig 0.17.1** for the vendored terminal engine.
Use `brew install zig` (check `zig version`), or `mise install zig@0.17.1`; set `ZIG` to its binary
or put it at `~/.local/zig-0.17.1/zig`. The first build fetches about 18 MB of Zig packages into
`~/.cache/vqro/zig-pkg` (override with `LIBGHOSTTY_VT_ZIG_PKG_DIR`), so it needs network once.

```sh
./scripts/dev-install.sh        # release build to ~/.local/bin/vqro, extensions to ~/.local/share/vqro/extensions
vqro tui                        # starts the core in the background if needed (herdr-core must be installed)
vqro status                     # core, extensions and connected UIs (flags out-of-date UIs)
vqro stop                       # stop the core; terminals keep running in herdr-core
```

In the UI, everything starts from the prefix (Ctrl-g by default, rebindable) or by clicking the `vqro` badge:
`n` new tab, `h`/`v` split, `x` close pane, `z` zoom, `s` sidebar, `w` switch workspace (each has its own
tabs), `,` settings, `:` all commands, `u` undo,
`!` safe mode, `q` quit. Settings has pages for extensions, interface, themes, keys (profiles: vqro, tmux,
screen) and more. The [Extensions guide](docs/extensions.md) covers the searchable catalogue, detail/actions and static manifests.
Agents and scripts drive the same primitives with `vqro call <method> '<json>'` and
`vqro run <command> '<json args>'`. Reinstalling restarts the core and any open UI restarts into the new build.

**Native Windows:** experimental. The background-extension console-window storm has a local fix,
with detached-core restart and installed desktop/TUI smokes passing. Services were stopped after testing.
The full gate is still not green and `dev-install.sh` is Unix-only. See the [Windows install attempt and friction log](docs/research/windows-native-install.md)
for prerequisites, the tested local installation, private fixture reproduction and remaining failures. Those native
results cover the human patch predecessor; the consolidated follow-ups still require a native rerun. Treat it as experimental.

Upgrading a pre-rename installation is a one-time offline operation: use the copy-only migration utility in
`scripts/` (`--help` first), with old UIs/launchers closed and the old core stopped. It retains the old state as a
backup, refuses an existing destination and never changes live backends or creates compatibility links. The
operator installs the new binaries/extensions and chooses any temporary endpoint handoff for existing clients.

### New Codex agent

Choose **New Codex agent** from commands. The bundled `codex` extension starts `vqro-bridge-codex` and
`codex app-server --stdio` on the terminal host. When the agent is observed within the launch window, it
records the actual SDK caller's request-route default (human for human starts, coordinator preference for
coordinator starts, with human fallback), creates one empty session and opens a new agent tab. Each launch
gets its own `p:<uuid>` identity. No prompt is sent; Codex's native `admit` delivery remains an explicit choice,
not a fabricated strict start mode. A late/unconfirmed agent instead leaves the observed terminal visible for
manual recovery; no background retry promises later route/session setup.

`codex.settings` / `doc/settings/codex` configure a list of literal `{name, argv, bridge_argv?, bridge_env?,
codex_home?, home?}` runtimes. Optional profile paths resolve on the terminal host and become `CODEX_HOME` / `HOME`;
absent settings use Codex's own normal login/profile, as if started by hand. vqro and its adapter never read,
copy or write credentials or native profile files; Codex owns its own authentication/state. Do not put credentials
in workspace settings. Disabling the launcher does not stop existing agents. Failed UI/routing/session setup
leaves the already-observed host visible for recovery and never replays a launch or native session creation.

### New Claude Code agent (headless route 1)

`claude-code.new` starts `vqro-bridge-claude` and the explicit native Claude executable on the execution
host, using the existing agent UI and request-routing primitives. The separate native-TUI Claude route is
unchanged. No prompt is sent on startup, no bare mode is selected, and vqro does not read/copy native
credentials, profiles or transcripts. Claude owns its own login/settings. Each launch gets a fresh explicit
endpoint principal, not a claim about native tool-process identity.

`claude-code.settings` / `doc/settings/claude-code` configure `{name, argv, bridge_argv?, bridge_env?}`
runtimes. The bridge owns stream formats/permission transport and refuses positional startup prompts,
conflicting transport flags and unsupported launch options. Never put credentials in workspace settings.
Only text admission is declared, not strict-start placement; receipt admission and native completion remain
separate. Literal `client_composed` input is currently source-qualified for CLI **2.1.329**; an unknown
version remains observable without a guessed literal-send capability. Native hooks can still transform input:
receipts retain `may_transform:"unknown"` rather than claiming no transformation.

This first vertical slice projects conversation/tools, bounded consent forms, interrupt, startup metadata and
turn-scoped availability. It does not claim native-TUI parity, complete native queues, transcript history,
child/task control, account restart or settings mutation. Native queue stays unknown; the separately named
vqro input ledger is not native evidence. The private execution-host restart helper enables guarded
`claude.restart`: default refusal for unknown/running background, explicit background-loss override only,
observed old exit before exact-session resume, and no adapter input replay. A missing helper never prevents
ordinary initial launch. Helpers/auth/model behaviour remain separately qualified; see
`docs/research/claude-code-route1.md` for boundaries and evidence.

Development: `cargo test`; `.run/harness.py` (ignored) drives `vqro tui` in a pty for UI checks against a scratch
core (`VQRO_STATE_DIR`, `VQRO_SOCKET`, `VQRO_EXTENSIONS_DIR`, `vqro serve --fake`).

Protocol: `docs/spec/core-protocol.md`.

## Read in this order

1. `docs/vision.md`: why, principles, non-goals, risks
2. `docs/decisions.md`: what is decided (D14 is the center; D15–D19 shape everything since)
3. `docs/architecture.md`: how it fits together
4. `docs/spec/core-protocol.md` (living primitives) and `docs/spec/provider-v0.md` section9 (implemented providers);
   `workspace-v0.md` / `backend-adapter-v0.md` are superseded sketches, not alternative wire APIs
5. `docs/roadmap.md`: milestones, exit tests, kill questions
6. `docs/test-plan.md`: what to test now and why; `docs/experiments/` for details
7. `docs/open-questions.md`
8. `docs/research/`: evidence (backends, agent runtimes, protocols and UI, extension models, prior-art tools, execution hosts, two reviews; `withdrawn/` holds superseded specs)
9. `docs/setup.md`: reference clones and tools for a new machine
