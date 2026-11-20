#!/usr/bin/env python3
"""vqro extension: settings, as a popup.

`settings.open` asks the invoking client to show `view/settings` as a popup. Its rows are live content
(`view.publish`: not saved, not in history) rebuilt whenever extensions or settings change, so toggles
update in place while the popup is open.

Extensions are switched with the core's `vqro.extension.enable|disable`; client options live in
`doc/settings/tui` and each client reads the ones it understands. The Theme page draws the `themes`
extension's live data and applies one with its `themes.apply` command; the Icons page does the same with
the `icons` extension (`icons.apply`), under the client's own Nerd Font choice. Every declared extension uses the
same static manifest and runtime facts; built-in recovery can enable settings again if this page is turned off.

Any extension can add its own page: it publishes live content `settings.page.<name>` = {title, items}
(rows whose commands are its own, over its own state), and Settings shows it after Interface, so the
same switches can live in the extension's own popup and here without a second copy of the state. The
page goes away when the extension stops (its live content is cleared).
"""
import os
import sys
import time
import unicodedata

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from vqro_ext import Extension, VqroError  # noqa: E402
from vqro_display import label, key_labels

PAGE_PREFIX = "settings.page."  # other extensions' own Settings pages (live content)
ext = Extension("settings", event_types=["entries", "view.content", "extension", "provider", "safe_mode", "command"],
                view_content={"names": ["themes", "icons", "keys"], "prefixes": [PAGE_PREFIX]})  # no resource events: not used here
TUI = "doc/settings/tui"
THEME = "doc/settings/theme"
ICONS = "doc/settings/icons"
OPTIONS = [
    ("mouse", "Capture the mouse", "off: your terminal handles the mouse and selects text", True),
    ("wheel_arrows", "Wheel scrolls full-screen programs", "sends arrow keys when a program has no mouse support", True),
    ("copy_on_select", "Copy selected text", "selecting in a terminal copies it through your terminal's clipboard (OSC 52)", True),
]
SCROLLBACK = [1000, 2000, 5000, 10000, 20000]  # the client bounds it to 100..20000
STATUS_STYLE = {"running": "ok", "failed": "error", "starting": "warn", "stopped-by-safe-mode": "warn", "provider-not-connected": "warn"}


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


def manifest_text(manifest, key, fallback=""):
    value = manifest.get(key)
    return label(value, 4096) if isinstance(value, str) else label(fallback, 4096) if isinstance(fallback, str) else ""


def info_row(text, style=None):
    return {"type": "text", "text": label(text, 8192), "wrap": True, "focusable": True, "style": style}


def time_text(at_ms):
    if at_ms is None:
        return "unknown time"
    try:
        return time.strftime("%Y-%m-%d %H:%M:%S %Z", time.localtime(at_ms / 1000))
    except (TypeError, ValueError, OverflowError, OSError):
        return str(at_ms)


# Dependencies (docs/research/declarations-and-ui-clients.md): the core resolves and reports them in
# `extension.list`; this page shows them and asks once before switching. Nothing is switched silently.
NEED_TEXT = {"satisfied": "ok", "off": "off", "missing": "not installed", "version": "other version installed",
             "invalid": "invalid name", "lacking": "some clients lack it", "unknown": "client support unknown"}
NEED_STYLE = {"satisfied": "ok", "off": "warn", "missing": "warn", "version": "warn", "invalid": "error", "lacking": "warn"}
SWITCHABLE = ("off", "stopped-by-safe-mode")  # providers that turning on would start


def ext_title(states, name):
    manifest = (states.get(name) or {}).get("manifest") or {}
    return manifest_text(manifest, "title", name) or name


def need_who(states, need):
    """Who could provide a need: declared providers by title, else the suggested name, else the capability."""
    names = [p.get("name") for p in need.get("providers") or [] if isinstance(p, dict) and p.get("name") != "vqro"]
    if names:
        return " or ".join(ext_title(states, n) for n in names)
    return label(need.get("suggest") or need.get("cap") or "?")


def unmet(deps, field="needs"):
    return [n for n in (deps or {}).get(field) or [] if isinstance(n, dict) and not str(n.get("cap", "")).startswith("ui/")
            and n.get("status") != "satisfied"]


def to_enable(states, deps):
    """Providers to turn on with an extension: per unmet need the suggested provider if declared, else the first."""
    names = []
    for need in unmet(deps):
        offered = [p.get("name") for p in need.get("providers") or [] if (states.get(p.get("name")) or {}).get("display_status") in SWITCHABLE]
        pick = need.get("suggest") if need.get("suggest") in offered else (offered or [None])[0]
        if pick and pick not in names:
            names.append(pick)
    return names


def breaks(states, name, deps):
    """(broken, limited) extension names if `name` stops: enabled users of its capabilities with no other running provider."""
    broken, limited = [], []
    for use in (deps or {}).get("used_by") or []:
        user = use.get("name")
        if (states.get(user) or {}).get("display_status") in (None, "off", "stopped-by-safe-mode"):
            continue
        user_deps = (states.get(user) or {}).get("dependencies") or {}
        rows = [n for f in ("needs", "works_with") for n in user_deps.get(f) or [] if n.get("cap") == use.get("cap")]
        others = [p for r in rows for p in r.get("providers") or [] if p.get("name") not in (name,) and p.get("display_status") in ("running", "core")]
        if others:
            continue
        target = broken if use.get("hard") else limited
        if user not in target:
            target.append(user)
    return broken, [u for u in limited if u not in broken]


def switch_options(states, name, on, deps):
    """One pick-one confirmation when switching has dependency consequences, else None (switch directly)."""
    title = ext_title(states, name)
    if not on:
        needs = unmet(deps)
        if not needs:
            return None
        enable = to_enable(states, deps)
        rest = [need_who(states, n) + " (" + NEED_TEXT.get(n.get("status"), "unavailable") + ")" for n in needs
                if not set(p.get("name") for p in n.get("providers") or []) & set(enable)]
        options = []
        if enable:
            options.append({"label": f"Turn on {title} and {', '.join(ext_title(states, n) for n in enable)}", "value": "with"})
        only = f"Turn on {title} only" if enable else f"Turn on {title} anyway"
        if rest:
            only += "; still needs " + ", ".join(rest)
        return options + [{"label": only, "value": "only"}, {"label": "Cancel", "value": "cancel"}]
    broken, limited = breaks(states, name, deps)
    if not broken:  # optional users only lose extras (listed under Used by): no confirmation
        return None
    what = []
    if broken:
        what.append("breaks " + ", ".join(ext_title(states, n) for n in broken))
    if limited:
        what.append("limits " + ", ".join(ext_title(states, n) for n in limited))
    return [{"label": f"Turn off {title}: " + "; ".join(what), "value": "off"}, {"label": "Cancel", "value": "cancel"}]


def dependency_details(states, name, deps):
    """Needs / Works with / Used by / Provides sections for the detail panel."""
    rows = []
    if not isinstance(deps, dict):
        return rows
    for field, heading in (("needs", "Needs"), ("works_with", "Works with")):
        items = [n for n in deps.get(field) or [] if isinstance(n, dict) and not str(n.get("cap", "")).startswith("ui/")]
        if not items:
            continue
        rows.append({"type": "header", "text": heading})
        for n in items:
            status = n.get("status")
            text = f"{need_who(states, n)}: {label(n.get('cap', '?'))} ({NEED_TEXT.get(status, label(status or '?'))})"
            note = n.get("why") or (n.get("without") and "without it: " + n["without"])
            rows.append(info_row(text + (" · " + label(note, 512) if note else ""), NEED_STYLE.get(status)))
    ui = [n for n in deps.get("needs") or [] if isinstance(n, dict) and str(n.get("cap", "")).startswith("ui/")]
    lacking = sorted({c for n in ui for c in n.get("lacking_in") or []})
    if lacking:
        rows.append(info_row("Client features missing in: " + ", ".join(label(c) for c in lacking) + " (they show a fallback)", "warn"))
    users = deps.get("used_by") or []
    if users:
        rows.append({"type": "header", "text": "Used by"})
        grouped = {}  # one row per user and kind of use, in first-seen order
        for u in users:
            grouped.setdefault((u.get("name"), bool(u.get("hard")), u.get("display_status")), []).append(label(u.get("cap", "?")))
        for (user, hard, status), caps in grouped.items():
            kind = "needs" if hard else "works with"
            rows.append(info_row(f"{ext_title(states, user)} {kind} {', '.join(caps)} ({status or 'unknown'})"))
    provides = ((states.get(name) or {}).get("manifest") or {}).get("provides")
    if isinstance(provides, list) and provides:
        rows += [{"type": "header", "text": "Provides"},
                 info_row(", ".join(label(p) for p in provides if isinstance(p, str)))]
    return rows


def ext_rows(states, commands=None):
    """All declarations follow the same manifest-driven list/detail contract, including this extension."""
    commands = commands or set()
    groups = {}
    for key in sorted(k for k in ext.entries if k.startswith("extension/")):
        name = key.split("/", 1)[1]
        value = ext.value(key) or {}
        state = states.get(name, {})
        safe = bool(state.get("safe_mode"))
        # The core's facts and this cache can briefly disagree; use the facts (same source as the status badge).
        on = bool(state.get("allowed_in_safe_mode")) if safe else state.get("enabled", value.get("enabled", True))
        manifest = state.get("manifest") or {}
        kind = manifest_text(manifest, "kind", value.get("kind", "other"))
        title = manifest_text(manifest, "title", name)
        shown = title if title == name else f"{title} ({name})"
        summary = manifest_text(manifest, "summary", value.get("description") or "No summary provided.").replace("\n", " ")
        status = state.get("display_status") or ("off" if not on else {"running": "running", "failed": "failed", "safe_mode": "stopped-by-safe-mode", "disabled": "off"}.get(state.get("status"), "starting"))
        switch = "vqro.safe_mode.allow" if safe else "vqro.extension.disable" if on else "vqro.extension.enable"
        switch_args = {"name": name, "on": not on} if safe else {"name": name}
        deps = state.get("dependencies")
        confirm = switch_options(states, name, on, deps)
        switch_row = {"command": switch, "args": switch_args}
        if confirm:  # one confirmation, drawn over the popup; Esc cancels
            switch_row = {"command": "settings.switch", "args": {"name": name, "on": not on}, "options": confirm}
        actions = [{"text": "Turn off" if on else "Turn on", **switch_row},
                   {"text": "Restart", "command": "vqro.extension.restart", "args": {"name": name}, "disabled": not on}]
        settings_command = manifest_text(manifest, "settings_command")
        if settings_command:
            actions.append({"text": "Extension settings", "command": settings_command,
                            "disabled": status not in ("running", "provider-not-connected") or settings_command not in commands})
        if "logs.open" in commands:  # the logs extension's viewer, when it runs
            actions.append({"text": "View log", "command": "logs.open", "args": {"name": name}})
        details = [info_row(manifest_text(manifest, "description", value.get("description") or "No description provided.")),
                   {"type": "header", "text": "Runtime"},
                   info_row(f"Status: {status}", STATUS_STYLE.get(status)),
                   info_row(f"Process: {state.get('status', 'stopped')}"),
                   info_row(f"Recent crashes (last {state.get('crash_window_seconds', 300)} s): {state.get('crashes', 0)}"),
                   info_row(f"Total crashes this supervision: {state.get('crashes_total', 'unavailable')}"),
                   info_row("Total is for this declaration in this core; removal or core restart resets it.", "dim"),
                   info_row(f"Last exit: {state.get('last_exit') or 'none recorded'}")]
        failure = state.get("failure") or {}
        if failure:
            details += [info_row(f"Failure at {time_text(failure.get('at_ms'))}: {failure.get('reason', 'unknown reason')}", "error"),
                        info_row(f"At failure: {failure.get('crashes', 0)} exits in {failure.get('window_seconds', 300)} s.")]
        if state.get("status") == "failed":
            details.append(info_row("Stopped until explicitly restarted or changed. Use Restart here, or the built-in failed/recover badge and prefix menu; safe mode still controls admission.", "warn"))
        for exit in state.get("last_exits") or []:
            details.append(info_row(f"Exit {time_text(exit.get('at_ms'))}: {exit.get('reason', 'unknown')}", "dim"))
        details += [{"type": "header", "text": "About"},
                   info_row(f"Kind: {kind}"),
                   info_row(f"Version: {manifest_text(manifest, 'version', 'not provided')}"),
                   info_row(f"Source: {state.get('manifest_source') or 'manifest not provided'}")]
        details[1:1] = dependency_details(states, name, deps)
        if safe:
            details.insert(1, info_row("Safe-mode session choice only; the normal enabled declaration is unchanged.", "warn"))
        if state.get("manifest_error"):
            details.append(info_row(f"Manifest: {state['manifest_error']}", "warn"))
        for field in ("author", "license", "homepage"):
            if manifest_text(manifest, field):
                details.append(info_row(f"{field.title()}: {manifest_text(manifest, field)}"))
        requires = manifest.get("requires")
        if isinstance(requires, dict):
            details.append(info_row(f"Requires protocol: {requires.get('protocol', 'not specified')}"))
            features = requires.get("client_features")
            if isinstance(features, list):
                details.append(info_row("Client features: " + ", ".join(label(f) for f in features if isinstance(f, str))))
        details.append({"type": "header", "text": "Contributes (manifest summary)"})
        contributes = manifest.get("contributes") or {}
        if isinstance(contributes, dict):
            for field in ("commands", "keys", "menu_items", "panels", "settings"):
                items = contributes.get(field)
                if isinstance(items, list) and items:
                    details.append(info_row(field.replace("_", " ").title() + ": " + ", ".join(label(s) for s in items if isinstance(s, str))))
        log_root = ext.info.get("state_dir")
        viewer = "View log above" if "logs.open" in commands else "turn on Extension logs to read it here"
        details.append(info_row(f"Log: {os.path.join(log_root, 'ext-'+name+'.log')} ({viewer})" if log_root else "Log location unavailable."))
        groups.setdefault(kind, []).append({"id": "extension/"+name, "type": "toggle", "text": shown,
            "checked": on, "description": summary, "tags": [name, kind, summary],
            "badges": [{"text": status, "style": STATUS_STYLE.get(status, "dim")}] + [
                {"text": f"needs {need_who(states, n)} ({NEED_TEXT.get(n.get('status'), 'unavailable')})", "style": "warn"}
                for n in unmet(deps)[:2]],
            **switch_row,
            "menu": [{"label": a["text"], "command": a["command"], "args": a.get("args", {})} for a in actions
                     if not a.get("disabled") and not a.get("options")],
            "detail_panel": {"title": shown, "requires": ["items", "chips", "wrap"],
                             "toolbar": [{"type": "chips", "chips": actions}], "items": details}})
    return [{"type": "header", "id": "kind/"+kind, "text": kind.replace("-", " ").title(),
             "expanded": True, "children": groups[kind]} for kind in sorted(groups)]


# Keys page. Mirrors the client's key rules (src/tui/keys.rs, src/tui/mod.rs `rank`) to show
# which binding wins each key; the client stays the authority.
PREFIXES = ["C-g", "C-a", "C-b", "C-Space"]
NAMED = {n.lower(): n for n in ("Up", "Down", "Left", "Right", "Home", "End", "PageUp", "PageDown", "Insert",
                                "Delete", "Tab", "BackTab", "Enter", "Esc", "Backspace", "Space")}
NAMED.update({f"f{n}": f"F{n}" for n in range(1, 13)})
# The client's default keys (recovery ones stay reachable from the vqro badge whatever wins).
BUILTIN = [(":", "all commands"), ("o", "next pane"), ("O", "previous pane"), ("u", "undo last change"),
           ("R", "reset layout"), ("!", "safe mode"), ("q", "quit")] + [(str(n), f"tab {n}") for n in range(1, 10)]


def norm_key(s):
    """Canonical key name (`C-space` -> `C-Space`), or None if it isn't one."""
    # Full modifier labels are already accepted by the native key parser; keep stored ids canonical.
    mods, rest = set(), key_labels(s, "short")
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
        titles = {c["name"]: c["title"] for c in ext.vqro.call("command.list")["commands"] if c.get("available")}
    except Exception:
        pass
    mode = tui().get("key_labels")
    prefix = key_labels(prefix_key(), mode)
    rows = []
    live = ext.live.get("keys") or {}
    if live.get("profiles"):
        rows.append({"type": "select", "text": "Profile", "value": live.get("profile"), "options": live["profiles"],
                     "command": "keys.profile", "description": "a keymap; its keys override extensions' own"})
    current = tui().get("prefix") or "C-g"
    options = [{"label": key_labels(p, mode), "value": p} for p in PREFIXES]
    if current not in PREFIXES:
        options.append({"label": key_labels(current, mode), "value": current})
    options.append({"label": "custom…", "input": f"Prefix key (e.g. {key_labels('C-a', mode)}, {key_labels('M-Space', mode)}, F12)", "prefill": current})
    rows.append({"type": "select", "text": "Prefix", "value": current, "options": options, "command": "settings.prefix",
                 "description": f"falls back to {key_labels('C-g', mode)} if unusable; press it twice to send it to the program"})
    rows += [{"type": "separator"}, {"type": "header", "text": f"Keys after {prefix} (first wins)"}]

    groups = {}
    for k, d in BUILTIN:
        groups.setdefault(k, []).append(((0, 0, ""), {"key": k, "label": d, "owner": "client"}, True))
    for k in sorted(k for k in ext.entries if k.startswith("binding/")):
        bv = ext.value(k)
        raw = bv.get("key", "?")
        scope = bv.get("scope") if bv.get("scope") in ("pane", "composer") else None
        group = f"{raw} on {bv.get('where', 'pane')}" if raw.startswith("mouse.") else (f"{scope}:" if scope else "") + (norm_key(raw) or raw)
        on = binding_on(bv, titles)
        groups.setdefault(group, []).append(((-int(bv.get("priority") or 0), 1, k), bv, on))
    for group in sorted(groups, key=lambda g: (g.startswith("mouse."), g)):
        cands = sorted(groups[group], key=lambda c: (not c[2], c[0]))
        for n, (_, bv, on) in enumerate(cands):
            cmd = bv.get("command")
            what = bv.get("label") or titles.get(cmd, cmd)
            if cmd == "client.none":
                what = "unbound"
            if isinstance(bv.get("when"), dict) and bv["when"]:
                # Conditional keys apply only on matching panes; elsewhere the key falls through.
                what += " (only where " + ", ".join(f"{k.replace('_', ' ')} is {v}" for k, v in bv["when"].items()) + ")"
            if group.startswith("mouse."):
                shown = group.replace("mouse.", "").replace(" on ", "-click on ")
            elif group.startswith(("pane:", "composer:")):  # keys without the prefix (D29)
                where, name = group.split(":", 1)
                shown = f"{key_labels(name, mode)} ({'in the pane' if where == 'pane' else 'while typing'})"
            else:
                shown = f"{prefix} {key_labels(group, mode)}"
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
    states = ext.vqro.call("extension.list")["extensions"]
    commands = {c["name"] for c in ext.vqro.call("command.list")["commands"] if c.get("available")}
    t = tui()
    interface = [{"type": "toggle", "text": label, "description": desc, "checked": bool(t[k]),
                  "command": "settings.toggle", "args": {"tui": k}} for k, label, desc, _ in OPTIONS]
    interface.append({"type":"select", "text":"Key labels", "value":"short" if t.get("key_labels")=="short" else "full",
                      "options":[{"label":"Full (Ctrl-g, Alt-x)","value":"full"},{"label":"Short (C-g, M-x)","value":"short"}],
                      "command":"settings.set", "args":{"tui":"key_labels"},
                      "description":"Display names only; configured key syntax stays unchanged."})
    interface.append({"type": "select", "text": "Scrollback lines", "value": t.get("scrollback_lines", 2000),
                      "options": [{"label": f"{n:,}", "value": n} for n in SCROLLBACK],
                      "command": "settings.set", "args": {"tui": "scrollback_lines"},
                      "description": "per terminal pane; about 6 MB per 1,000 lines at 200 columns; panes opened or reattached after a change use it"})
    info = ext.info
    about = [
        {"text": "vqro", "detail": f"{info.get('server')} {info.get('version')} · protocol {info.get('protocol')}", "disabled": True, "style": "plain"},
        {"text": "socket", "detail": str(info.get("socket")), "disabled": True, "style": "plain"},
        {"text": "state", "detail": str(info.get("state_dir")), "disabled": True, "style": "plain"},
        {"type": "separator"},
        {"type": "text", "text": "Everything here is an extension; see docs/decisions.md D17."},
    ]
    extra = [{"title": label(str(page.get("title") or name[len(PAGE_PREFIX):]), 40), "items": page.get("items") or []}
             for name, page in sorted(ext.live.items()) if name.startswith(PAGE_PREFIX) and isinstance(page, dict)
             and isinstance(page.get("items"), list)]
    ext.publish("settings", {"title": "Settings", "requires": ["pages", "menu", "input", "tree"], "pages": [
        {"title": "Extensions", "list_detail": {"requires": ["list_detail"]}, "items": ext_rows(states, commands)},
        {"title": "Interface", "items": interface}, *extra,
        {"title": "Theme", "items": theme_rows()},
        {"title": "Icons", "items": icon_rows()},
        {"title": "Keys", "items": key_rows()},
        {"title": "About", "items": about},
    ]})


@ext.on_event
def on_event(t, d):
    if t in ("extension", "provider", "missed", "safe_mode", "command") or (t == "view.content" and (d.get("view") in ("themes", "icons", "keys")
                                                                                                or str(d.get("view", "")).startswith(PAGE_PREFIX))) \
            or (t == "entries" and any(c["key"].startswith(("extension/", TUI, THEME, ICONS, "binding/")) for c in d["changes"])):
        refresh()


@ext.command("settings.open", "Open settings")
def open_settings(args):
    refresh()
    return {"client": {"popup": {"view": "settings", "width": 112}}}


@ext.command("settings.set", "Set a client option (args: tui, value)")
def set_option(args):
    key, value = args.get("tui"), args.get("value")
    if not key:
        raise VqroError("needs `tui`", "invalid")
    t = dict(ext.value(TUI) or {}, **{key: value})
    ext.apply([{"op": "set", "key": TUI, "value": t, "expect": ext.version(TUI)}], f"settings: {key}")


@ext.command("settings.switch", "Turn an extension on or off after its dependency confirmation (args: name, on, value)")
def switch(args):
    """`value` is the confirmed choice: `with` also turns on needed providers, `only`/`off` switches just this one,
    `cancel` does nothing. Each extension is switched by the core's own command (or the safe-mode allowance)."""
    name, on, choice = args.get("name"), bool(args.get("on")), args.get("value")
    if not name or choice not in ("with", "only", "off", "cancel"):
        raise VqroError("needs `name` and a confirmed `value`", "invalid")
    if choice == "cancel":
        return {"cancelled": True}
    states = ext.vqro.call("extension.list")["extensions"]
    safe = bool((states.get(name) or {}).get("safe_mode"))
    names = (to_enable(states, (states.get(name) or {}).get("dependencies")) if on and choice == "with" else []) + [name]
    done = []
    for n in names:
        if safe:
            ext.invoke("vqro.safe_mode.allow", {"name": n, "on": on})
        else:
            ext.invoke("vqro.extension.enable" if on else "vqro.extension.disable", {"name": n})
        done.append(n)
    return {"switched": done, "on": on}


@ext.command("settings.toggle", "Toggle an extension (args: extension) or a client option (args: tui)")
def toggle(args):
    if "extension" in args:
        name = args["extension"]
        on = (ext.value(f"extension/{name}") or {}).get("enabled", True)
        return ext.invoke("vqro.extension.disable" if on else "vqro.extension.enable", {"name": name})
    if "tui" in args:
        t = tui()
        t[args["tui"]] = not t[args["tui"]]
        ext.apply([{"op": "set", "key": TUI, "value": t, "expect": ext.version(TUI)}], "settings: interface")


@ext.command("settings.prefix", "Set the client prefix key (args: value or text)")
def set_prefix(args):
    want = (args.get("value") or args.get("text") or "").strip()
    key = norm_key(want)
    if not key or not (key[:2] in ("C-", "M-") or (key[0] == "F" and len(key) > 1)):
        raise VqroError(f"{want!r} can't be a prefix: use {key_labels('C-a or M-Space', tui().get('key_labels'))} or F1-F12", "invalid")
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
