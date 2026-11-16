#!/usr/bin/env python3
"""prim extension: settings, as a popup.

`settings.open` asks the invoking client to show `view/settings` as a popup. Its rows are live content
(`view.publish`: not saved, not in history) rebuilt whenever extensions or settings change, so toggles
update in place while the popup is open.

Extensions are switched with the core's `prim.extension.enable|disable`; client options live in
`doc/settings/tui` and each client reads the ones it understands. This extension can't switch itself
off from its own popup: turning settings off from settings would leave you without a way back.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from prim_ext import Extension, PrimError  # noqa: E402

ext = Extension("settings")
TUI = "doc/settings/tui"
OPTIONS = [
    ("mouse", "Capture the mouse", "off: your terminal handles the mouse and selects text", True),
    ("wheel_arrows", "Wheel scrolls full-screen programs", "sends arrow keys when a program has no mouse support", True),
]
STATUS_STYLE = {"running": "ok", "failed": "error", "restarting": "warn"}


def tui():
    return dict({k: d for k, _, _, d in OPTIONS}, **(ext.value(TUI) or {}))




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
    return rows


def key_rows():
    """Every binding: prefix keys and gestures, with what they run and who added them."""
    titles = {}
    try:
        titles = {c["name"]: c["title"] for c in ext.prim.call("command.list")["commands"]}
    except Exception:
        pass
    rows = []
    for k in sorted(k for k in ext.entries if k.startswith("binding/")):
        b = ext.value(k)
        key = b.get("key", "?")
        where = f" on {b['where']}" if b.get("where") else ""
        shown = ("Ctrl-g " + key) if not key.startswith("mouse.") else key.replace("mouse.", "") + "-click" + where
        rows.append({"text": shown, "detail": b.get("owner") or "workspace",
                     "description": b.get("label") or titles.get(b.get("command"), b.get("command"))})
    builtin = [(":", "all commands"), ("o", "next pane"), ("1-9", "tab"), ("u", "undo last change"),
               ("R", "reset layout"), ("!", "safe mode"), ("q", "quit")]
    rows.append({"type": "separator"})
    rows.append({"type": "header", "text": "Built in (recovery, always there)"})
    rows += [{"text": "Ctrl-g " + k, "detail": "core", "description": d} for k, d in builtin]
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
        {"title": "Keys", "items": key_rows()},
        {"title": "About", "items": about},
    ]})


@ext.on_event
def on_event(t, d):
    if t in ("extension", "missed", "safe_mode", "command") or (t == "entries" and any(c["key"].startswith(("extension/", TUI, "binding/")) for c in d["changes"])):
        refresh()


@ext.command("settings.open", "Open settings")
def open_settings(args):
    refresh()
    return {"client": {"popup": {"view": "settings", "width": 72}}}


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


ext.contribute({
    "view/settings": {"kind": "panel", "title": "Settings", "items": [{"type": "text", "text": "no content: the settings extension is starting or stopped"}]},
    "menu/settings.open": {"where": "status", "label": "⚙ settings", "command": "settings.open", "order": 90},
    "binding/settings.open": {"key": ",", "command": "settings.open", "label": "settings"},
})
refresh()
ext.run()
