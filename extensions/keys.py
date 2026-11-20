#!/usr/bin/env python3
"""vqro extension: keys. Named keymap profiles; applying one writes its bindings and the prefix.

A profile is an opinion about keys (D17), so it lives here and not in the client. Its bindings are
`binding/keys.*` entries owned by `keys` with priority 10, so they override extensions' own keys
(priority 0) without deleting them; switching profiles removes the previous profile's bindings.
Switching also sets the client prefix (`doc/settings/tui.prefix`); changing the prefix afterwards
(Settings > Keys) keeps the profile. The active profile is `doc/keys/profile`. The profile list is
published live as view `keys` (`{profile, profiles: [{label, value}]}`) for the settings page.

Turn this extension off (or enter safe mode) and its bindings disappear; the prefix setting stays,
and whatever it is, the client's `vqro` badge still opens the prefix menu with every recovery action.
"""
import os
import sys
import threading

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from vqro_ext import Extension, VqroError  # noqa: E402

from vqro_display import key_labels

ext = Extension("keys", event_types=["entries"])  # no resource events: not used here
STATE = "doc/keys/profile"
TUI = "doc/settings/tui"
PREFIX = "binding/keys."
PRIORITY = 10


def b(key, command, label, **extra):
    return dict(key=key, command=command, label=label, **extra)


def tab_digits():
    """0-9 select tabs counting from 0, as tmux and screen number windows (`client.tab` counts from 1)."""
    return {f"tab-{d}": b(str(d), "client.tab", f"tab {d}", args={"to": d + 1}, hidden=True) for d in range(10)}


SPLIT_SIDE = {"dir": "right"}
SPLIT_STACK = {"dir": "down"}
PICK_TAB = dict(pick="resource")  # choose a terminal; tabs.show activates its tab or opens one

PROFILES = {
    # vqro: the client's defaults and the extensions' own keys, nothing added.
    "vqro": {"label": "vqro (C-g)", "prefix": "C-g", "keys": {}},
    # tmux's default prefix key table (`tmux list-keys -T prefix`), mapped onto vqro commands.
    # Gaps (no vqro command): l last window, ] paste (copy mode is scroll mode: select with the mouse), q display-panes (vqro's q quit stays),
    # $ ( ) rename/cycle sessions, f find, t clock, i info, ~ messages, D/L clients.
    # Differences: x and & close without asking and keep the programs running; s switches workspace (a
    # tmux session), so the sidebar's own s is shadowed here (its status-bar button remains); d quits this UI,
    # which is close to detaching because terminals live in the backend. ! breaks the pane out as in tmux, so
    # safe mode is reached from the prefix menu or the vqro badge. Resize steps are percentages (tmux uses
    # cells): Ctrl-arrows 2%, Alt-arrows 10%. Layouts: M-1 even-h, M-2 even-v, M-3 main-h, M-4 main-v, M-5 tiled.
    "tmux": {"label": "tmux (C-b)", "prefix": "C-b", "keys": {
        "split-h": b("%", "panes.split", "split side by side", args=SPLIT_SIDE),
        "split-v": b('"', "panes.split", "split stacked", args=SPLIT_STACK),
        "close-pane": b("x", "panes.close", "close pane (program keeps running)"),
        "new-tab": b("c", "tabs.open", "new shell in a tab"),
        "next-tab": b("n", "client.tab", "next tab", args={"to": "next"}),
        "prev-tab": b("p", "client.tab", "previous tab", args={"to": "prev"}),
        **tab_digits(),
        "next-pane": b("o", "client.focus", "next pane", args={"dir": "next"}),
        **{f"focus-{d}": b(d.capitalize(), "client.focus", f"pane {d}", args={"dir": d}, hidden=True, repeat=True)
           for d in ("left", "right", "up", "down")},
        "rename-tab": b(",", "tabs.rename", "rename tab", input="Tab name", prefill={"context": "tab_title"}),
        "close-tab": b("&", "tabs.close", "close tab (programs keep running)"),
        "choose": b("w", "tabs.show", "choose a terminal", **PICK_TAB),
        # tmux sessions are the closest thing to vqro workspaces (each has its own windows/tabs).
        "sessions": b("s", "workspaces.switch", "switch workspace"),
        "detach": b("d", "client.quit", "detach (quit this UI)"),
        "commands": b(":", "client.palette", "all commands"),
        "keys": b("?", "client.prefix", "list keys"),
        "copy-mode": b("[", "client.scroll", "scroll back and search"),
        "zoom": b("z", "client.zoom", "zoom pane (toggle)"),
        "last-pane": b(";", "client.focus", "last pane", args={"dir": "last"}),
        "swap-prev": b("{", "panes.swap", "swap with previous pane", args={"dir": "prev"}),
        "swap-next": b("}", "panes.swap", "swap with next pane", args={"dir": "next"}),
        "rotate": b("C-o", "panes.rotate", "rotate panes", args={"dir": "next"}),
        "rotate-back": b("M-o", "panes.rotate", "rotate panes back", args={"dir": "prev"}, hidden=True),
        "break": b("!", "panes.break", "break pane out to a tab"),
        "move-tab": b(".", "tabs.move", "move tab to number", input="Move tab to (1, 2, …)"),
        "next-layout": b("Space", "panes.layout", "next pane layout", args={"preset": "next"}),
        **{f"layout-{n}": b(f"M-{n}", "panes.layout", f"layout {p}", args={"preset": p}, hidden=True)
           for n, p in enumerate(("even-h", "even-v", "main-h", "main-v", "tiled"), 1)},
        **{f"resize-{d}": b(f"C-{d.capitalize()}", "panes.resize", f"resize {d}", args={"dir": d, "by": 2}, hidden=d != "left", repeat=True)
           for d in ("left", "right", "up", "down")},
        **{f"resize-{d}-more": b(f"M-{d.capitalize()}", "panes.resize", f"resize {d} more", args={"dir": d, "by": 10}, hidden=True, repeat=True)
           for d in ("left", "right", "up", "down")},
    }},
    # GNU screen's default key bindings (`man screen`, "Default Key Bindings"), mapped onto vqro.
    # Gaps: C-a (other window) and a (send C-a; here press C-a twice), w (window list in the message
    # line), Q (only this region), ] paste (copy mode is scroll mode: select with the mouse), x lock, H log, M/_ monitor, F fit, i info,
    # ' select by name, z suspend, \ quit all.
    # Differences: k kills the program but its pane stays (showing it ended) until closed; " picks
    # from every terminal rather than this session's windows.
    "screen": {"label": "screen (C-a)", "prefix": "C-a", "keys": {
        "new-tab": b("c", "tabs.open", "new shell in a tab"),
        "new-tab-c": b("C-c", "tabs.open", "new shell in a tab", hidden=True),
        "next-tab": b("n", "client.tab", "next tab", args={"to": "next"}),
        "next-tab-c": b("C-n", "client.tab", "next tab", args={"to": "next"}, hidden=True),
        "next-tab-space": b("Space", "client.tab", "next tab", args={"to": "next"}, hidden=True),
        "prev-tab": b("p", "client.tab", "previous tab", args={"to": "prev"}),
        "prev-tab-c": b("C-p", "client.tab", "previous tab", args={"to": "prev"}, hidden=True),
        "prev-tab-bs": b("Backspace", "client.tab", "previous tab", args={"to": "prev"}, hidden=True),
        **tab_digits(),
        "split-v": b("S", "panes.split", "split stacked", args=SPLIT_STACK),
        "split-h": b("|", "panes.split", "split side by side", args=SPLIT_SIDE),
        "close-pane": b("X", "panes.close", "close pane (program keeps running)"),
        "next-pane": b("Tab", "client.focus", "next pane", args={"dir": "next"}),
        "list": b('"', "tabs.show", "choose a terminal", **PICK_TAB),
        "kill": b("k", "panes.kill", "kill the program"),
        "kill-c": b("C-k", "panes.kill", "kill the program", hidden=True),
        "title": b("A", "tabs.rename", "rename tab", input="Tab name", prefill={"context": "tab_title"}),
        "detach": b("d", "client.quit", "detach (quit this UI)"),
        "detach-c": b("C-d", "client.quit", "detach (quit this UI)", hidden=True),
        "commands": b(":", "client.palette", "all commands"),
        "keys": b("?", "client.prefix", "list keys"),
        "copy-mode": b("[", "client.scroll", "scroll back and search"),
    }},
}
DEFAULT = "vqro"
lock = threading.Lock()


def current():
    name = (ext.value(STATE) or {}).get("profile")
    return name if name in PROFILES else DEFAULT


def binding_ops(name):
    """Ops that make the `keys` bindings exactly this profile's (only what changed)."""
    want = {PREFIX + k: dict(v, owner=ext.name, priority=PRIORITY) for k, v in PROFILES[name]["keys"].items()}
    ops = [{"op": "delete", "key": k} for k in ext.entries if k.startswith(PREFIX) and k not in want]
    ops += [{"op": "set", "key": k, "value": v} for k, v in want.items() if ext.value(k) != v]
    return ops


def publish():
    mode = (ext.value(TUI) or {}).get("key_labels")
    ext.publish("keys", {"profile": current(),
                         "profiles": [{"label": p["label"].replace(p["prefix"], key_labels(p["prefix"], mode)), "value": n}
                                      for n, p in PROFILES.items()]})


def sync():
    """Bring the bindings in line with the active profile (on start, or if the state changed)."""
    def attempt():
        ops = binding_ops(current())
        if ops:
            ext.apply(ops, "keys: sync")
    with lock:
        ext.retrying(attempt)
    publish()


@ext.on_event
def on_event(t, d):
    if t == "missed" or (t == "entries" and any(c["key"] == STATE for c in d["changes"])):
        sync()
    elif t == "entries" and any(c["key"] == TUI for c in d["changes"]):
        publish()  # display change only: never reconcile/rewrite bindings for a label preference


@ext.command("keys.profile", "Apply a keymap profile (args: value = vqro | tmux | screen)")
def apply_profile(args):
    name = args.get("value") or args.get("text")
    if name not in PROFILES:
        raise VqroError(f"unknown profile {name!r} (have: {', '.join(PROFILES)})", "invalid")

    def attempt():
        tui = dict(ext.value(TUI) or {}, prefix=PROFILES[name]["prefix"])
        ops = binding_ops(name) + [
            {"op": "set", "key": STATE, "value": {"profile": name}, "expect": ext.version(STATE)},
            {"op": "set", "key": TUI, "value": tui, "expect": ext.version(TUI)},
        ]
        return ext.apply(ops, f"keys: profile {name}")
    with lock:
        ext.retrying(attempt)
    publish()
    profile = PROFILES[name]
    ext.notify("keys: " + profile["label"].replace(profile["prefix"], key_labels(profile["prefix"], (ext.value(TUI) or {}).get("key_labels"))), "ok")


sync()
ext.run()
