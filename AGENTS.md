# Working on prim

**prim is a working prototype, and fluid.** It runs daily over herdr-core, but its protocols, extension APIs and
UI are expected to change as we learn. Prefer clear, simple, well-explained code over premature stability; fix
forward; remove what proves wrong. Nothing is frozen until a decision says so.

Read `README.md`, then `docs/decisions.md` (D14 is the center; D15–D19 shape everything since), then
`docs/spec/core-protocol.md` (the living protocol: what the code actually does).

## Rules for every change
- **The center (D14).** One lean, standardized layer of primitives over any multiplexer and agent runtime.
  Coordination is workspace behaviour, not a separate system. Judge a change by: is it lean, standardized, does it
  keep adding a backend or runtime quick, and could an agent make it using only public primitives?
- **Backends own execution (D14).** prim hosts no terminals or agents and builds no persistence or lifetime
  guarantees; adapters declare capabilities and never compensate. prim does supervise its own extensions (D15).
- **No policy in core (D0); opinions are extensions (D17).** Anything that is a choice about how to work (tabs,
  splits, sidebar, settings, themes, keymaps, icons) is an extension using public primitives. The core and
  clients keep only mechanism and recovery, which must work with every extension broken or in safe mode.
- **Generic over specific.** A new client capability is generic (any extension can use it), documented in
  `docs/spec/core-protocol.md`, and forward compatible: unknown values degrade gracefully; panels declare needs in
  `requires`.
- **Workspace state changes only through transactions** (validated, all or nothing, history, revert). Derived,
  frequently changing UI goes through live content (`view.publish`), never transactions.
- **Recovery is sacred (D15).** The `prim` badge and prefix menu always reach commands, undo, reset layout, safe
  mode and quit, whatever the keymap; bad settings fall back (prefix → Ctrl-g, theme → built-in palette).
- **Report, don't promise (D10); no replay.** Effects say what the layer below said happened. Nothing resends
  input to an agent after a restart.
- **Capabilities, not modes (D4).** Agent adapters report what a runtime natively does; they never emulate.
- **Platform-neutral contracts (D12).** No Unix-only concepts in a contract.
- **No colours in code or extensions** beyond the palette (D18): Rust draws with `pal.*`; extensions use semantic
  style names (`accent`, `ok`, `warn`, `error`, `dim`, `muted`, `bold`).
- **Evidence over assumption.** Cite file:line, URL or a test for claims; mark the unverified. Never guess
  codepoints, colours or APIs: check the source.
- **Two implementations before calling a contract stable.**
- **Never write into the `herdr-app` document**; keep Unix socket paths under 103 bytes (macOS).

## How we work
- **Run `scripts/check.sh full` before installing** (`quick` while iterating): one gate for every suite, private and receipted.
- **Small, clean commits** with a why in the message; zero warnings; `cargo test` passing; tests for new core or
  store behaviour. Stdlib-only Python for extensions, using `extensions/prim_ext.py`.
- **Parallel streams** run on git worktrees (`git worktree add -b <stream> ../prim-<stream>`), one branch each,
  reviewed and merged into `main` by a coordinator. Stream agents never merge, push, or touch the live install.
- **Test against a scratch prim, never the live one:**
  `PRIM_STATE_DIR=$PWD/.run/st PRIM_SOCKET=$PWD/.run/p.sock PRIM_EXTENSIONS_DIR=$PWD/extensions ./target/debug/prim serve --fake &`
  (drop `--fake` to use herdr-core, and remove every terminal you create). `.run/` is ignored scratch space.
- **UI checks:** a pty harness (`.run/harness.py`, kept outside git) runs `prim tui` in a pty, sends keys and SGR
  mouse clicks, and renders the screen through tmux; search raw output bytes for exact colours and glyphs.
- **Install for daily use:** `scripts/dev-install.sh` (release build to `~/.local/bin/prim`, extensions to
  `~/.local/share/prim/extensions`, then restarts the core; open UIs reconnect and restart into the new build).
- Record findings in `docs/research/`, binding choices in `docs/decisions.md`, progress in `docs/build-plan.md`,
  direction in `docs/roadmap.md`. Commit only when asked.
