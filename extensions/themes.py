#!/usr/bin/env python3
"""vqro extension: themes. A few named palettes, and a command that applies one.

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
from vqro_ext import Extension, VqroError  # noqa: E402

ext = Extension("themes", event_types=["entries"])  # no resource events: not used here
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
            "badge_text": "#ffffff", "surface": "#eaeef2", "tab_active": "#0969da", "tab_active_text": "#ffffff", "tab_bar": "#f6f8fa",
        },
    },
    {
        # https://www.nordtheme.com/docs/colors-and-palettes
        "name": "nord", "title": "Nord", "credit": "palette by Arctic Ice Studio",
        "palette": {
            "text": "default", "muted": "#d8dee9", "dim": "#616e88", "accent": "#88c0d0", "ok": "#a3be8c",
            "warn": "#ebcb8b", "error": "#bf616a", "prompt": "#b48ead", "border": "#4c566a",
            "border_focus": "#88c0d0", "selection": "#434c5e", "selection_text": "#eceff4", "badge": "#d8dee9",
            "badge_text": "#2e3440", "surface": "#3b4252", "tab_active": "#5e81ac", "tab_active_text": "#eceff4", "tab_bar": "#2e3440",
        },
    },
    {
        # https://github.com/morhetz/gruvbox
        "name": "gruvbox", "title": "Gruvbox dark", "credit": "palette by Pavel Pertsev (morhetz)",
        "palette": {
            "text": "default", "muted": "#a89984", "dim": "#928374", "accent": "#fabd2f", "ok": "#b8bb26",
            "warn": "#fe8019", "error": "#fb4934", "prompt": "#d3869b", "border": "#665c54",
            "border_focus": "#fabd2f", "selection": "#504945", "selection_text": "#ebdbb2", "badge": "#a89984",
            "badge_text": "#282828", "surface": "#3c3836", "tab_active": "#458588", "tab_active_text": "#ebdbb2", "tab_bar": "#1d2021",
        },
    },
    {
        # https://catppuccin.com/palette
        "name": "catppuccin-mocha", "title": "Catppuccin Mocha", "credit": "palette by Catppuccin",
        "palette": {
            "text": "default", "muted": "#bac2de", "dim": "#6c7086", "accent": "#cba6f7", "ok": "#a6e3a1",
            "warn": "#f9e2af", "error": "#f38ba8", "prompt": "#f5c2e7", "border": "#45475a",
            "border_focus": "#cba6f7", "selection": "#313244", "selection_text": "#cdd6f4", "badge": "#7f849c",
            "badge_text": "#1e1e2e", "surface": "#313244", "tab_active": "#89b4fa", "tab_active_text": "#1e1e2e", "tab_bar": "#181825",
        },
    },
    {
        # https://catppuccin.com/palette
        "name": "catppuccin-macchiato", "title": "Catppuccin Macchiato", "credit": "palette by Catppuccin",
        "palette": {
            "text": "default", "muted": "#b8c0e0", "dim": "#6e738d", "accent": "#c6a0f6", "ok": "#a6da95",
            "warn": "#eed49f", "error": "#ed8796", "prompt": "#f5bde6", "border": "#494d64",
            "border_focus": "#c6a0f6", "selection": "#363a4f", "selection_text": "#cad3f5", "badge": "#8087a2",
            "badge_text": "#24273a", "surface": "#363a4f", "tab_active": "#8aadf4", "tab_active_text": "#24273a", "tab_bar": "#1e2030",
        },
    },
    {
        # https://catppuccin.com/palette
        "name": "catppuccin-frappe", "title": "Catppuccin Frappé", "credit": "palette by Catppuccin",
        "palette": {
            "text": "default", "muted": "#b5bfe2", "dim": "#737994", "accent": "#ca9ee6", "ok": "#a6d189",
            "warn": "#e5c890", "error": "#e78284", "prompt": "#f4b8e4", "border": "#51576d",
            "border_focus": "#ca9ee6", "selection": "#414559", "selection_text": "#c6d0f5", "badge": "#838ba7",
            "badge_text": "#303446", "surface": "#414559", "tab_active": "#8caaee", "tab_active_text": "#303446", "tab_bar": "#292c3c",
        },
    },
    {
        # https://catppuccin.com/palette
        "name": "catppuccin-latte", "title": "Catppuccin Latte", "credit": "palette by Catppuccin (light: needs a light terminal)",
        "palette": {
            "text": "default", "muted": "#5c5f77", "dim": "#9ca0b0", "accent": "#8839ef", "ok": "#40a02b",
            "warn": "#df8e1d", "error": "#d20f39", "prompt": "#ea76cb", "border": "#bcc0cc",
            "border_focus": "#8839ef", "selection": "#ccd0da", "selection_text": "#4c4f69", "badge": "#8c8fa1",
            "badge_text": "#eff1f5", "surface": "#ccd0da", "tab_active": "#1e66f5", "tab_active_text": "#eff1f5", "tab_bar": "#e6e9ef",
        },
    },
    {
        # https://draculatheme.com/contribute
        "name": "dracula", "title": "Dracula", "credit": "palette by Zeno Rocha and the Dracula team",
        "palette": {
            "text": "default", "muted": "#f8f8f2", "dim": "#6272a4", "accent": "#bd93f9", "ok": "#50fa7b",
            "warn": "#f1fa8c", "error": "#ff5555", "prompt": "#ff79c6", "border": "#44475a",
            "border_focus": "#bd93f9", "selection": "#44475a", "selection_text": "#f8f8f2", "badge": "#6272a4",
            "badge_text": "#282a36", "surface": "#44475a", "tab_active": "#bd93f9", "tab_active_text": "#282a36", "tab_bar": "#21222c",
        },
    },
    {
        # https://github.com/folke/tokyonight.nvim
        "name": "tokyo-night", "title": "Tokyo Night", "credit": "palette by enkia; values from folke/tokyonight.nvim (night)",
        "palette": {
            "text": "default", "muted": "#a9b1d6", "dim": "#565f89", "accent": "#7aa2f7", "ok": "#9ece6a",
            "warn": "#e0af68", "error": "#f7768e", "prompt": "#bb9af7", "border": "#414868",
            "border_focus": "#7aa2f7", "selection": "#292e42", "selection_text": "#c0caf5", "badge": "#545c7e",
            "badge_text": "#1a1b26", "surface": "#292e42", "tab_active": "#7aa2f7", "tab_active_text": "#1a1b26", "tab_bar": "#16161e",
        },
    },
    {
        # https://ethanschoonover.com/solarized/
        "name": "solarized-dark", "title": "Solarized Dark", "credit": "palette by Ethan Schoonover",
        "palette": {
            "text": "default", "muted": "#93a1a1", "dim": "#586e75", "accent": "#268bd2", "ok": "#859900",
            "warn": "#b58900", "error": "#dc322f", "prompt": "#d33682", "border": "#073642",
            "border_focus": "#2aa198", "selection": "#073642", "selection_text": "#eee8d5", "badge": "#586e75",
            "badge_text": "#002b36", "surface": "#073642", "tab_active": "#268bd2", "tab_active_text": "#fdf6e3", "tab_bar": "#073642",
        },
    },
    {
        # https://github.com/atom/atom/tree/master/packages/one-dark-ui
        "name": "one-dark", "title": "One Dark", "credit": "palette from Atom's One Dark",
        "palette": {
            "text": "default", "muted": "#abb2bf", "dim": "#5c6370", "accent": "#61afef", "ok": "#98c379",
            "warn": "#e5c07b", "error": "#e06c75", "prompt": "#c678dd", "border": "#3e4451",
            "border_focus": "#61afef", "selection": "#3e4451", "selection_text": "#d7dae0", "badge": "#5c6370",
            "badge_text": "#282c34", "surface": "#3e4451", "tab_active": "#61afef", "tab_active_text": "#282c34", "tab_bar": "#21252b",
        },
    },
    {
        # https://rosepinetheme.com/palette/
        "name": "rose-pine", "title": "Rosé Pine", "credit": "palette by Rosé Pine",
        "palette": {
            "text": "default", "muted": "#908caa", "dim": "#6e6a86", "accent": "#c4a7e7", "ok": "#9ccfd8",
            "warn": "#f6c177", "error": "#eb6f92", "prompt": "#ebbcba", "border": "#26233a",
            "border_focus": "#c4a7e7", "selection": "#403d52", "selection_text": "#e0def4", "badge": "#6e6a86",
            "badge_text": "#191724", "surface": "#26233a", "tab_active": "#31748f", "tab_active_text": "#e0def4", "tab_bar": "#191724",
        },
    },
    {
        # https://github.com/rebelot/kanagawa.nvim
        "name": "kanagawa", "title": "Kanagawa", "credit": "palette by Tommaso Laurenzi (rebelot/kanagawa.nvim, wave)",
        "palette": {
            "text": "default", "muted": "#c8c093", "dim": "#727169", "accent": "#7e9cd8", "ok": "#98bb6c",
            "warn": "#e6c384", "error": "#e46876", "prompt": "#957fb8", "border": "#363646",
            "border_focus": "#7e9cd8", "selection": "#2d4f67", "selection_text": "#dcd7ba", "badge": "#727169",
            "badge_text": "#1f1f28", "surface": "#2a2a37", "tab_active": "#7e9cd8", "tab_active_text": "#1f1f28", "tab_bar": "#16161d",
        },
    },
]
# Catppuccin's 14 accents per flavour (https://catppuccin.com/palette). Its ports let you pick one;
# here it drives `accent`, `border_focus` and `tab_active`. Lavender by default.
ACCENT_NAMES = ["rosewater", "flamingo", "pink", "mauve", "red", "maroon", "peach", "yellow", "green", "teal",
                "sky", "sapphire", "blue", "lavender"]
ACCENTS = {
    "catppuccin-mocha": "f5e0dc f2cdcd f5c2e7 cba6f7 f38ba8 eba0ac fab387 f9e2af a6e3a1 94e2d5 89dceb 74c7ec 89b4fa b4befe",
    "catppuccin-macchiato": "f4dbd6 f0c6c6 f5bde6 c6a0f6 ed8796 ee99a0 f5a97f eed49f a6da95 8bd5ca 91d7e3 7dc4e4 8aadf4 b7bdf8",
    "catppuccin-frappe": "f2d5cf eebebe f4b8e4 ca9ee6 e78284 ea999c ef9f76 e5c890 a6d189 81c8be 99d1db 85c1dc 8caaee babbf1",
    "catppuccin-latte": "dc8a78 dd7878 ea76cb 8839ef d20f39 e64553 fe640b df8e1d 40a02b 179299 04a5e5 209fb5 1e66f5 7287fd",
}
ACCENT_TOKENS = ("accent", "border_focus", "tab_active")
for t in THEMES:
    if t["name"] in ACCENTS:
        t["accents"] = {n: "#" + h for n, h in zip(ACCENT_NAMES, ACCENTS[t["name"]].split())}
        t["accent"] = "lavender"
        # Calmer than pink for the prompt line: mauve.
        t["palette"]["prompt"] = t["accents"]["mauve"]
        for k in ACCENT_TOKENS:
            t["palette"][k] = t["accents"]["lavender"]
BY_NAME = {t["name"]: t for t in THEMES}


def publish():
    data = [dict(t, preview=t.get("preview") or {k: t["palette"][k] for k in PREVIEW if k in t["palette"]})
            for t in THEMES]
    ext.publish("themes", {"data": data, "preview": PREVIEW})


def write(name, accent=None):
    theme = BY_NAME.get(name)
    if theme is None:
        raise VqroError(f"no theme {name!r}; try one of: {', '.join(BY_NAME)}", "invalid")
    value = {"name": name, "palette": dict(theme["palette"])}
    if "accents" in theme:
        current = ext.value(ENTRY) or {}
        # Keep the accent you chose when switching between flavours that have one.
        accent = accent or (current.get("accent") if current.get("accent") in theme["accents"] else None) or theme["accent"]
        if accent not in theme["accents"]:
            raise VqroError(f"no accent {accent!r}; try one of: {', '.join(theme['accents'])}", "invalid")
        value["accent"] = accent
        for k in ACCENT_TOKENS:
            value["palette"][k] = theme["accents"][accent]
    ext.retrying(lambda: ext.apply([{"op": "set", "key": ENTRY, "value": value, "expect": ext.version(ENTRY)}],
                                   f"themes: {name}" + (f" ({value['accent']})" if "accent" in value else "")))
    return {"theme": name, "accent": value.get("accent")}


@ext.command("themes.apply", "Apply a theme (args: name, or value from a pick-one menu; accent?)")
def apply(args):
    return write(args.get("name") or args.get("value"), args.get("accent"))


@ext.command("themes.accent", "Choose the current theme's accent, for themes that have accents (args: value)")
def accent(args):
    name = (ext.value(ENTRY) or {}).get("name")
    if name not in BY_NAME or "accents" not in BY_NAME[name]:
        raise VqroError("the current theme has no accent choice", "invalid")
    return write(name, args.get("value") or args.get("accent") or args.get("text"))


publish()
ext.run()
