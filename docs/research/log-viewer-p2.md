# Review P2 test: an extension log viewer — 2026-11-19

The adversarial review (2026-11-17, P2 "closed view vocabulary") asked us to give one real extension a different
interaction model, keep it as a production extension, and record every host change it forced, adding only the
smallest primitive that removes a real blocker. The human chose the extension log viewer (Q-UI-3), which also
fills a known gap: Settings said "no in-app log viewer".

## What was built
`extensions/logs.py` (stdlib, public primitives only): `logs.open {name}`, `logs.show`, `logs.refresh`. It reads
the end of `<state dir>/ext-<name>.log` (and the rotated `.1` when the current file is short) on the core host,
bounded to 128 KiB and 400 lines of at most 500 characters, removes control sequences, styles error and warning
lines, and publishes a panel. While a viewer is open (it was opened in the last 10 minutes, or a layout shows
it) it checks the file once a second and republishes only on change. Settings > Extensions gains a View log
action. The viewer opens as a popup or, through `tabs.show`, in a tab.

Its view `view/logs.tail` has its own kind, `logs.tail`, with `fallback: "logs.rows"` (the published panel). No
client renders `logs.tail` natively today, so every client (TUI, text client) shows the fallback: the first real
use of view-kind fallbacks. A future client can render a native tail (for example OpenTUI with a scroll box)
without changing the extension.

## Host changes it forced
1. **View-kind fallback** (D29, `element-and-view-fallbacks.md`). Without it the extension could only publish a
   plain panel under kind `panel`, with no honest way to say "this is a log tail; render it natively if you can".
   With it, popups, tabs and embedded references of the custom kind all work. This was the blocker the review
   named.
2. **`follow` panel hint** (optional feature `follow`): keep the selection on the last row while the reader is
   there; stop when they move up or scroll; resume at the end. The panel catalogue had no way to anchor to the
   end, so a growing log scrolled away from the newest lines. It is one panel field plus a few lines in the
   shared model (`ui_model::panel::follow`); clients without it show the same rows from the top.

Nothing else was needed: filtering (`/`), the extension chooser (`select` row), toolbar chips, focusable text
rows for keyboard reading, semantic styles, popups, `tabs.show {view}`, live content and the release lint all
existed.

## Gaps noticed, not built (no blocker yet)
- **Long lines** are truncated at the panel width (no horizontal scroll or preformatted rows). `wrap:true` would
  fit them but loses one-line-per-entry density. Revisit if a second extension needs preformatted text.
- **Visibility**: an extension can't tell whether a popup showing its view is open (popups are client-local, by
  design), so the viewer tails for a fixed time after it is opened. A stat per second is cheap; a "view in use"
  signal would be a new core concept and isn't justified by this case.
- **Large logs**: bounded reads keep the panel small (about 50 KiB); a native renderer could page through the
  whole file through a dedicated command later.

## Verification
- `tests/e2e_tui_providers.py`: Settings > View log opens the viewer through its fallback; appended lines appear
  without a key press; moving up stops following. `tests/e2e_text_client.py`: the text client shows the same
  viewer through the fallback. Unit test for `follow` in the shared model; release lint now rejects a contributed
  view with a non-base kind and no fallback.
