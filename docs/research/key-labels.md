# Full key labels by default (2026-11-17)

Human request; display only. Baseline6f28c53, branch key-labels.

`doc/settings/tui.key_labels` is `full` (default/fallback) or `short`. Native rendering uses one formatter in
`tui/keys.rs`; extensions embedding hints read the same option and use `prim_display.key_labels`. This lean
cross-language route was parent-approved; no new RPC or rich-text protocol. Both implementations consume the
same18-case JSON table, testing default/full/unknown/short modes.

- Prefix prompt, menu, held modifiers, native help and panel paging hints use full names by default.
- Native status-hint templates keep canonical keys and reformat live; arbitrary messages/identifiers are not
  rewritten. `Ctrl-g , → settings` changes back to `C-g , → settings` in short mode.
- Settings exposes a generic Interface select (`settings.set`, `tui: key_labels`), formats Keys-page headers,
  bindings and prefix-choice labels, and preserves canonical option values. Raw editable configuration syntax
  stays accepted; long modifier aliases already accepted by the native parser also round-trip through Settings.
- Key-profile labels/notifications use the shared helper. A display-option change republishes their live labels
  without reconciling/re-writing bindings. Sidebar's client-drawn panel hints follow the native formatter.
- Unknown modes fall back to full. The native default needs no extension and works in safe mode. Matching,
  prefix-twice behavior, terminal encodings and stored key ids are untouched.

Evidence: native rendering was red (`C-g` instead of `Ctrl-g`), then shared-table/native rendering/Python Settings
checks passed. Private physical `tests/e2e_tui_keys.py` captured full and short prompts while the prefix menu
remained open, checked unknown fallback and unchanged binding entries, and retained its modifier-byte delivery
checks on private herdr/tmux. `kl/logs/physical-keys.log` under the agent scratch root holds both captures.
Full gate evidence is in the task closeout receipt/report; no live install or session effects.
