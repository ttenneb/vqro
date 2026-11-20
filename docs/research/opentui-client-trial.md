# OpenTUI client trial — 2026-11-19

Human decision Q-UI-4: try a Bun/OpenTUI client for the base profile plus terminal views, user-local only,
pinned, with conformance fixtures. Code: `extensions/clients/opentui` (README there). Earlier research:
`<private-notes>`, section 3.

## Install and versions
- No install was needed: a user-local Bun 1.4.2 already existed at `~/.local/bin/bun` (left untouched; OpenTUI
  needs Bun ≥ 1.3.14). Packages install into the client directory from `bun.lock`; the package cache was pointed at
  scratch (`BUN_INSTALL_CACHE_DIR`), so the human's `~/.bun` was not used. Nothing global or system-wide.
- `@opentui/core` 0.9.5 (released the same day; six releases in six days). Runtime packages: OpenTUI core and its
  per-platform native library, `diff` (BSD-3-Clause), `string-width`, `bun-ffi-structs` and friends (MIT).
  **Licence note:** the Linux native library `@opentui/core-linux-x64` is "MIT AND LGPL-2.1-or-later": it includes
  FFmpeg (for OpenTUI's video support). Fine for a trial run from source; needs a decision before vqro ships or
  installs this client (D25 dependency policy).
- Dev only: TypeScript 7.0.2 and `@types/bun` 1.4.5 (`bun run check`, strict mode, passes).

## Milestones
1. **M1** connect: JSON-RPC over the Unix socket with Bun's socket API, hello with profiles/features, snapshot.
2. **M2** model: `src/model.ts`, a third implementation of the client model written from the spec, passes every
   shared fixture case (panels, views, contributions, recovery, window) on the first run. The fixtures did their
   job: three languages agree on flattening, fallbacks, precedence and recovery content.
3. **M3** base profile: layout, panels, menus, palette, prefix menu with recovery, safe-mode chooser, reply hints,
   toasts, themes/icon sets, reconnect after a core restart.
4. **M4** terminal views on `EmbeddedTerminalRenderable` (Ghostty VT).

`tests/e2e_opentui.py` (in `check.sh full`, skipped without Bun or packages) drives it against a private core with
a real shell in a private herdr-core: typing and output, SGR colour interpreted, resize, core restart with
reconnect and reattach to the same shell, prefix/recovery, a Settings popup over a terminal, the dependency
warning, the safe-mode chooser, quit. 15/15.

## What OpenTUI did well
- **Terminal fidelity for free:** Ghostty's emulator, and `encodeKey` encodes keys in the program's own keyboard
  mode (kitty, modifyOtherKeys), which the ratatui client emulates only partially ("disambiguate").
- **Query responses are tagged:** the emulator reports its replies to terminal queries as `source: "response"`, so
  the contract rule "a viewer never answers queries" was one line.
- **Layers:** z-ordered renderables made menus and popups over terminal panes simple.
- **Model reuse:** drawing on OpenTUI's buffer from the shared model mirrored the ratatui client closely; no
  framework-specific state model was needed.

## Problems found
- **Escape timing:** Escape followed immediately by another key reads as Alt+key (normal terminal parsing, but the
  harness must pause). Same class of issue any terminal app has.
- **Packaging:** `bun build` can't bundle the optional per-platform native packages; the client runs from source
  with an 85 MB `node_modules`. Tests symlink it rather than copy it.
- **Resize is asynchronous** end to end (`dispatched` is not applied): tests must poll. Already the documented rule.
- **Not done:** mouse forwarding to programs (the overlay layer receives the mouse), `list_detail` (the plain-list
  fallback shows, as designed), `embedded_views`.

## Contract findings
No contract change was needed. The client contract, the fixtures and the fallback rules were enough to build a
third client without reading the Rust client's code (the protocol spec and fixtures sufficed, plus one lookup of the
attach history parameter type, now named in the spec's Resources table).

## Decision (human, 2026-11-19, Q-UI-5)
The OpenTUI client stays an optional, separately installed client and is never bundled with vqro (so its
dependencies, including the FFmpeg/LGPL native library, are the installer's choice). The built-in TUI stays the
default and the rescue client. Its gaps (mouse forwarding, `list_detail`, `embedded_views`) were closed the
same day (Q-UI-6; `tests/e2e_opentui.py` checks each).
