# libghostty-vt local patches

None at pin 9ad3c422 (Ghostty main, 2026-11-20). `scripts/check_vendor.py` checks that every patch listed here
(by its path under vendor/patches/libghostty-vt) is applied, and that no unlisted one exists.

Patches dropped when the pin moved from c5a21edf (2026-07-14) to 9ad3c422:

- 0001, default lib-vt panes to grapheme clustering (herdr): the C API now sets default modes
  (`GHOSTTY_TERMINAL_OPT_MODE_DEFAULT`), so vqro sets mode 2027 that way. RIS keeps it.
- 0002, expose the modifyOtherKeys mode (herdr): vqro never used it.
- 0003, reload cell pointers when print grows a page (upstream 33cda4dc, backported): the new pin contains it.
