# vqro OpenTUI client (trial)

A second terminal client for vqro on [OpenTUI](https://github.com/anomalyco/opentui), approved as a trial by the
human (Q-UI-4). It implements the `base`, `keys` and `terminal` profiles (D29, `docs/spec/core-protocol.md`,
"Clients and profiles"). **Optional and separately installed, never bundled** (human decision 2026-11-19, D29):
`scripts/dev-install.sh` and vqro's own installs leave it out; the built-in TUI stays the default and the rescue
client.

    cd extensions/clients/opentui && bun install   # once: pinned packages from bun.lock
    vqro ui opentui                                 # from a checkout whose extensions dir is this one

- `src/model.ts`: the client model, a third independent implementation (after Rust and Python) checked against
  `tests/fixtures/client/*.json` (`bun test`; `scripts/check.sh` step `opentui-model`).
- `src/rpc.ts`: LF JSON-RPC over the core's local endpoint; deadlines, nothing resent.
- `src/app.ts`: layout (splits, tabs), panels (pages, tree, toolbar, chips, select/options, prompts, wrap, follow,
  element and view-kind fallbacks), menus, palette, prefix menu with recovery, the safe-mode chooser, reply hints,
  `ui.notify`, themes and icon sets (ignored in safe mode), reconnect after a core restart. Drawn on OpenTUI's
  buffer by two layers: the screen, and menus/popups above terminal panes.
- `src/terminal.ts`: terminal views on `EmbeddedTerminalRenderable` (Ghostty's VT): attach with backend history,
  ordered `stream.input` (64 KiB chunks, 4 MiB paste limit, `overloaded` retried, nothing resent), keys encoded in
  the program's own keyboard mode, bracketed paste, `stream.resize`, fresh emulator and reattach after a gap.
  Query responses from the emulator are dropped: viewers never answer terminal queries.

Versions (2026-11-19): Bun 1.4.2 (already user-local at `~/.local/bin/bun`), `@opentui/core` 0.9.5 (MIT; its
Linux native library also contains FFmpeg, LGPL-2.1-or-later), TypeScript 7.0.2 and `@types/bun` 1.4.5 for
`bun run check` only. Not yet: `embedded_views`. End to end: `tests/e2e_opentui.py`.
