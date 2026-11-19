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

Upgrading a pre-rename installation is a one-time offline operation: use the copy-only migration utility in
`scripts/` (`--help` first), with old UIs/launchers closed and the old core stopped. It retains the old state as a
backup, refuses an existing destination and never changes live backends or creates compatibility links. The
operator installs the new binaries/extensions and chooses any temporary endpoint handoff for existing clients.

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
