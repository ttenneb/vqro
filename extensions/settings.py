#!/usr/bin/env python3
"""prim extension: settings, as a popup.

`settings.open` asks the invoking client to show `view/settings` as a popup. Its rows are live content
(`view.publish`: not saved, not in history) rebuilt whenever extensions or settings change, so toggles
update in place while the popup is open.

Extensions are switched with the core's `prim.extension.enable|disable`; client options live in
`doc/settings/tui` and each client reads the ones it understands. The Theme page draws the `themes`
extension's live data and applies one with its `themes.apply` command; the Icons page does the same with
the `icons` extension (`icons.apply`), under the client's own Nerd Font choice. This extension can't switch
itself off from its own popup: turning settings off from settings would leave you without a way back.
"""
import os
import sys
import unicodedata

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from prim_ext import Extension, PrimError  # noqa: E402

ext = Extension("settings", event_types=["entries", "view.content", "extension", "safe_mode", "command"])  # no resource events: not used here
TUI = "doc/settings/tui"
THEME = "doc/settings/theme"
ICONS = "doc/settings/icons"
OPTIONS = [
    ("mouse", "Capture the mouse", "off: your terminal handles the mouse and selects text", True),
    ("wheel_arrows", "Wheel scrolls full-screen programs", "sends arrow keys when a program has no mouse support", True),
]
STATUS_STYLE = {"running": "ok", "failed": "error", "restarting": "warn"}


def tui():
    return dict({k: d for k, _, _, d in OPTIONS}, **(ext.value(TUI) or {}))


def cells(glyph):
    """Terminal cells a glyph takes: 2 for East Asian Wide (emoji), else 1. Enough for previews; the
    client measures for real."""
    return 2 if glyph and unicodedata.east_asian_width(glyph[0]) in ("W", "F") else 1


def icon_rows():
    """The client's own-glyph choice, then the icons extension's sets, each with a preview of its glyphs."""
    rows = [{"type": "select", "text": "Own glyphs", "value": (ext.value(TUI) or {}).get("icons", "auto"),
             "command": "settings.set", "args": {"tui": "icons"},
             "description": "what an icon shows when the set has none for it, and with Default",
             "options": [{"label": "auto (Nerd Font in Ghostty, WezTerm)", "value": "auto"},
                         {"label": "Nerd Font", "value": "nerd"}, {"label": "plain text", "value": "text"}]},
            {"type": "separator"}]
    sets = ext.live.get("icons") or {}
    if not sets.get("data"):
        return rows + [{"type": "text", "text": "No icon sets listed: the icons extension is not running."},
                       {"type": "text", "text": "Icons keep their extensions' own glyphs."}]
    current = (ext.value(ICONS) or {}).get("name") or "default"
    for s in sets["data"]:
        on = s["name"] == current
        # Each glyph padded to the set's column, as the client lays them out.
        glyphs = [s["glyphs"][n] for n in sets.get("preview", []) if n in s.get("glyphs", {})]
        preview = " ".join(g + " " * max(0, s.get("width", 1) - cells(g)) for g in glyphs).rstrip()
        rows.append({"text": ("● " if on else "○ ") + s["title"], "style": "accent" if on else None,
                     "detail": preview, "detail_style": "plain",
                     "description": f"{s.get('description', '')} · {s.get('credit', '')}",
                     "command": "icons.apply", "args": {"name": s["name"]}})
    return rows


def theme_rows():
    """The themes extension's themes, the current one marked, each with colour swatches."""
    themes = ext.live.get("themes") or {}
    if not themes.get("data"):
        return [{"type": "text", "text": "No themes listed: the themes extension is not running."},
                {"type": "text", "text": "The current colours stay as they are."}]
    entry = ext.value(THEME) or {}
    current = entry.get("name") or "default"
    rows = []
    cur = next((t for t in themes["data"] if t["name"] == current), None)
    if cur and cur.get("accents"):
        accent = entry.get("accent") or cur.get("accent")
        rows += [{"type": "select", "text": "Accent", "value": accent, "command": "themes.accent",
                  "options": [{"label": n, "value": n} for n in cur["accents"]]},
                 {"type": "separator"}]
    for t in themes["data"]:
        on = t["name"] == current
        swatches = [{"text": "  ", "color": t["preview"][k]} for k in themes.get("preview", []) if k in t.get("preview", {})]
        rows.append({"text": ("● " if on else "○ ") + t["title"], "style": "accent" if on else None,
                     "description": t.get("credit"), "badges": swatches,
                     "command": "themes.apply", "args": {"name": t["name"]}})
    return rows


def ext_rows(states):
    rows = []
    for key in sorted(k for k in ext.entries if k.startswith("extension/")):
        name = key.split("/", 1)[1]
        on = ext.value(key).get("enabled", True)
        st = states.get(name, {})
        status = st.get("status", "stopped")
        row = {"type": "toggle", "text": name, "checked": on, "detail": status,
               "detail_style": STATUS_STYLE.get(status, "dim"),
               "command": "settings.toggle", "args": {"extension": name}}
        if status == "failed" and st.get("last_exit"):
            row["description"] = st["last_exit"]
        if name == ext.name:
            row.update(disabled=True, style="plain", description="always on here; use the palette to turn it off")
        rows.append(row)
        if name == "agents" and on and status == "running":
            rows.append({"text": "Configure agent runtimes", "command": "agents.settings",
                         "description": "Explicit Pi RPC argv; existing agents are unchanged"})
    return rows


# Keys page. Mirrors the client's key rules (src/tui/keys.rs, src/tui/mod.rs `rank`) to show
# which binding wins each key; the client stays the authority.
PREFIXES = ["C-g", "C-a", "C-b", "C-Space"]
NAMED = {n.lower(): n for n in ("Up", "Down", "Left", "Right", "Home", "End", "PageUp", "PageDown", "Insert",
                                "Delete", "Tab", "BackTab", "Enter", "Esc", "Backspace", "Space")}
NAMED.update({f"f{n}": f"F{n}" for n in range(1, 13)})
# The client's default keys (recovery ones stay reachable from the prim badge whatever wins).
BUILTIN = [(":", "all commands"), ("o", "next pane"), ("O", "previous pane"), ("u", "undo last change"),
           ("R", "reset layout"), ("!", "safe mode"), ("q", "quit")] + [(str(n), f"tab {n}") for n in range(1, 10)]


def norm_key(s):
    """Canonical key name (`C-space` -> `C-Space`), or None if it isn't one."""
    mods, rest = set(), s
    while len(rest) > 2 and rest[1] == "-" and rest[0].upper() in "CMS":
        mods.add(rest[0].upper())
        rest = rest[2:]
    if rest == " ":
        rest = "Space"
    elif len(rest) == 1:
        if "S" in mods:
            return None
        rest = rest.lower() if "C" in mods else rest
    elif rest.lower() in NAMED:
        rest = NAMED[rest.lower()]
    else:
        return None
    return "".join(m + "-" for m in "CMS" if m in mods) + rest


def prefix_key():
    k = norm_key(str(tui().get("prefix") or ""))
    return k if k and (k[:2] in ("C-", "M-") or (k[0] == "F" and len(k) > 1)) else "C-g"


def binding_on(b, titles):
    """Whether a client would use this binding: its owner is on and its command available.
    (No need to check safe mode: this extension isn't running then.)"""
    owner = b.get("owner")
    if owner and not (ext.value(f"extension/{owner}") or {"enabled": False}).get("enabled", True):
        return False
    cmd = b.get("command", "")
    return cmd.startswith("client.") or cmd in titles


def key_rows():
    """Profile and prefix, then every binding grouped by key, the winner first."""
    titles = {}
    try:
        titles = {c["name"]: c["title"] for c in ext.prim.call("command.list")["commands"] if c.get("available")}
    except Exception:
        pass
    prefix = prefix_key()
    rows = []
    live = ext.live.get("keys") or {}
    if live.get("profiles"):
        rows.append({"type": "select", "text": "Profile", "value": live.get("profile"), "options": live["profiles"],
                     "command": "keys.profile", "description": "a keymap; its keys override extensions' own"})
    current = tui().get("prefix") or "C-g"
    options = [{"label": p, "value": p} for p in PREFIXES]
    if current not in PREFIXES:
        options.append({"label": current, "value": current})
    options.append({"label": "custom…", "input": "Prefix key (e.g. C-a, M-Space, F12)"})
    rows.append({"type": "select", "text": "Prefix", "value": current, "options": options, "command": "settings.prefix",
                 "description": "falls back to C-g if unusable; press it twice to send it to the program"})
    rows += [{"type": "separator"}, {"type": "header", "text": f"Keys after {prefix} (first wins)"}]

    groups = {}
    for k, d in BUILTIN:
        groups.setdefault(k, []).append(((0, 0, ""), {"key": k, "label": d, "owner": "client"}, True))
    for k in sorted(k for k in ext.entries if k.startswith("binding/")):
        bv = ext.value(k)
        raw = bv.get("key", "?")
        group = f"{raw} on {bv.get('where', 'pane')}" if raw.startswith("mouse.") else norm_key(raw) or raw
        on = binding_on(bv, titles)
        groups.setdefault(group, []).append(((-int(bv.get("priority") or 0), 1, k), bv, on))
    for group in sorted(groups, key=lambda g: (g.startswith("mouse."), g)):
        cands = sorted(groups[group], key=lambda c: (not c[2], c[0]))
        for n, (_, bv, on) in enumerate(cands):
            cmd = bv.get("command")
            what = bv.get("label") or titles.get(cmd, cmd)
            if cmd == "client.none":
                what = "unbound"
            if group.startswith("mouse."):
                shown = group.replace("mouse.", "").replace(" on ", "-click on ")
            else:
                shown = f"{prefix} {group}"
            row = {"text": shown if n == 0 else "  ↳ " + shown, "detail": bv.get("owner") or "workspace", "description": what}
            if not on:
                row.update(style="dim", detail=f"{row['detail']} · off")
            elif n > 0:
                row.update(style="dim", detail=f"{row['detail']} · overridden")
            elif len(cands) > 1:
                row["badges"] = [{"text": "wins", "style": "ok"}]
            rows.append(row)
    return rows


def refresh(*_):
    states = ext.prim.call("extension.list")["extensions"]
    t = tui()
    interface = [{"type": "toggle", "text": label, "description": desc, "checked": bool(t[k]),
                  "command": "settings.toggle", "args": {"tui": k}} for k, label, desc, _ in OPTIONS]
    info = ext.info
    about = [
        {"text": "prim", "detail": f"{info.get('server')} {info.get('version')} · protocol {info.get('protocol')}", "disabled": True, "style": "plain"},
        {"text": "socket", "detail": str(info.get("socket")), "disabled": True, "style": "plain"},
        {"text": "state", "detail": str(info.get("state_dir")), "disabled": True, "style": "plain"},
        {"type": "separator"},
        {"type": "text", "text": "Everything here is an extension; see docs/decisions.md D17."},
    ]
    ext.publish("settings", {"title": "Settings", "requires": ["pages", "menu", "input"], "pages": [
        {"title": "Extensions", "items": ext_rows(states)},
        {"title": "Interface", "items": interface},
        {"title": "Theme", "items": theme_rows()},
        {"title": "Icons", "items": icon_rows()},
        {"title": "Keys", "items": key_rows()},
        {"title": "About", "items": about},
    ]})


@ext.on_event
def on_event(t, d):
    if t in ("extension", "missed", "safe_mode", "command") or (t == "view.content" and d.get("view") in ("themes", "icons", "keys")) \
            or (t == "entries" and any(c["key"].startswith(("extension/", TUI, THEME, ICONS, "binding/")) for c in d["changes"])):
        refresh()


@ext.command("settings.open", "Open settings")
def open_settings(args):
    refresh()
    return {"client": {"popup": {"view": "settings", "width": 72}}}


@ext.command("settings.set", "Set a client option (args: tui, value)")
def set_option(args):
    key, value = args.get("tui"), args.get("value")
    if not key:
        raise PrimError("needs `tui`", "invalid")
    t = dict(ext.value(TUI) or {}, **{key: value})
    ext.apply([{"op": "set", "key": TUI, "value": t, "expect": ext.version(TUI)}], f"settings: {key}")


@ext.command("settings.toggle", "Toggle an extension (args: extension) or a client option (args: tui)")
def toggle(args):
    if "extension" in args:
        name = args["extension"]
        if name == ext.name:
            raise PrimError("settings can't turn itself off from here", "invalid")
        on = (ext.value(f"extension/{name}") or {}).get("enabled", True)
        return ext.invoke("prim.extension.disable" if on else "prim.extension.enable", {"name": name})
    if "tui" in args:
        t = tui()
        t[args["tui"]] = not t[args["tui"]]
        ext.apply([{"op": "set", "key": TUI, "value": t, "expect": ext.version(TUI)}], "settings: interface")


@ext.command("settings.prefix", "Set the client prefix key (args: value or text, e.g. C-a)")
def set_prefix(args):
    want = (args.get("value") or args.get("text") or "").strip()
    key = norm_key(want)
    if not key or not (key[:2] in ("C-", "M-") or (key[0] == "F" and len(key) > 1)):
        raise PrimError(f"{want!r} can't be a prefix: use Ctrl or Alt with a key (C-a, M-Space) or F1-F12", "invalid")
    t = dict(ext.value(TUI) or {}, prefix=key)
    ext.apply([{"op": "set", "key": TUI, "value": t, "expect": ext.version(TUI)}], "settings: prefix")


ext.contribute({
    "view/settings": {"kind": "panel", "title": "Settings", "items": [{"type": "text", "text": "no content: the settings extension is starting or stopped"}]},
    "menu/settings.open": {"where": "status", "label": "settings", "icon": {"name": "settings", "text": "⚙"},
                           "command": "settings.open", "order": 90},
    "binding/settings.open": {"key": ",", "command": "settings.open", "label": "settings"},
})
refresh()
ext.run()
