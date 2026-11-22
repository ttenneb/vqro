# tmux size and backend readiness fixes (2026-11-17)

Baseline `421024c`; private native tmux 3.7b and herdr-core 0.1.21 only. No install or live-session effects.

## Red → green

- `tests/tmux_provider.rs`: spawn requested110×33 but native pane stayed100×30; stream open requested120×35
  but stayed100×30. Native spawn/open/resize now submit `resize-window` then `resize-pane`, before spawn reply
  / open repaint. Multiple viewers exercise latest-request sizing; native splits retain their minimums. Invalid
  dimensions reject before creating a pane. Assertions check native layout **and actual kernel PTY size**.
- `tests/cases/tmux_lifetime.rs`: fresh owned server incorrectly advertised a zsh bootstrap as a terminal. It now
  retains a marked, exited native `true` pane, excludes it from resources/counts, and starts no login shell.
  Lifecycle tests explicitly spawn their own user terminal; disable/safe-mode/provider/core crashes still preserve it.
  Existing unmarked panes are never hidden/killed, since an older bootstrap may hold user work.
- TUI `backend_notice_is_drawn_even_with_an_alternative_backend_and_tracks_events` failed to render the supplied
  warning. The warning now precedes contributed status buttons and follows authoritative snapshot/event facts,
  including configured-default failure when another backend is connected. Palette `warn`, no new colours.
- `tests/backend_notices.rs`: a provider extension running `/bin/cat` incorrectly displayed `running` readiness.
  Raw supervisor status stays truthful (`running`), but `display_status` is `provider-not-connected` until an
  authenticated owned provider is connected and synced. Different registration names work. Settings displays both
  facts, refreshes on provider events, and keeps available configuration commands usable for recovery.
- `tests/e2e_tui_providers.py`: actual private TUI verifies warning, disconnected-provider/process detail, and live
  recovery without reopening Settings. Search waits for the popup to exist before sending keys.

## Native sizing is not a synthetic acknowledgement

`new-window` has no width/height flags in tmux3.7b. Spawn resizing is submitted before the observed reply, not
before native program exec. Window sizing becomes native manual sizing; pane geometry remains layout-constrained;
other native clients may resize it, and closing a viewer does not restore a previous size. Capabilities and the
living provider protocol declare these limitations.

The first full gate exposed a previously ineffective E2E resize check: its native starting size already matched
its requested target on old code. Once spawn/open correctly request80×24, a subsequent100×30 resize can update
native layout before tmux's kernel PTY resize timer expires. Direct observation proved layout100×30 while the
PTY was still80×24; this is not a settled resize acknowledgement. tmux3.7b deliberately coalesces resize queues
on a250ms timer (`server_client_check_pane_resize`):
https://github.com/tmux/tmux/blob/3.7b/server-client.c#L1589-L1657

`tests/e2e_providers.py` now asserts `dispatched`, observes the owned PTY read-only with `TIOCGWINSZ`, and only then
asks the shell once. The focused Rust tests also poll actual PTY sizes with `stty -F`; they do not merely trust
pane metadata, increase a sleep, or replay input. This preserves D10 and native tmux timing rather than compensating
in prim.

## Verification

Focused native tmux provider/lifetime suite:9/9. Core readiness integration, TUI render regression, settings unit
tests and both physical provider E2Es passed. Full gate receipt is recorded in the task closeout report under
`<private-notes>`; initial failure evidence is retained there too. Tests use the
existing per-agent Cargo target, offline -j8, nice10 and eight-CPU affinity; the full gate owns its cgroup.
