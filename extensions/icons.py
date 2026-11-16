#!/usr/bin/env python3
"""prim extension: icon sets. A few named sets of glyphs, and a command that applies one.

An icon set is data: applying one writes `doc/settings/icons` = `{"name", "width", "glyphs": {name: glyph}}`,
which clients read live (`docs/spec/core-protocol.md`, Icons; D20). Extensions ask for icons by name
(`{"name": "repo", "text": "⎇"}`); the client draws the active set's glyph, else the icon's own glyph. The
available sets are published as live data under the name `icons`
(`{"data": [{"name", "title", "description", "credit", "width", "glyphs"}], "preview": [name], "names": {name: use}}`)
for any extension to draw; settings shows them on its Icons page. The current set is the entry's `name`.

`default` has no glyphs: every icon keeps its extension's own glyph (Nerd Font or Unicode, by the client's
`icons` setting), which is prim's look with no set at all.

Every Nerd Font codepoint below was checked against Nerd Fonts' glyphnames.json (v3.5.1) and carries its
glyph name; every other glyph carries its Unicode name. Emoji are all Emoji_Presentation characters
(East Asian Width W) without variation selectors, so terminals agree they take two cells.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from prim_ext import Extension, PrimError  # noqa: E402

ext = Extension("icons")
ENTRY = "doc/settings/icons"

# The names prim's bundled extensions draw today, and what each marks. A set may leave any out.
NAMES = {
    "repo": "a workspace that is a git repository (sidebar)",
    "worktree": "a git worktree of a workspace (sidebar)",
    "folder": "a workspace that is a plain folder (sidebar)",
    "terminal": "a terminal running an ordinary program (sidebar)",
    "agent": "a terminal whose program reports agent status (sidebar)",
    "settings": "the settings button (status bar)",
    "sidebar": "the sidebar button (status bar)",
    "blocked": "an agent waiting for you; the blocked count (sidebar, status bar)",
    "working": "an agent at work (sidebar)",
    "done": "an agent that finished (sidebar)",
    "error": "an agent that failed (sidebar)",
    "add": "add something: a tab, a workspace (tab bar, sidebar)",
    "filter": "filter a list (sidebar toolbar)",
    "refresh": "refresh derived state, e.g. git (sidebar view)",
}
# The names a preview shows, in order.
PREVIEW = ["repo", "worktree", "folder", "terminal", "agent", "working", "blocked", "done", "error", "settings"]

MATERIAL = {
    "repo": "\U000f0ccf",      # nf-md-source_repository
    "worktree": "\U000f062c",  # nf-md-source_branch
    "folder": "\U000f024b",    # nf-md-folder
    "terminal": "\U000f018d",  # nf-md-console
    "agent": "\U000f06a9",     # nf-md-robot
    "settings": "\U000f0493",  # nf-md-cog
    "sidebar": "\U000f06fd",   # nf-md-page_layout_sidebar_left
    "blocked": "\U000f0e47",   # nf-md-hand_back_right
    "working": "\U000f051f",   # nf-md-timer_sand
    "done": "\U000f05e0",      # nf-md-check_circle
    "error": "\U000f0159",     # nf-md-close_circle
    "add": "\U000f0416",       # nf-md-plus_box
    "filter": "\U000f0232",    # nf-md-filter
    "refresh": "\U000f0450",   # nf-md-refresh
}

SETS = [
    {
        "name": "default", "title": "Default", "width": 1, "glyphs": {},
        "description": "each extension's own glyphs: Nerd Font or Unicode, as chosen above",
        "credit": "prim's look with no icon set",
    },
    {
        "name": "material", "title": "Material Design", "width": 1, "glyphs": MATERIAL,
        "description": "filled, solid shapes; needs a Nerd Font",
        "credit": "Material Design Icons (Pictogrammers) via Nerd Fonts",
    },
    {
        "name": "material-wide", "title": "Material Design, roomy", "width": 2, "glyphs": MATERIAL,
        "description": "a 2-cell column, for non-Mono Nerd Fonts whose glyphs spill past one cell",
        "credit": "Material Design Icons (Pictogrammers) via Nerd Fonts",
    },
    {
        "name": "codicons", "title": "Codicons", "width": 1, "glyphs": {
            "repo": "",      # nf-cod-repo
            "worktree": "",  # nf-cod-git_branch
            "folder": "",    # nf-cod-folder
            "terminal": "",  # nf-cod-terminal
            "agent": "",     # nf-cod-robot
            "settings": "",  # nf-cod-settings_gear
            "sidebar": "",   # nf-cod-layout_sidebar_left
            "blocked": "",   # nf-cod-warning
            "working": "",   # nf-cod-loading
            "done": "",      # nf-cod-pass
            "error": "",     # nf-cod-error
            "add": "",       # nf-cod-add
            "filter": "",    # nf-cod-filter
            "refresh": "",   # nf-cod-refresh
        },
        "description": "thin outlines, as in VS Code; needs a Nerd Font",
        "credit": "Codicons (Microsoft) via Nerd Fonts",
    },
    {
        "name": "unicode", "title": "Unicode geometric", "width": 1, "glyphs": {
            "repo": "⎇",      # ALTERNATIVE KEY SYMBOL
            "worktree": "⑂",  # OCR FORK
            "folder": "▫",    # WHITE SMALL SQUARE
            "terminal": "›",  # SINGLE RIGHT-POINTING ANGLE QUOTATION MARK
            "agent": "◆",     # BLACK DIAMOND
            "settings": "⚙",  # GEAR
            # Not TRIGRAM FOR HEAVEN (the built-in ☰): it is 2 cells wide since Unicode 16.
            "sidebar": "◧",   # SQUARE WITH LEFT HALF BLACK
            "blocked": "⚠",   # WARNING SIGN
            "working": "◐",   # CIRCLE WITH LEFT HALF BLACK
            "done": "✓",      # CHECK MARK
            "error": "✗",     # BALLOT X
            "add": "+",            # PLUS SIGN
            "filter": "/",         # SOLIDUS
            "refresh": "↻",   # CLOCKWISE OPEN CIRCLE ARROW
        },
        "description": "plain shapes, the built-in look without a Nerd Font, with agent states",
        "credit": "Unicode symbols, in most fonts",
    },
    {
        "name": "ascii", "title": "ASCII", "width": 1, "glyphs": {
            "repo": "#", "worktree": "&", "folder": "/", "terminal": ">", "agent": "@", "settings": "=",
            "sidebar": "|", "blocked": "?", "working": "*", "done": "v", "error": "x", "add": "+",
            "filter": "/", "refresh": "~",
        },
        "description": "works in every terminal and font",
        "credit": "ASCII",
    },
    {
        "name": "emoji", "title": "Emoji", "width": 2, "glyphs": {
            "repo": "\U0001f4e6",      # PACKAGE
            "worktree": "\U0001f33f",  # HERB
            "folder": "\U0001f4c1",    # FILE FOLDER
            "terminal": "\U0001f4bb",  # PERSONAL COMPUTER
            "agent": "\U0001f916",     # ROBOT FACE
            "settings": "\U0001f527",  # WRENCH
            "sidebar": "\U0001f4d1",   # BOOKMARK TABS
            "blocked": "✋",       # RAISED HAND
            "working": "⏳",       # HOURGLASS WITH FLOWING SAND
            "done": "✅",          # WHITE HEAVY CHECK MARK
            "error": "❌",         # CROSS MARK
            "add": "➕",           # HEAVY PLUS SIGN
            "filter": "\U0001f50d",    # LEFT-POINTING MAGNIFYING GLASS
            "refresh": "\U0001f504",   # ANTICLOCKWISE DOWNWARDS AND UPWARDS OPEN CIRCLE ARROWS
        },
        "description": "colourful, two cells wide; needs a colour emoji font",
        "credit": "Unicode emoji",
    },
]
BY_NAME = {s["name"]: s for s in SETS}


def publish():
    ext.publish("icons", {"data": SETS, "preview": PREVIEW, "names": NAMES})


@ext.command("icons.apply", "Apply an icon set (args: name, or value from a pick-one menu)")
def apply(args):
    name = args.get("name") or args.get("value")
    s = BY_NAME.get(name)
    if s is None:
        raise PrimError(f"no icon set {name!r}; try one of: {', '.join(BY_NAME)}", "invalid")
    value = {"name": name, "width": s["width"], "glyphs": dict(s["glyphs"])}
    ext.retrying(lambda: ext.apply([{"op": "set", "key": ENTRY, "value": value, "expect": ext.version(ENTRY)}],
                                   f"icons: {name}"))
    return {"icons": name}


publish()
ext.run()
