# prim

One lean, standardized layer of workspace primitives over any multiplexer and any agent runtime. Humans and agents use
the same primitives to build and change their agent workspace; coordination is workspace behaviour too. Backends
(herdr-core, Rex, tmux, Zellij, ...) own execution; prim adapts to them through thin adapters that declare capabilities.

Status: slice 1 working (`docs/build-plan.md`).

## Try it

```sh
cargo build
./target/debug/prim serve &                 # needs herdr-core's `hc` on PATH (or --fake)
./target/debug/prim tui                     # Ctrl-g n: new shell, Ctrl-g : commands, Ctrl-g q: quit
# install the example extension, as an agent would:
./target/debug/prim call workspace.apply '{"ops":[{"op":"set","key":"extension/sidebar",
  "value":{"argv":["python3","'$PWD'/extensions/sidebar.py"]}}]}'
./target/debug/prim run sidebar.toggle      # or Ctrl-g s in the TUI
./target/debug/prim run prim.revert         # undo the last change not made by an extension
```

Protocol: `docs/spec/core-protocol.md`.

## Read in this order

1. `docs/vision.md`: why, principles, non-goals, risks
2. `docs/decisions.md`: what is decided (start with D14, then D15 and D16)
3. `docs/architecture.md`: how it fits together
4. `docs/spec/`: `workspace-v0.md` (the primitives) and `backend-adapter-v0.md` (how backends plug in)
5. `docs/roadmap.md`: milestones, exit tests, kill questions
6. `docs/test-plan.md`: what to test now and why; `docs/experiments/` for details
7. `docs/open-questions.md`
8. `docs/research/`: evidence (backends, agent runtimes, protocols and UI, extension models, prior-art tools, execution hosts, two reviews; `withdrawn/` holds superseded specs)
9. `docs/setup.md`: reference clones and tools for a new machine
