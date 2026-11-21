# libghostty-vt local patches

In vqro: these are herdr's patches, applied to the tree copied from herdr (see `libghostty-vt.notices.md`).
`scripts/check_vendor.py` checks that each one is applied. The verification commands below are herdr's; vqro's own
checks are the engine conformance suite (libghostty plan G6).

This file tracks intentional local changes applied on top of the vendored
`libghostty-vt` source. Remove a patch only when the vendored source commit
contains the upstream behavior and the listed verification still passes.

## 0001 default lib-vt panes to grapheme clustering

status: active

patch: `vendor/patches/libghostty-vt/0001-default-grapheme-cluster-mode.patch`

herdr issue: https://github.com/herdrdev/herdr/issues/243

upstream discussion: not opened; libghostty-vt currently exposes current mode mutation but no C API for configuring terminal default modes

upstream pr: not opened

vendored base: `c5a21edfcbc2d5b46540ad91b7980aca31f5f1f3`

local files:

- `vendor/libghostty-vt/src/terminal/c/terminal.zig`

reason: Herdr renders terminal cells directly and requires DEC private mode
2027 to store flags, ZWJ emoji, and other multi-codepoint grapheme clusters in
one cell. This patch makes clustering active for new terminals and keeps it as
the reset default so RIS (`ESC c`) does not disable it.

remove when: libghostty-vt exposes a C API for setting default mode 2027, or
upstream makes grapheme clustering the lib-vt default, and the reset-survival
regression passes without this patch.

verification:

```sh
cargo nextest run --locked grapheme_cluster_mode_is_default_and_survives_full_reset
cargo nextest run --locked grapheme_cluster_mode_renders_flag_emoji_in_single_wide_cell
cargo nextest run --locked grapheme_cluster_mode_renders_zwj_family_in_single_wide_cell
```

## 0002 expose modifyOtherKeys mode through terminal data

status: active

patch: `vendor/patches/libghostty-vt/0002-expose-modify-other-keys-mode.patch`

herdr issue: none; fixes the performance regression exposed by
https://github.com/herdrdev/herdr/pull/2303

upstream discussion: not opened

upstream pr: not opened

vendored base: `c5a21edfcbc2d5b46540ad91b7980aca31f5f1f3`

local files:

- `vendor/libghostty-vt/include/ghostty/vt/terminal.h`
- `vendor/libghostty-vt/src/terminal/c/terminal.zig`

reason: Herdr must know whether xterm modifyOtherKeys mode 2 is active to
request printable key releases from the outer terminal. The formatter API can
recover this fact only by formatting the active screen and scrollback. A typed
terminal-data query exposes the authoritative scalar without formatting or
allocation.

remove when: the vendored source exposes an equivalent scalar query for
modifyOtherKeys mode 2 and Herdr can use it without this patch.

verification:

```sh
cargo nextest run --locked modify_other_keys_query_tracks_mode_two
cargo nextest run --locked host_report_all_supplies_printable_releases_for_event_type_only_panes
python3 -m unittest scripts.test_vendor_libghostty_vt scripts.test_ui_hot_path_architecture
```

## 0003 reload cell pointers when print grows a page (upstream backport)

status: active

patch: `vendor/patches/libghostty-vt/0003-reload-cell-pointers-when-print-grows-a-page.patch`

vqro finding: ghostty-audit P0-2 (`qol/libghostty-audit-findings.md`). A 24-byte input (an OSC 8 hyperlink, then
REP of the linked character) and fuzz seed 15851 segfault a ReleaseSafe build inside `Terminal.print`. Plain
printing of about 26k hyperlinked characters does the same.

upstream: Ghostty 33cda4dc5dbfd0478f6891fb4b53844a4fbee17c (2026-08-22). Only its code hunks are backported; its
new Zig test did not apply to the base and is left out.

vendored base: `c5a21edfcbc2d5b46540ad91b7980aca31f5f1f3`

local files:

- `vendor/libghostty-vt/src/terminal/Terminal.zig`

reason: `Terminal.print` held raw cell pointers across operations that can replace the page (a hyperlink growing
it), then wrote and asserted through freed memory.

remove when: the vendored source contains upstream 33cda4dc (a pin bump past 2026-08-22).

verification: `cargo test --lib engines_survive_known_crash_traces`, which replays
`tests/fixtures/terminal/libghostty-replays/p0-2-*`. Built with `LIBGHOSTTY_VT_OPTIMIZE=ReleaseSafe`, it
segfaults without this patch (check.sh step `libghostty-release-safe`).
