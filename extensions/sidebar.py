#!/usr/bin/env python3
"""vqro extension: the sidebar, one tree: workspace → worktree → tab → pane, plus "Not in a tab". Layouts no
workspace claims are no section of their own: `main` is a workspace-like "Scratch" row (`scratch`).

Data comes from public sources only: resources from the core, layouts (`layout/*`) and the `workspaces`
extension's live data. Each workspace (and each of its git worktrees) has its own layout (named in the
workspaces data); its tabs are listed in order, the selected one marked ●, and under each tab its panes:
terminals and agents as resource rows (an agent carries its bridge terminal as a collapsed child), other
panels by title. Terminals that no tab of any layout shows sit under the deepest workspace or worktree
folder they work in, in a "Not in a tab" group; terminals in no listed folder and no tab are "Other
terminals". Layouts no workspace claims (`main`, or every layout when the workspaces extension is off)
are listed under "Layouts" like workspaces. Status badges come from each terminal's OSC 7501 report
(or a tier-1 agent's live report) and roll up to its parents, each resource counted once.

Every level can be shown or hidden (`doc/settings/sidebar` `show`: workspaces, worktrees, tabs, panes,
stray); a hidden level's rows are replaced by their children, so any combination works: panes straight
under workspaces (tabs hidden), tabs without panes, or a flat list (workspaces hidden). Worktree options
(`worktrees`: limit per workspace, default 10 with changed ones first, and a "+N more" row; only with changes;
hide detached) narrow which
worktrees are listed. The same rows appear in the Show popup (`sidebar.view`, from the toolbar's
`Show: <preset> ▾` chip) and as this extension's Settings page (live `settings.page.sidebar`): one state.

Clicking a tab or layout shows that layout in the window you clicked in (the reply's
`{"client": {"layout"}}`), makes it the shared default for new windows (`workspace/active`) and selects
the tab; clicking a resource shows it. Each window marks the layout it shows itself (`current_when`
rows), since this content is shared. Showing/hiding the sidebar applies to every layout.
Rows are live content (`view.publish`): not saved, not in history, so each publication is bounded. Ended
terminals no tab shows fold into one "N ended" row per place, opened by hand. A tree that fits `WHOLE_BYTES`
(60 KB) is published whole: no pager. A place closed by its ▸ is one row at any size: clients with lazy rows
(`lazy_children`) report the ▸ (`sidebar.place`), and a closed place's contents are sent only once it opens
again. A larger tree first shows the places that are
not open as single rows (open: the default workspace, the ones recently shown as default, or expanded by
hand; "Expand here"/"Collapse" in row menus). If that still does not fit, the default is one scrolling list cut
to a page's budget, ending with a row naming the places left (`fit`); pages (by bytes, keeping folder ancestors
and each agent/bridge group together) are a setting, "Split into pages" (`pages`). The toolbar's "/" searches every row of the whole tree (collapsed, hidden or on another page too)
and lists matches with their place in a popup; the panel's own filter still narrows the visible rows.
"""
import copy
import json
import os
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from vqro_ext import Extension, VqroError, views_in  # noqa: E402
from vqro_views import ended, title, unknown_since, view_resource

# `provider` keeps the registry cache current: an unavailable agent shows when its provider disconnected.
ext = Extension("sidebar", event_types=["entries", "resource", "resource.removed", "provider", "view.content"],
                view_content={"names": ["workspaces", "agents.terminal_status"]})
PREFS = "doc/settings/sidebar"
LAYOUT_KEYS = ("layout/", "doc/layout/")  # tabs and their layouts' titles
# One tree: workspace → worktree → tab → pane (+ "Not in a tab"). Each level can be hidden; its children
# then move up. `stray`: terminals in no workspace folder and no tab.
LEVELS = [("workspaces", "Workspaces", "workspace and layout rows"), ("worktrees", "Worktrees", "git worktrees under their repo"),
          ("tabs", "Tabs", "each tab, and \"Not in a tab\""), ("panes", "Panes", "terminals, agents and panels"),
          ("stray", "Other terminals", "in no workspace and no tab")]
LEVEL_SWITCH = {"workspace": "workspaces", "worktree": "worktrees", "tab": "tabs", "pane": "panes"}
SHOW_DEFAULT = {key: True for key, _, _ in LEVELS}
PRESETS = {
    "everything": dict(SHOW_DEFAULT),
    "panes under workspaces": dict(SHOW_DEFAULT, tabs=False),
    "tabs only": dict(SHOW_DEFAULT, panes=False, stray=False),
    "workspaces and worktrees": {"workspaces": True, "worktrees": True, "tabs": False, "panes": False, "stray": False},
    "terminals only": {"workspaces": False, "worktrees": False, "tabs": False, "panes": True, "stray": True},
}
# Short names for the toolbar chip, so the current choice fits a ~24-column sidebar on one line.
SHORT = {"everything": "all", "panes under workspaces": "no tabs", "tabs only": "tabs",
         "workspaces and worktrees": "trees", "terminals only": "terminals"}
# A workspace lists its first 10 worktrees (changed ones first) and a "+N more" row that opens the rest by hand: a
# repository with 185 worktrees (each a ~460-byte row) otherwise fills two sidebar pages that a collapsed ▸ only hides
# (collapse is the window's own; the published tree still holds them). 0 means no limit.
WORKTREES_DEFAULT = {"limit": 10, "changed_only": False, "hide_detached": False}
LIMITS = [0, 3, 5, 10, 20]
expanded_worktrees = set()  # workspaces whose "+N more" was clicked this run (derived view state, not history)
# Meaning roles (core-protocol.md, Themes): working is in progress (`info`), blocked needs you, done succeeded.
STATUS = {"working": "info", "blocked": "warn", "error": "error", "done": "ok"}
# What a row is, at a glance, by icon name (the active icon set draws it, D20). Without a set, Nerd Font
# glyphs where the client has one, plain Unicode otherwise. The icon's style carries its state.
ICON = {
    "repo": {"name": "repo", "nerd": "\ue702", "text": "⎇"},              # nf-dev-git
    "worktree": {"name": "worktree", "nerd": "\ue725", "text": "⑂"},      # nf-dev-git_branch
    "folder": {"name": "folder", "nerd": "\uf07b", "text": "▫"},          # nf-fa-folder
    "terminal": {"name": "terminal", "nerd": "\uf489", "text": "›"},      # nf-oct-terminal
    "agent": {"name": "agent", "nerd": "\U000f06a9", "text": "◆"},        # nf-md-robot
    "add": {"name": "add", "text": "+"},
    "filter": {"name": "filter", "text": "/"},
    "refresh": {"name": "refresh", "text": "↻"},
    "blocked": {"name": "blocked", "text": "⚠"},
    "sidebar": {"name": "sidebar", "text": "☰"},
    "panel": {"name": "panel", "text": "▤"},
    "loose": {"name": "loose", "text": "…"},
}
last = [None, None, None, None]  # what was last published: sidebar, status segment, view popup, settings page
SETTINGS_PAGE = "settings.page.sidebar"  # shown by the settings extension as a page (generic `settings.page.*`)
refresh_pending = threading.Event()
PUBLISH_INTERVAL = 0.5  # Coalesce status bursts into at most two sidebar updates/second.
PAGE_BYTES = 48 * 1024  # A page fills to about this many encoded bytes: one bounded publication (< 64 KiB with ancestors).
# A tree up to this size is published whole, with no pager and no compaction: unpaged, no ancestor repeats, so it
# can use more of the same < 64 KiB per-publication budget than a page.
WHOLE_BYTES = 60 * 1024
OPEN_RECENT = 8  # Places last shown as the default stay open when the tree must be compacted.
recent_places, expanded_places, collapsed_places = [], set(), set()  # derived view state of this run, not history
expanded_ended = set()  # "N ended" rows opened by hand (derived view state of this run, not history)
last_active = [None]  # the default layout when `recent_places` was last updated
page_state = [0, 1]  # Shared sidebar page and current page count; derived, never history.


def show():
    saved = dict((ext.value(PREFS) or {}).get("show") or {})
    if "terminals" in saved and "panes" not in saved:
        saved["panes"] = saved["terminals"]  # older preference name for what are now panes
    return {key: bool(saved.get(key, default)) for key, default in SHOW_DEFAULT.items()}


def current_preset():
    return next((name for name, p in PRESETS.items() if p == show()), None)


def status_of(r):
    """The terminal's OSC 7501 status; without one, a tier-1 agent's report via live data `agents.terminal_status` (lossy, advisory)."""
    st = r.get("status")
    if isinstance(st, dict) and st:
        return st
    reports = (ext.live.get("agents.terminal_status") or {}).get("data")
    report = reports.get(r.get("id")) if isinstance(reports, dict) else None
    tier1 = report.get("status") if isinstance(report, dict) else None
    return tier1 if isinstance(tier1, dict) else {}


def capability(provider, name):
    declared = (ext.providers.get(provider) or {}).get("capabilities")
    return isinstance(declared, dict) and isinstance(declared.get(name), dict)


def host_killable(r):
    """The agent's host is a live terminal whose provider declares terminal.kill."""
    host = ext.resources.get(r.get("host")) or {}
    return host.get("kind") == "terminal" and not ended(host) and capability(host.get("provider"), "terminal.kill")


def declares_stop(r):
    """The agent itself declares agent.stop (its own caps within its provider's bound, agent v0 section 2.4)."""
    facts = (r.get("data") or {}).get("agent")
    own = facts.get("caps") if isinstance(facts, dict) else None
    return isinstance(own, dict) and isinstance(own.get("agent.stop"), dict) and capability(r.get("provider"), "agent.stop")


def folder_of(r):
    """Short name of a terminal's current folder ("~" for home), or None."""
    cwd = r.get("cwd")
    if not cwd:
        return None
    return "~" if os.path.realpath(cwd) == os.path.realpath(os.path.expanduser("~")) else os.path.basename(cwd.rstrip("/" + os.sep)) or cwd


def term_row(rid, r, where=False):
    """A terminal row. With `where` (not placed under a folder in the tree), an unnamed terminal also
    shows its folder, so a column of "zsh" rows can be told apart; named ones keep their room for badges."""
    label = title(r, ext.resources)
    folder = folder_of(r) if where and not r.get("name") else None
    if folder and folder != label:
        label = f"{label} · {folder}"
    st = status_of(r)
    badges = []
    if ended(r):
        badges.append({"text": "ended", "style": "dim"})
    elif not r.get("available", True) and r.get("kind") == "agent":
        badges.extend(disconnected_badges(r))
    elif not r.get("available", True):
        badges.append({"text": "unavailable", "style": "warn"})
    elif st.get("state"):
        badges.append({"text": st["state"], "style": STATUS.get(st["state"])})
    if isinstance(st.get("progress"), (int, float)):
        badges.append({"text": f"{int(st['progress'])}%", "style": "info"})
    live = r.get("state") == "live"
    # A terminal whose program reports status (OSC 7501) is an agent; its icon takes the status colour,
    # and a set with a glyph for the state shows that instead of the agent's (shape, not only colour).
    agent = r.get("kind") == "agent" or bool(st.get("state") or st.get("app"))
    icon_style = STATUS.get(st.get("state")) if live and st.get("state") else ("muted" if live else "dim")
    icon = ICON["agent" if agent else "terminal"]
    if agent and st.get("state") in STATUS:
        icon = dict(icon, name=[st["state"], "agent"])
    return {
        "id": f"term:{rid}", "text": label, "style": ("agent" if agent else None) if live else "dim",
        "icon": icon, "icon_style": icon_style,
        "badges": badges,
        "command": "sidebar.show", "args": {"resource": rid},
        "menu": [
            {"label": "Show", "command": "sidebar.show", "args": {"resource": rid}},
            *([{"label": "Stop", "command": "sidebar.kill", "args": {"resource": rid}}]
              if r.get("kind", "terminal") == "terminal" and live else []),
            # Agents declaring data.agent config/commands (agent v0 6.3); the agents extension owns the popup.
            *([{"label": "Agent settings…", "command": "agents.config", "args": {"resource": rid}}]
              if r.get("kind") == "agent" and live and isinstance((r.get("data") or {}).get("agent"), dict)
              and (r["data"]["agent"].get("config") or r["data"]["agent"].get("commands")) else []),
            # Agents with a killable bridge terminal, or that declare agent.stop themselves (agent v0), can be
            # stopped from vqro; the agents extension asks first.
            *([{"label": "Stop agent…", "command": "agents.stop", "args": {"resource": rid}}]
              if r.get("kind") == "agent" and not ended(r) and (host_killable(r) or declares_stop(r)) else []),
            # Explicit only: retention never forgets an agent by time.
            *([{"label": "Forget agent", "command": "agents.forget", "args": {"resource": rid}}]
              if r.get("kind") == "agent" and not ended(r) and not r.get("available", True) else []),
        ],
    }


def disconnected_badges(r):
    """An unavailable agent: its bridge terminal finished (being removed), or state unknown since when."""
    host = ext.resources.get(r.get("host")) if r.get("host") else None
    if host and host.get("kind") == "terminal" and ended(host):
        return [{"text": "ended", "style": "dim"}]
    since = unknown_since(ext, r)
    return [{"text": "unknown since " + since if since else "unknown", "style": "warn"}]


def counts_of(rows):
    """Blocked/working resources beneath `rows`, each counted once even if several tabs show it."""
    counts, seen = {}, set()

    def walk(rs):
        for x in rs:
            rid = x.get("id", "")
            if rid.startswith("term:") and rid.split("@")[0] not in seen:
                seen.add(rid.split("@")[0])
                for b in x.get("badges", []):
                    if b["text"] in ("blocked", "working"):
                        counts[b["text"]] = counts.get(b["text"], 0) + 1
            walk(x.get("children", []))
    walk(rows)
    return counts


def status_counts():
    """Blocked/working resources anywhere (also inside collapsed places or other pages)."""
    counts = {}
    for r in ext.resources.values():
        state = status_of(r).get("state") if r.get("state") == "live" and r.get("available", True) else None
        if state in ("blocked", "working"):
            counts[state] = counts.get(state, 0) + 1
    return counts


def rollup(rows):
    """Badges for a parent: how many terminals beneath it are blocked or working."""
    c = counts_of(rows)
    out = []
    if c.get("blocked"):
        out.append({"text": f"{c['blocked']} blocked", "style": "warn"})
    if c.get("working"):
        out.append({"text": f"{c['working']} working", "style": "info"})
    return out


def badged(badges):
    """Rows carry `badges` only when there are some."""
    return {"badges": badges} if badges else {}


def git_badges(w):
    b = []
    if w.get("branch"):
        b.append({"text": w["branch"]})
    if w.get("dirty"):
        b.append({"text": f"±{w['dirty']}", "style": "changed"})
    if w.get("ahead"):
        b.append({"text": f"↑{w['ahead']}", "style": "info"})
    return b


def first_tabs_path(node, path=()):
    """(path, node) of a layout's first tabs node (where new tabs go and digits select), or None."""
    if "tabs" in node:
        return list(path), node
    for i, child in enumerate(node.get("children", [])):
        found = first_tabs_path(child, (*path, i))
        if found:
            return found
    return None


def active_layout_name():
    return (ext.value("workspace/active") or {}).get("layout")


def layout_tabs(name):
    """(tabs path, tabs node) of layout `name`, or (None, None)."""
    root = ext.value(f"layout/{name}") if name else None
    found = first_tabs_path(root) if isinstance(root, dict) else None
    return found if found else (None, None)


def shown_resources():
    """Every resource a layout shows somewhere (any layout, any tab), so "Not in a tab" means exactly that."""
    out = set()
    for key, entry in ext.entries.items():
        if key.startswith("layout/") and isinstance(entry.get("value"), dict):
            for v in views_in(entry["value"]):
                rid = view_resource(ext, v)
                if rid:
                    out.add(rid)
    return out


def resource_row(rid, where, place=""):
    """A terminal or agent row (an agent carries its bridge terminal as a collapsed child). `place` keeps
    ids unique when one resource is shown in several tabs; `term:<rid>` stays the prefix."""
    r = ext.resources.get(rid)
    if r is None:
        return None
    row = term_row(rid, r, where=where)
    if place:
        row["id"] = f"term:{rid}@{place}"
    host = ext.resources.get(r.get("host")) if r.get("kind") == "agent" else None
    if host and host.get("kind") == "terminal":
        bridge = term_row(host["id"], host)
        if place:
            bridge["id"] = f"term:{host['id']}@{place}"
        row["children"] = [bridge]
        row["expanded"] = False
    return row


def pane_rows(name, path, index, tab, hide_ended):
    """The panes of one tab: terminals and agents as resource rows, other panels by their title."""
    rows, seen = [], set()
    for v in views_in(tab.get("node") or {}):
        if v in seen or v == "sidebar":
            continue
        seen.add(v)
        rid = view_resource(ext, v)
        if rid:
            r = ext.resources.get(rid)
            if r is None or (hide_ended and ended(r)):
                continue
            row = resource_row(rid, where=False, place=f"{name}/{index}/{v}")
        else:
            value = ext.value(f"view/{v}") or {}
            row = {"id": f"pane:{name}/{index}/{v}", "text": value.get("title") or v, "icon": ICON["panel"], "icon_style": "muted",
                   "command": "sidebar.tab", "args": {"layout": name, "path": path, "tab": index}}
        if row:
            rows.append(dict(row, _level="pane"))
    return rows


def tab_rows(name, hide_ended=False):
    """One row per tab of layout `name` (the selected one marked ●), each holding its panes."""
    path, tabs = layout_tabs(name)
    if tabs is None:
        return []
    rows = []
    for i, tab in enumerate(tabs.get("tabs", [])):
        selected = i == tabs.get("active", 0)
        panes = pane_rows(name, path, i, tab, hide_ended)
        rows.append({"id": f"tab:{name}:{i}", "text": tab.get("title") or f"tab {i+1}", "_level": "tab",
                     "icon": "●" if selected else "○", "icon_style": "accent" if selected else "dim",
                     **badged(rollup(panes)), "children": panes, "expanded": True,
                     "command": "sidebar.tab", "args": {"layout": name, "path": path, "tab": i}})
    return rows


def fold_ended(key, rows, lazy=True):
    """Ended terminals that no tab shows, as one "N ended" row: retention keeps up to 64 of them, and one row each
    used to push a daily sidebar past one page. With `lazy` its rows are published only while it is opened by hand
    (`sidebar.ended`); search builds without `lazy`, so they stay findable. Live rows are returned unchanged."""
    plain = lambda row: {k: v for k, v in row.items() if k != "_ended"}
    live = [plain(row) for row in rows if not row.get("_ended")]
    done = [plain(row) for row in rows if row.get("_ended")]
    if not done:
        return live
    fid = f"ended:{key}"
    opened = fid in expanded_ended or not lazy
    fold = {"id": fid, "text": f"{len(done)} ended", "style": "dim", "icon": ICON["loose"], "icon_style": "dim",
            "_level": "pane", "expanded": opened, "children": done if opened else [],
            "command": "sidebar.ended", "args": {"id": fid},
            "menu": [{"label": "Hide them" if opened else "Show them", "command": "sidebar.ended", "args": {"id": fid}}]}
    return live + [fold]


def loose_group(key, rows, lazy=True):
    """Terminals of a place that no tab shows (the ended ones folded, see `fold_ended`). A tab-level row: hiding
    tabs moves them up."""
    if not rows:
        return []
    kids = fold_ended(key, rows, lazy)
    if all(row.get("_ended") for row in rows):
        # Only ended terminals: their "N ended" row sits in the place itself, without a "Not in a tab" group around
        # one row (the human's "Not in a tab › 1 ended").
        return kids
    return [{"id": f"loose:{key}", "text": "Not in a tab", "style": "dim", "icon": ICON["loose"], "icon_style": "dim",
             "_level": "tab", **badged(rollup(rows)), "expanded": True, "children": kids}]


def current_when(name):
    """Each window marks the workspace it shows itself (`current_when`): the content is shared."""
    return {"current_when": {"layout": name}} if name else {}


def layout_rows(claimed, hide_ended=False):
    """Layouts no workspace claims (`main`, ...), each with its tabs. Workspace-level rows."""
    names = sorted(k[len("layout/"):] for k in ext.entries if k.startswith("layout/") and k[len("layout/"):] not in claimed)
    rows = []
    for name in names:
        settings = ext.value(f"doc/layout/{name}") or {}
        tabs = tab_rows(name, hide_ended)
        rows.append({"id": f"layout:{name}", "text": settings.get("title") or name, "expanded": True, "_level": "workspace",
                     "icon": ICON["folder"], "icon_style": "muted", **current_when(name), "children": tabs, **badged(rollup(tabs)),
                     "command": "sidebar.layout", "args": {"layout": name}})
    return rows


SCRATCH = "Scratch"


def scratch(layouts):
    """With workspaces listed, layouts no workspace claims are not a section of their own (the human's choice (a),
    feedback batch 4): `main`, which holds what belongs to no workspace, is one workspace-like row named "Scratch"
    (unless the layout has its own title), and any other unclaimed layout shows only while it is the default one
    (so it stays reachable while in use). Nothing is deleted here."""
    active = active_layout_name()
    out = []
    for row in layouts:
        name = row["id"][len("layout:"):]
        if name == "main":
            titled = (ext.value("doc/layout/main") or {}).get("title")
            out.insert(0, dict(row, text=titled or SCRATCH))
        elif name == active:
            out.append(row)
    return out


def worktree_prefs():
    return dict(WORKTREES_DEFAULT, **((ext.value(PREFS) or {}).get("worktrees") or {}))


def visible_worktrees(w, prefs, limited=True):
    """The worktrees this workspace lists (settings: hide detached, only changed, limit) and how many are cut.
    Without `limited` the limit (a display cut) is not applied: search lists every worktree the filters keep."""
    wts = [t for t in w.get("worktrees", [])
           if not (prefs["hide_detached"] and t.get("branch") == "(detached)")
           and not (prefs["changed_only"] and not (t.get("dirty") or t.get("ahead")))]
    limit = prefs["limit"]
    if limited and type(limit) is int and limit > 0 and w["path"] not in expanded_worktrees and len(wts) > limit:
        # Cut to the ones with changes (uncommitted or unpushed) first, the rest in their own order.
        changed = [t for t in wts if t.get("dirty") or t.get("ahead")]
        wts = changed + [t for t in wts if not (t.get("dirty") or t.get("ahead"))]
        return wts[:limit], len(wts) - limit
    return wts, 0


def skip_hidden(rows, sh):
    """Drop the internal level marks; a hidden level's row is replaced by its children (they move up).
    Hidden panes go with their own children (an agent's bridge), and empty "Not in a tab" groups go."""
    out = []
    for row in rows:
        level = row.get("_level")
        row = {k: v for k, v in row.items() if k != "_level"}
        if level == "pane" and not sh["panes"]:
            continue
        kids = skip_hidden(row["children"], sh) if level and "children" in row else row.get("children")
        if level and not sh[LEVEL_SWITCH[level]]:
            out.extend(kids or [])
            continue
        if level and "children" in row:
            if row["id"].startswith("loose:") and not kids:
                continue
            row["children"] = kids
        out.append(row)
    return out


def build(sh=None, compact=True):
    """One tree: workspace → worktree → tab → pane, each level skippable; see `skip_hidden`.

    `compact`: if the whole tree would not be published whole (`WHOLE_BYTES`), workspaces and layouts that are
    not open are published as single rows (see `collapse`), so typical use needs no pages."""
    sh = sh or show()
    data = (ext.live.get("workspaces") or {}).get("data")
    hide_ended = (ext.value(PREFS) or {}).get("hide_ended", False)
    prefs = worktree_prefs()
    shown = shown_resources()
    agents = {r.get("host") for r in ext.resources.values() if r.get("kind") == "agent"}
    # Folders a terminal in no tab can belong to (listed workspaces and worktrees); the deepest match wins.
    places, homes = [], []
    for w in data or []:
        wts, cut = visible_worktrees(w, prefs, limited=compact)
        places.append((w, wts, cut))
        homes.append((os.path.realpath(w["path"]), w["path"]))
        homes += [(os.path.realpath(t["path"]), t["path"]) for t in wts]
    homes.sort(key=lambda h: -len(h[0]))
    loose, stray = {}, []
    for rid, r in sorted(ext.resources.items(), key=lambda kv: int(kv[0][1:]) if kv[0][1:].isdigit() else 0):
        if rid in agents or rid in shown or (hide_ended and ended(r)):
            continue
        host = ext.resources.get(r.get("host")) if r.get("kind") == "agent" else None
        folder = r.get("cwd") or (host or {}).get("cwd")
        cwd = os.path.realpath(folder) if folder else None
        home = next((key for p, key in homes if cwd and (cwd == p or cwd.startswith(p + os.sep))), None)
        row = dict(resource_row(rid, where=True), _ended=ended(r))
        if home is not None and data is not None:
            loose.setdefault(home, []).append(dict(row, _level="pane"))
        else:
            stray.append(row)

    rows, layouts = [], []
    if data is not None:
        for w, wts, cut in places:
            kids = tab_rows(w.get("layout"), hide_ended) + loose_group(w["path"], loose.get(w["path"], []), compact)
            for t in wts:
                inner = tab_rows(t.get("layout"), hide_ended) + loose_group(t["path"], loose.get(t["path"], []), compact)
                kids.append({
                    "id": f"wt:{t['path']}", "text": os.path.basename(t["path"].rstrip("/" + os.sep)), "expanded": True, "_level": "worktree",
                    "icon": ICON["worktree"], "icon_style": "changed" if t.get("dirty") else "muted",
                    "badges": git_badges(t) + rollup(inner), **current_when(t.get("layout")), "children": inner,
                    "command": "workspaces.open", "args": {"path": t["path"]},
                    "menu": [{"label": "Switch to its tabs", "command": "workspaces.open", "args": {"path": t["path"]}},
                             {"label": "Open shell here", "command": "workspaces.shell", "args": {"path": t["path"]}}],
                })
            if cut:
                kids.append({"id": f"more:{w['path']}", "text": f"+{cut} more worktrees", "style": "control", "_level": "worktree",
                             "command": "sidebar.worktrees_more", "args": {"path": w["path"]}, "children": []})
            menu = [{"label": "Switch to its tabs", "command": "workspaces.open", "args": {"path": w["path"]}},
                    {"label": "Open shell here", "command": "workspaces.shell", "args": {"path": w["path"]}}]
            # "New worktree…" asks for a branch name, so it stays in the command list, not this right-click menu
            # (human feedback batch 5).
            menu += [{"label": "Refresh git state", "command": "workspaces.refresh"},
                     {"label": "Remove from workspaces", "command": "workspaces.remove", "args": {"path": w["path"]}}]
            rows.append({
                "id": f"ws:{w['path']}", "text": w["name"], "expanded": True, "_level": "workspace",
                "icon": ICON["repo" if w.get("git") else "folder"],
                "icon_style": "error" if w.get("missing") else ("changed" if w.get("dirty") else "muted"),
                "style": "error" if w.get("missing") else "bold",
                "badges": ([{"text": "missing", "style": "error"}] if w.get("missing") else git_badges(w)) + rollup(kids), **current_when(w.get("layout")),
                "children": kids, "command": "workspaces.open", "args": {"path": w["path"]}, "menu": menu,
            })
        claimed = {w.get("layout") for w in data} | {t.get("layout") for w in data for t in w.get("worktrees", [])}
        layouts = scratch(layout_rows(claimed, hide_ended))
    else:
        rows, layouts = [], layout_rows(set(), hide_ended)
    note_active(rows + layouts)
    stray = fold_ended("stray", stray, compact)
    items = assemble(rows, layouts, stray, data is not None, sh)
    if compact and sh["workspaces"]:
        # Places closed by hand (▸, which a client with lazy rows reports) are published as one row each, whatever
        # the size; a tree still over WHOLE_BYTES also closes the places that are not open.
        known = {row["id"] for row in rows + layouts}
        closed = collapsed_places & known
        # Always: open places carry their on_toggle too, so a ▸ closed at any size reaches here (238).
        items = assemble(collapse(rows, closed), collapse(layouts, closed), stray, data is not None, sh)
        if encoded(items) > WHOLE_BYTES:
            closed = known - open_places(rows + layouts)
            items = assemble(collapse(rows, closed, large=True), collapse(layouts, closed, large=True), stray, data is not None, sh)
    return items or [{"type": "text", "text": "nothing to show"}]


def assemble(rows, layouts, stray, have_ws, sh):
    """Headers and sections around the place rows, with hidden levels skipped."""
    items = []
    if have_ws:
        if sh["workspaces"]:
            items.append({"type": "header", "text": "Workspaces"})
        items += skip_hidden(rows, sh)
        if sh["workspaces"]:
            items.append({"id": "add", "text": "add workspace…", "style": "control", "icon": ICON["add"], "icon_style": "control",
                          "command": "workspaces.add", "input": "Folder path"})
        # Scratch (and an unclaimed layout in use) sit with the workspaces: no "Layouts" section (see `scratch`).
        place = len(items) - (1 if sh["workspaces"] else 0)
        items[place:place] = skip_hidden(layouts, sh)
        if sh["stray"] and stray:
            items.append({"type": "header", "text": "Other terminals"})
            items += stray
    else:
        # Without workspace data (extension off): every layout as a place, terminals in no tab after them.
        items += skip_hidden(layouts, sh)
        if stray and (sh["stray"] or sh["panes"]):
            items.append({"type": "header", "text": "Terminals"})
            items += stray
    return items


def encoded(value):
    return len(json.dumps(value, ensure_ascii=False, separators=(",", ":")).encode())


def note_active(places):
    """Remember the places shown most recently as the default (bounded): they stay open when collapsing.
    Only a change of the default counts, so collapsing the current place by hand sticks."""
    active = active_layout_name()
    if active == last_active[0]:
        return
    for row in places:
        layouts = {row.get("current_when", {}).get("layout")} | {c.get("current_when", {}).get("layout") for c in row.get("children", [])}
        if active and active in layouts:
            if row["id"] in recent_places:
                recent_places.remove(row["id"])
            recent_places.insert(0, row["id"])
            del recent_places[OPEN_RECENT:]
            # Going there only switches: a place closed by its ▸ stays closed (opening it is the ▸'s job).
            last_active[0] = active  # only once the place is known (workspace data may still be loading)
            return


def open_places(places):
    known = {row["id"] for row in places}
    return ((set(recent_places) | expanded_places) - collapsed_places) & known


def collapse(places, closed, large=False):
    """Places in `closed` become one row (badges keep their roll-up): a lazy row (feature `lazy_children`) whose ▸
    asks for its contents (`sidebar.place`). Clients without the feature ignore those fields and show a plain row
    whose "Expand here" menu opens it. Open places report their own ▸ the same way; in a `large` tree their menu can
    also close them here."""
    out = []
    for row in places:
        toggle = {"command": "sidebar.place", "args": {"id": row["id"]}}
        if row["id"] not in closed:
            menu = (row.get("menu") or []) + ([{"label": "Collapse", "command": "sidebar.collapse", "args": {"id": row["id"]}}] if large else [])
            out.append(dict(row, menu=menu, on_toggle=toggle))
            continue
        # Small on purpose (hundreds of them at scale): its full menu comes back once it is open.
        kept = {k: v for k, v in row.items() if k not in ("children", "expanded", "menu", "_level")}
        kept["menu"] = [{"label": "Expand here", "command": "sidebar.expand", "args": {"id": row["id"]}}]
        out.append(dict(kept, has_children=True, expanded=False, on_toggle=toggle, _level=row.get("_level")))
    return out


def paginate(items, page, budget=None, whole=None):
    """Slice logical tree rows into pages of about `budget` encoded bytes, keeping ancestor context.

    A resource and its one bridge child are a unit. Folder ancestors can repeat across pages; no
    resource is omitted. Typical trees are one page, so no pager shows; it is a last resort at scale.
    """
    def children(row):
        return [] if row.get("id", "").startswith("term:") else row.get("children", [])
    starts, sizes = [0], []
    def measure(rows):
        for row in rows:
            if row.get("type") == "header":
                continue
            own = row if row.get("id", "").startswith("term:") else {k: v for k, v in row.items() if k != "children"}
            sizes.append(encoded(own))
            measure(children(row))
    measure(items)
    whole = (WHOLE_BYTES if budget is None else budget) if whole is None else whole
    budget = PAGE_BYTES if budget is None else budget
    used = 0
    for i, size in enumerate(sizes if sum(sizes) > whole else []):  # a tree that fits `whole` is one page
        if used and used + size > budget:
            starts.append(i)
            used = 0
        used += size
    total, pages = len(sizes), len(starts)
    page = max(0, min(page, pages-1))
    start, end = starts[page], (starts[page+1] if page+1 < pages else total)
    index = 0
    def window(rows):
        nonlocal index
        selected, header = [], None
        for row in rows:
            if row.get("type") == "header":
                header = row
                continue
            own = start <= index < end
            index += 1
            kids = window(children(row))
            if own or kids:
                if header is not None:
                    selected.append(header)
                    header = None
                kept = dict(row)
                if "children" in row and not row.get("id", "").startswith("term:"):
                    kept["children"] = kids
                selected.append(kept)
        return selected
    return window(items), page, pages, total


def fit(items, budget=None):
    """One scrolling list (the default; pages are a setting): the whole tree when it fits one publication
    (`WHOLE_BYTES`), else rows in order while they fit a page's budget, then one row saying how many places are left
    (search still reaches them).
    A place that alone is too large is shown closed (its ▸ or "Expand here" opens it)."""
    total = sum(1 for row in items if row.get("type") != "header")
    if encoded(items) <= (WHOLE_BYTES if budget is None else budget):
        return items, total
    # Cut to a page's budget: the same headroom under the publication bound as a page has.
    room = (PAGE_BYTES if budget is None else budget) - 256  # for the closing row
    kept, used, left = [], encoded([]), 0
    for row in items:
        size = encoded(row) + 1
        if left == 0 and used + size > room and row.get("id", "").startswith(("ws:", "layout:")):
            closed = collapse([row], {row["id"]})[0]
            closed.pop("_level", None)
            if used + encoded(closed) + 1 <= room:
                row, size = closed, encoded(closed) + 1
        if left or used + size > room:
            left += row.get("type") != "header" and row.get("id", "").startswith(("ws:", "layout:", "term:"))
            continue
        kept.append(row)
        used += size
    if left:
        kept.append({"id": "more-places", "type": "text", "style": "dim",
                     "text": f"{left} more place{'s' if left != 1 else ''}: search (/) or open one; pages are a Sidebar setting"})
    return kept, total


def toolbar():
    """One line: what is shown (opens the view popup) and the filter."""
    short = SHORT.get(current_preset(), "custom")
    rows = [{"type": "chips", "chips": [
        {"text": f"Show: {short} ▾", "command": "sidebar.view"},
        {"text": "", "icon": ICON["filter"], "command": "sidebar.search", "input": "Search the sidebar"},
    ]}]
    if page_state[1] > 1:
        rows.append({"type": "chips", "chips": [
            {"text": "‹", "command": "sidebar.page", "args": {"delta": -1}},
            {"text": f"{page_state[0]+1}/{page_state[1]}", "command": "sidebar.page", "input": "Sidebar page number"},
            {"text": "›", "command": "sidebar.page", "args": {"delta": 1}},
        ]})
    return rows


def option_rows():
    """The sidebar's options: shared by the Show popup and the Settings page (one state: doc/settings/sidebar)."""
    sh, wt = show(), worktree_prefs()
    have_ws = ext.live.get("workspaces") is not None
    rows = [{"type": "header", "text": "Show"}]
    for k, label, what in LEVELS:
        row = {"type": "toggle", "text": label, "checked": sh[k], "detail": what,
               "command": "sidebar.preset", "args": {"show": {k: not sh[k]}}}
        # Say why a toggle can't matter right now instead of hiding it.
        if not have_ws and k == "worktrees":
            row.update(disabled=True, detail="workspaces extension off")
        rows.append(row)
    rows += [{"type": "header", "text": "Worktrees"},
             {"type": "select", "text": "Per workspace", "value": wt["limit"], "command": "sidebar.worktrees", "args": {"key": "limit"},
              "options": [{"label": "all" if n == 0 else f"at most {n}", "value": n} for n in sorted(set(LIMITS) | {wt["limit"]})]},
             {"type": "toggle", "text": "Only with changes", "checked": bool(wt["changed_only"]), "detail": "uncommitted or ahead of the repo's branch",
              "command": "sidebar.worktrees", "args": {"key": "changed_only", "value": not wt["changed_only"]}},
             {"type": "toggle", "text": "Hide detached", "checked": bool(wt["hide_detached"]), "detail": "no branch checked out",
              "command": "sidebar.worktrees", "args": {"key": "hide_detached", "value": not wt["hide_detached"]}},
             {"type": "header", "text": "Size"},
             {"type": "toggle", "text": "Split into pages", "checked": bool((ext.value(PREFS) or {}).get("pages")),
              "detail": "off: one scrolling list (a very large tree ends with a row naming what is left)",
              "command": "sidebar.pages", "args": {"value": not (ext.value(PREFS) or {}).get("pages")}},
             {"type": "separator"}, {"type": "header", "text": "Presets"}]
    current = current_preset()
    rows += [{"text": ("● " if name == current else "○ ") + name, "style": "accent" if name == current else None,
              "command": "sidebar.preset", "args": {"preset": name}} for name in PRESETS]
    return rows


def view_panel():
    """The `sidebar.view` popup: the options, then actions too rare for the toolbar."""
    have_ws = ext.live.get("workspaces") is not None
    rows = option_rows()
    rows += [{"type": "separator"},
             {"text": "Choose any resource…", "command": "sidebar.show", "pick": "resource"},
             {"type": "text", "text": "Filter applies to this page; the resource chooser lists all resources."},
             {"text": "Close/hide ended items", "command": "sidebar.close_ended"},
             {"text": "Show ended items", "command": "sidebar.show_ended"}]
    if have_ws:
        rows += [{"type": "separator"},
                 {"text": "Refresh git state", "icon": ICON["refresh"], "icon_style": "plain", "command": "workspaces.refresh"},
                 {"text": "Add workspace…", "icon": ICON["add"], "icon_style": "plain", "command": "workspaces.add",
                  "input": "Folder path"}]
    return {"title": "Sidebar view", "items": rows}


def settings_page():
    """This extension's page in Settings (live `settings.page.sidebar`): the same rows as the Show popup."""
    return {"title": "Sidebar", "items": option_rows()}


publish_lock = threading.Lock()  # one publication at a time: each publishes what it built just before


def refresh_now():
    """A user's own action (a page, expand, collapse, "+N more", an ended fold) publishes at once, in the command's
    thread: the worker's coalescing wait (PUBLISH_INTERVAL) is for status bursts, and made the pager lag."""
    refresh_pending.clear()
    try:
        refresh()
    except Exception as error:
        ext.log("sidebar publication failed:", error)
        refresh_pending.set()  # the worker tries again


def refresh(*_):
    with publish_lock:
        publish_snapshot()


def publish_snapshot():
    # The worker reads a coherent cache, but never holds its lock across RPC/publication.
    with ext.cache_lock:
        all_items = build()
        if (ext.value(PREFS) or {}).get("pages"):
            items, page_state[0], page_state[1], total = paginate(all_items, page_state[0])
        else:
            items, total = fit(all_items)
            page_state[0], page_state[1] = 0, 1
        content = {"requires": ["tree", "badges", "menu", "toolbar", "chips", "icon", "icon_names"], "toolbar": toolbar(), "items": items,
                   "page": {"number": page_state[0]+1, "count": page_state[1], "rows": total}}
        view = view_panel()
        page = settings_page()
        c = status_counts()
        seg = {"label": f"{c['blocked']} blocked", "icon": ICON["blocked"], "style": "warn"} if c.get("blocked") else {"label": ""}
    for index, name, value in ((0, "sidebar", content), (2, "sidebar.view", view), (1, "sidebar.status", seg), (3, SETTINGS_PAGE, page)):
        if value != last[index]:
            ext.publish(name, value)
            last[index] = value


def refresh_worker():
    failed = None
    while True:
        refresh_pending.wait()
        time.sleep(PUBLISH_INTERVAL)
        # Clear before building: an event during publication schedules another fresh snapshot.
        refresh_pending.clear()
        try:
            refresh()
            failed = None
        except Exception as error:
            if str(error) != failed:
                ext.log("sidebar publication failed:", error)
                failed = str(error)


@ext.on_event
def on_event(t, d):
    if t in ("resource", "resource.removed", "provider", "missed") or (t == "view.content" and d.get("view") in ("workspaces", "agents.terminal_status")) \
            or (t == "entries" and any(c["key"] in (PREFS, "workspace/active") or c["key"].startswith(LAYOUT_KEYS)
                                       for c in d["changes"])):
        refresh_pending.set()


def has_sidebar(root):
    return root.get("split") == "h" and bool(root.get("children")) and root["children"][0].get("view") == "sidebar"


def without_sidebar(root):
    """The layout with its leading sidebar removed (a two-way split collapses to the other child)."""
    rest = root["children"][1:]
    if len(rest) == 1:
        return rest[0]
    out = dict(root, children=rest)
    if isinstance(root.get("sizes"), list) and len(root["sizes"]) == len(root["children"]):
        out["sizes"] = root["sizes"][1:]
    return out


@ext.command("sidebar.toggle", "Show or hide the sidebar (in every layout, so switching keeps it)")
def toggle(args):
    def attempt():
        # Recomputed from the reloaded cache on each try: a compare-and-set conflict with a concurrent
        # layout write is normal and must not surface to the user as an error.
        lay = ext.context_layout(args)  # what the invoking window shows decides show or hide
        if lay is None:
            raise VqroError("no active layout yet", "invalid")
        want = not has_sidebar(lay[2])
        ops = []
        with ext.cache_lock:
            layouts = [(k, e["version"], e["value"]) for k, e in ext.entries.items() if k.startswith("layout/")]
        for key, ver, root in layouts:
            if not isinstance(root, dict) or has_sidebar(root) == want:
                continue
            new = {"split": "h", "children": [{"view": "sidebar"}, root], "sizes": [1, 3]} if want else without_sidebar(root)
            ops.append({"op": "set", "key": key, "value": new, "expect": ver})
        return ext.apply(ops, "sidebar: toggle") if ops else {}
    return ext.retrying(attempt, attempts=8)


def select_ops(layout, path=None, tab=None):
    """Make `layout` the shared default (the last one switched to; new windows open there) and, given a
    tabs path and index, select that tab. Only changed entries. The invoking window itself is told to
    show `layout` by the reply (`switch`)."""
    key = f"layout/{layout}"
    root = copy.deepcopy(ext.value(key))  # the cache's own value is never mutated
    if root is None:
        raise VqroError("that layout is gone", "conflict")
    ops = []
    if active_layout_name() != layout:
        ops.append({"op": "set", "key": "workspace/active", "value": {"layout": layout}, "expect": ext.version("workspace/active")})
    if tab is not None:
        found = first_tabs_path(root)
        if not found or found[0] != list(path or []) or not 0 <= tab < len(found[1]["tabs"]):
            raise VqroError("that tab moved", "conflict")
        if found[1].get("active", 0) != tab:
            found[1]["active"] = tab
            ops.append({"op": "set", "key": key, "value": root, "expect": ext.version(key)})
    return ops


def switch(note, *target):
    def attempt():
        ops = select_ops(*target)
        return ext.apply(ops, note) if ops else {}
    result = ext.retrying(attempt, attempts=8)
    return dict(result or {}, client={"layout": target[0]})


@ext.command("sidebar.layout", "Switch to a layout (args: layout)")
def layout_cmd(args):
    return switch("sidebar: switch layout", args.get("layout"))


@ext.command("sidebar.tab", "Switch to a layout's tab and focus it (args: layout, path, tab)")
def tab_cmd(args):
    result = switch("sidebar: switch tab", args.get("layout"), args.get("path"), args.get("tab"))
    # Like clicking a resource row: the chosen tab gets the focus, not the sidebar.
    found = first_tabs_path(ext.value(f"layout/{args.get('layout')}") or {})
    tabs = found[1].get("tabs", []) if found else []
    tab = args.get("tab")
    views = views_in(tabs[tab]["node"]) if isinstance(tab, int) and 0 <= tab < len(tabs) else []
    return dict(result, client=dict(result["client"], focus={"view": views[0]})) if views else result


@ext.command("sidebar.view", "Choose what the sidebar shows (opens a popup)")
def view_cmd(args):
    return {"client": {"popup": {"view": "sidebar.view", "width": 52}}}


@ext.command("sidebar.page", "Choose a sidebar page (args: delta, or text page number)")
def page_cmd(args):
    with ext.cache_lock:
        try:
            selected = int(args["text"])-1 if "text" in args else page_state[0]+int(args.get("delta", 0))
        except (ValueError, TypeError):
            raise VqroError("enter a sidebar page number", "invalid")
        page_state[0] = max(0, min(selected, page_state[1]-1))
    refresh_now()


@ext.command("sidebar.show", "Show a resource in a tab (args: resource, or picked)")
def show_cmd(args):
    return ext.invoke("tabs.show", {"resource": args.get("resource") or args.get("picked")})


@ext.command("sidebar.close_ended", "Close ended tabs and hide ended sidebar items (nothing is killed)")
def close_ended(args):
    ext.invoke("tabs.close_ended", {})
    set_ended_visibility(True)


@ext.command("sidebar.show_ended", "Show ended sidebar items")
def show_ended(args):
    set_ended_visibility(False)


def set_ended_visibility(hidden):
    def attempt():
        value = dict(ext.value(PREFS) or {}, hide_ended=hidden)
        ext.apply([{"op": "set", "key": PREFS, "value": value, "expect": ext.version(PREFS)}], "sidebar: ended visibility")
    ext.retrying(attempt)


@ext.command("sidebar.pages", "Split a very large sidebar into pages, or not (the default: one scrolling list) (args: value)")
def pages_cmd(args):
    value = args.get("value")
    if not isinstance(value, bool):
        raise VqroError("value must be true or false", "invalid")
    def attempt():
        prefs = dict(ext.value(PREFS) or {}, pages=value)
        ext.apply([{"op": "set", "key": PREFS, "value": prefs, "expect": ext.version(PREFS)}], "sidebar: pages")
    ext.retrying(attempt)
    with ext.cache_lock:
        page_state[0] = 0
    refresh_now()


@ext.command("sidebar.kill", "Kill a terminal's program (args: resource)")
def kill(args):
    return ext.vqro.call("resource.kill", {"resource": args["resource"]})


@ext.command("sidebar.worktrees", "Worktree listing options (args: key = limit | changed_only | hide_detached, value)")
def worktrees_cmd(args):
    key, value = args.get("key"), args.get("value")
    if key == "limit" and type(value) is int and value >= 0:
        pass
    elif key in ("changed_only", "hide_detached") and type(value) is bool:
        pass
    else:
        raise VqroError("key: limit (integer >= 0) or changed_only/hide_detached (true/false)", "invalid")
    def attempt():
        prefs = ext.value(PREFS) or {}
        value_ = dict(prefs, worktrees=dict(prefs.get("worktrees") or {}, **{key: value}))
        ext.apply([{"op": "set", "key": PREFS, "value": value_, "expect": ext.version(PREFS)}], "sidebar: worktrees")
    ext.retrying(attempt)


@ext.command("sidebar.worktrees_more", "List every worktree of one workspace in this session (args: path)")
def worktrees_more(args):
    with ext.cache_lock:
        expanded_worktrees.add(args.get("path"))
    refresh_now()


@ext.command("sidebar.expand", "Show a collapsed workspace's contents in the sidebar (args: id)")
def expand_cmd(args):
    with ext.cache_lock:
        expanded_places.add(args.get("id"))
        collapsed_places.discard(args.get("id"))
    refresh_now()


@ext.command("sidebar.ended", "Show or hide the ended terminals folded in an \"N ended\" row (args: id)")
def ended_cmd(args):
    with ext.cache_lock:
        expanded_ended.symmetric_difference_update({args.get("id")})
    refresh_now()


@ext.command("sidebar.place", "A place's ▸ was opened or closed in a client (args: id, open)")
def toggle_cmd(args):
    with ext.cache_lock:
        if args.get("open") is True:
            expanded_places.add(args.get("id"))
            collapsed_places.discard(args.get("id"))
        else:
            expanded_places.discard(args.get("id"))
            collapsed_places.add(args.get("id"))
    refresh_now()


@ext.command("sidebar.collapse", "Show a workspace as one row in a large sidebar (args: id)")
def collapse_cmd(args):
    with ext.cache_lock:
        expanded_places.discard(args.get("id"))
        collapsed_places.add(args.get("id"))
    refresh_now()


SEARCH_LIMIT = 64


def search_rows(query):
    """Every row of the whole tree (all levels shown, nothing collapsed or paged) whose text matches."""
    query = query.strip().lower()
    with ext.cache_lock:
        tree = build(sh=dict(SHOW_DEFAULT), compact=False)
    found, more = [], 0
    def walk(rows, trail):
        nonlocal more
        for row in rows:
            if row.get("type") in ("header", "text"):
                continue
            text = str(row.get("text", ""))
            if query in text.lower() and row.get("command"):
                if len(found) < SEARCH_LIMIT:
                    found.append({k: row[k] for k in ("text", "icon", "icon_style", "badges", "command", "args", "input") if k in row}
                                 | ({"description": " › ".join(trail)} if trail else {}))
                else:
                    more += 1
            walk(row.get("children", []), trail + [text])
    walk(tree, [])
    return found, more


@ext.command("sidebar.search", "Search every sidebar row, collapsed and off-page ones too (args: text)")
def search_cmd(args):
    query = str(args.get("text") or "")
    if not query.strip():
        raise VqroError("type something to search for", "invalid")
    found, more = search_rows(query)
    items = found or [{"type": "text", "text": "No matches."}]
    if more:
        items.append({"type": "text", "style": "dim", "text": f"{more} more matches; search for something longer"})
    ext.publish("sidebar.search", {"title": f"Search: {query.strip()}", "requires": ["badges", "icon"], "items": items})
    return {"client": {"popup": {"view": "sidebar.search", "width": 60}}}


@ext.command("sidebar.preset", "Choose what the sidebar shows (args: preset, or show: {...})")
def preset(args):
    if isinstance(args.get("show"), dict):
        new = dict(show(), **{k: bool(v) for k, v in args["show"].items() if k in SHOW_DEFAULT})
    elif any(args.get(k) in PRESETS for k in ("preset", "value", "text")):
        new = PRESETS[next(args[k] for k in ("preset", "value", "text") if args.get(k) in PRESETS)]
    else:
        raise VqroError(f"presets: {', '.join(PRESETS)}", "invalid")
    ext.apply([{"op": "set", "key": PREFS, "value": dict(ext.value(PREFS) or {}, show=new), "expect": ext.version(PREFS)}], "sidebar: show")


if "binding/sidebar-toggle" in ext.entries:
    ext.apply([{"op": "delete", "key": "binding/sidebar-toggle"}], "sidebar: migrate binding")
ext.contribute({
    "view/sidebar": {"kind": "panel", "title": "sidebar", "items": [{"type": "text", "text": "no content: the sidebar extension is starting or stopped"}]},
    "view/sidebar.view": {"kind": "panel", "title": "Sidebar view", "items": [{"type": "text", "text": "no content: the sidebar extension is stopped"}]},
    "view/sidebar.search": {"kind": "panel", "title": "Search the sidebar", "items": [{"type": "text", "text": "no content: the sidebar extension is stopped"}]},
    "binding/sidebar.toggle": {"key": "s", "command": "sidebar.toggle", "label": "toggle sidebar"},
    "menu/sidebar.toggle": {"where": "status", "label": "sidebar", "icon": ICON["sidebar"], "command": "sidebar.toggle", "order": 10},
    "menu/sidebar.blocked": {"where": "status", "label": "", "view": "sidebar.status", "command": "sidebar.toggle", "order": 5},
})
refresh()
threading.Thread(target=refresh_worker, daemon=True, name="sidebar-publish").start()
ext.run()
