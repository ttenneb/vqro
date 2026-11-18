# prim

One lean, standardized layer of workspace primitives over any multiplexer and any agent runtime. Humans and agents use
the same primitives to build and change their agent workspace; coordination is workspace behaviour too. Backends
(herdr-core, Rex, tmux, Zellij, ...) own execution; prim adapts to them through thin adapters that declare capabilities.

Status: **a working prototype, and fluid.** It is usable daily over herdr-core, but protocols, extension APIs and
UI will keep changing as we learn (`docs/roadmap.md` for direction, `docs/build-plan.md` for the build log).

## Install and run

```sh
./scripts/dev-install.sh        # release build to ~/.local/bin/prim, extensions to ~/.local/share/prim/extensions
prim tui                        # starts the core in the background if needed (herdr-core must be installed)
prim status                     # core, extensions and connected UIs (flags out-of-date UIs)
prim stop                       # stop the core; terminals keep running in herdr-core
```

In the UI, everything starts from the prefix (Ctrl-g by default, rebindable) or by clicking the `prim` badge:
`n` new tab, `h`/`v` split, `x` close pane, `z` zoom, `s` sidebar, `,` settings, `:` all commands, `u` undo,
`!` safe mode, `q` quit. Settings has pages for extensions, interface, themes, keys (profiles: prim, tmux,
screen) and more. The [Extensions guide](docs/extensions.md) covers the searchable catalogue, detail/actions and static manifests.
Agents and scripts drive the same primitives with `prim call <method> '<json>'` and
`prim run <command> '<json args>'`. Reinstalling restarts the core and any open UI restarts into the new build.

Development: `cargo test`; `.run/harness.py` (ignored) drives `prim tui` in a pty for UI checks against a scratch
core (`PRIM_STATE_DIR`, `PRIM_SOCKET`, `PRIM_EXTENSIONS_DIR`, `prim serve --fake`).

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
