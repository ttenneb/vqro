#!/usr/bin/env python3
"""prim extension: themes. A few named palettes, and a command that applies one.

A theme is data: applying one writes `doc/settings/theme` = `{"name", "palette": {token: colour}}`, which
clients read live (`docs/spec/core-protocol.md`, Themes). The available themes are published as live data
under the name `themes` (`{"data": [{"name", "title", "credit", "palette", "preview"}], "preview": [token]}`)
for any extension to draw; settings shows them on its Theme page. The current theme is the entry's `name`.

`default` has an empty palette: each client's own built-in look. Its `preview` repeats the TUI's built-in
colours only so it can be shown next to the others.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from prim_ext import Extension, PrimError  # noqa: E402

ext = Extension("themes")
ENTRY = "doc/settings/theme"

# The tokens a preview shows, in order.
PREVIEW = ["accent", "ok", "warn", "error", "prompt", "muted", "border"]

THEMES = [
    {
        "name": "default", "title": "Default (dark)", "credit": "the client's built-in look, in your terminal's colours",
        "palette": {},
        "preview": {"accent": "cyan", "ok": "green", "warn": "yellow", "error": "red", "prompt": "magenta",
                    "muted": "gray", "border": "darkgray"},
    },
    {
        "name": "light", "title": "Light", "credit": "for terminals with a light background",
        "palette": {
            "text": "default", "muted": "#57606a", "dim": "#8c959f", "accent": "#0969da", "ok": "#1a7f37",
            "warn": "#9a6700", "error": "#cf222e", "prompt": "#8250df", "border": "#d0d7de",
            "border_focus": "#0969da", "selection": "#ddf4ff", "selection_text": "#1f2328", "badge": "#6e7781",
            "badge_text": "#ffffff", "surface": "#eaeef2", "tab_active": "#0969da", "tab_active_text": "#ffffff",
        },
    },
    {
        # https://www.nordtheme.com/docs/colors-and-palettes
        "name": "nord", "title": "Nord", "credit": "palette by Arctic Ice Studio",
        "palette": {
            "text": "default", "muted": "#d8dee9", "dim": "#616e88", "accent": "#88c0d0", "ok": "#a3be8c",
            "warn": "#ebcb8b", "error": "#bf616a", "prompt": "#b48ead", "border": "#4c566a",
            "border_focus": "#88c0d0", "selection": "#434c5e", "selection_text": "#eceff4", "badge": "#d8dee9",
            "badge_text": "#2e3440", "surface": "#3b4252", "tab_active": "#5e81ac", "tab_active_text": "#eceff4",
        },
    },
    {
        # https://github.com/morhetz/gruvbox
        "name": "gruvbox", "title": "Gruvbox dark", "credit": "palette by Pavel Pertsev (morhetz)",
        "palette": {
            "text": "default", "muted": "#a89984", "dim": "#928374", "accent": "#fabd2f", "ok": "#b8bb26",
            "warn": "#fe8019", "error": "#fb4934", "prompt": "#d3869b", "border": "#665c54",
            "border_focus": "#fabd2f", "selection": "#504945", "selection_text": "#ebdbb2", "badge": "#a89984",
            "badge_text": "#282828", "surface": "#3c3836", "tab_active": "#458588", "tab_active_text": "#ebdbb2",
        },
    },
]
BY_NAME = {t["name"]: t for t in THEMES}


def publish():
    data = [dict(t, preview=t.get("preview") or {k: t["palette"][k] for k in PREVIEW if k in t["palette"]})
            for t in THEMES]
    ext.publish("themes", {"data": data, "preview": PREVIEW})


@ext.command("themes.apply", "Apply a theme (args: name, or value from a pick-one menu)")
def apply(args):
    name = args.get("name") or args.get("value")
    theme = BY_NAME.get(name)
    if theme is None:
        raise PrimError(f"no theme {name!r}; try one of: {', '.join(BY_NAME)}", "invalid")
    value = {"name": theme["name"], "palette": theme["palette"]}
    ext.retrying(lambda: ext.apply([{"op": "set", "key": ENTRY, "value": value, "expect": ext.version(ENTRY)}],
                                   f"themes: {name}"))
    return {"theme": name}


publish()
ext.run()
