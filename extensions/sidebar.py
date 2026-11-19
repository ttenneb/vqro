#!/usr/bin/env python3
"""vqro extension: the sidebar, one tree of workspaces → worktrees → terminals.

Data comes from public sources only: resources (with their current folder) from the core, and the
`workspaces` extension's live data. A terminal sits under the deepest workspace or worktree folder it is
working in; terminals outside all of them are "other terminals". Status badges come from each
terminal's OSC 7501 report and roll up to its parents.

What to show is a preference in `doc/settings/sidebar` (`show`: workspaces, worktrees, terminals,
stray). The sidebar's pinned toolbar is one line so it fits a narrow sidebar: a `Show: <preset> ▾` chip
naming what is shown, and `/` to filter. The chip opens the `sidebar.view` popup: show toggles,
presets, and rarely used actions (refresh git state, add workspace). The popup is live content too,
so its checks update in place while it is open.
Rows are live content (`view.publish`): not saved, not in history. Large trees use shared, bounded
64-row pages, keeping folder ancestors and each agent/bridge group together. The page chips browse all
rows; filtering is page-local and the view popup also offers the complete resource chooser.
"""
import os
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from vqro_ext import Extension, VqroError  # noqa: E402
from vqro_views import ended, title, unknown_since

# `provider` keeps the registry cache current: an unavailable agent shows when its provider disconnected.
ext = Extension("sidebar", event_types=["entries", "resource", "resource.removed", "provider", "view.content"],
                view_content={"names": ["workspaces"]})
PREFS = "doc/settings/sidebar"
SHOW_DEFAULT = {"workspaces": True, "worktrees": True, "terminals": True, "stray": True}
PRESETS = {
    "everything": {"workspaces": True, "worktrees": True, "terminals": True, "stray": True},
    "workspaces only": {"workspaces": True, "worktrees": False, "terminals": False, "stray": False},
    "workspaces and worktrees": {"workspaces": True, "worktrees": True, "terminals": False, "stray": False},
    "worktrees and stray terminals": {"workspaces": True, "worktrees": True, "terminals": False, "stray": True},
    "terminals only": {"workspaces": False, "worktrees": False, "terminals": True, "stray": True},
}
# Short names for the toolbar chip, so the current choice fits a ~24-column sidebar on one line.
SHORT = {"everything": "all", "workspaces only": "workspaces", "workspaces and worktrees": "trees",
         "worktrees and stray terminals": "trees+other", "terminals only": "terminals"}
SHOW_LABELS = [("workspaces", "Workspaces", "folders you added"), ("worktrees", "Worktrees", "git worktrees"),
               ("terminals", "Terminals", "in workspaces"), ("stray", "Other terminals", "elsewhere")]
STATUS = {"working": "accent", "blocked": "warn", "error": "error", "done": "ok"}
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
}
last = [None, None, None]  # what was last published: sidebar, status segment, view popup
refresh_pending = threading.Event()
PUBLISH_INTERVAL = 0.5  # Coalesce status bursts into at most two sidebar updates/second.
PAGE_SIZE = 64  # Bound each full live publication, including off-screen tree detail.
page_state = [0, 1]  # Shared sidebar page and current page count; derived, never history.


def show():
    return dict(SHOW_DEFAULT, **((ext.value(PREFS) or {}).get("show") or {}))


def current_preset():
    return next((name for name, p in PRESETS.items() if p == show()), None)


def status_of(r):
    st = r.get("status")
    return st if isinstance(st, dict) else {}


def folder_of(r):
    """Short name of a terminal's current folder ("~" for home), or None."""
    cwd = r.get("cwd")
    if not cwd:
        return None
    return "~" if os.path.realpath(cwd) == os.path.realpath(os.path.expanduser("~")) else os.path.basename(cwd.rstrip("/")) or cwd


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
        badges.append({"text": f"{int(st['progress'])}%", "style": "accent"})
    live = r.get("state") == "live"
    # A terminal whose program reports status (OSC 7501) is an agent; its icon takes the status colour,
    # and a set with a glyph for the state shows that instead of the agent's (shape, not only colour).
    agent = r.get("kind") == "agent" or bool(st.get("state") or st.get("app"))
    icon_style = STATUS.get(st.get("state")) if live and st.get("state") else ("muted" if live else "dim")
    icon = ICON["agent" if agent else "terminal"]
    if agent and st.get("state") in STATUS:
        icon = dict(icon, name=[st["state"], "agent"])
    return {
        "id": f"term:{rid}", "text": label, "style": None if live else "dim",
        "icon": icon, "icon_style": icon_style,
        "badges": badges,
        "command": "sidebar.show", "args": {"resource": rid},
        "menu": [
            {"label": "Show", "command": "sidebar.show", "args": {"resource": rid}},
            *([{"label": "Kill program", "command": "sidebar.kill", "args": {"resource": rid}}]
              if r.get("kind", "terminal") == "terminal" and live else []),
            # Agents declaring data.agent config/commands (agent v0 6.3); the agents extension owns the popup.
            *([{"label": "Agent settings…", "command": "agents.config", "args": {"resource": rid}}]
              if r.get("kind") == "agent" and live and isinstance((r.get("data") or {}).get("agent"), dict)
              and (r["data"]["agent"].get("config") or r["data"]["agent"].get("commands")) else []),
            # The agents extension explains agents it can't stop (no bridge terminal) and asks first.
            *([{"label": "Stop agent…", "command": "agents.stop", "args": {"resource": rid}}]
              if r.get("kind") == "agent" and not ended(r) else []),
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
    counts = {}

    def walk(rs):
        for x in rs:
            if x.get("id", "").startswith("term:"):
                for b in x.get("badges", []):
                    if b["text"] in ("blocked", "working"):
                        counts[b["text"]] = counts.get(b["text"], 0) + 1
            walk(x.get("children", []))
    walk(rows)
    return counts


def rollup(rows):
    """Badges for a parent: how many terminals beneath it are blocked or working."""
    c = counts_of(rows)
    out = []
    if c.get("blocked"):
        out.append({"text": f"{c['blocked']} blocked", "style": "warn"})
    if c.get("working"):
        out.append({"text": f"{c['working']} working", "style": "accent"})
    return out


def git_badges(w):
    b = []
    if w.get("branch"):
        b.append({"text": w["branch"]})
    if w.get("dirty"):
        b.append({"text": f"±{w['dirty']}", "style": "warn"})
    return b


def build():
    sh = show()
    data = (ext.live.get("workspaces") or {}).get("data")
    tree = data is not None and sh["workspaces"]
    # Every folder a terminal can belong to; the deepest match wins.
    homes = []
    for w in data or []:
        homes.append((os.path.realpath(w["path"]), ("ws", w["path"])))
        for t in w.get("worktrees", []):
            homes.append((os.path.realpath(t["path"]), ("wt", t["path"])))
    homes.sort(key=lambda h: -len(h[0]))
    # `placed`: terminals under a tree folder. `stray`: the rest in the tree, or the flat list without one,
    # where Terminals still means "in workspaces" and Other "elsewhere" (all count when there's no data).
    placed, stray = {}, []
    agents = {r.get("host"): r for r in ext.resources.values() if r.get("kind") == "agent"}
    hide_ended = (ext.value(PREFS) or {}).get("hide_ended", False)
    for rid, r in sorted(ext.resources.items(), key=lambda kv: int(kv[0][1:]) if kv[0][1:].isdigit() else 0):
        if rid in agents or (hide_ended and ended(r)):
            continue
        row = term_row(rid, r, where=True)
        host = ext.resources.get(r.get("host")) if r.get("kind") == "agent" else None
        if host and host.get("kind") == "terminal":
            row["children"] = [term_row(host["id"], host)]
            row["expanded"] = False
        cwd = os.path.realpath(r.get("cwd") or (host or {}).get("cwd")) if r.get("cwd") or (host or {}).get("cwd") else None
        home = next((k for p, k in homes if cwd and (cwd == p or cwd.startswith(p + os.sep))), None)
        if tree and home:
            placed.setdefault(home, []).append(row)
        elif tree or data is None or sh["terminals" if home else "stray"]:
            stray.append(row)

    items = []
    if tree:
        items.append({"type": "header", "text": "Workspaces"})
        for w in data:
            kids = list(placed.get(("ws", w["path"]), [])) if sh["terminals"] else []
            if sh["worktrees"]:
                for t in w.get("worktrees", []):
                    tt = placed.get(("wt", t["path"]), []) if sh["terminals"] else []
                    kids.append({
                        "id": f"wt:{t['path']}", "text": os.path.basename(t["path"].rstrip("/")), "expanded": True,
                        "icon": ICON["worktree"], "icon_style": "warn" if t.get("dirty") else "accent",
                        "badges": git_badges(t) + rollup(tt), "children": tt,
                        "command": "workspaces.open", "args": {"path": t["path"]},
                        "menu": [{"label": "Open shell here", "command": "workspaces.open", "args": {"path": t["path"]}}],
                    })
            menu = [{"label": "Open shell here", "command": "workspaces.open", "args": {"path": w["path"]}}]
            if w.get("git"):
                menu.append({"label": "New worktree…", "command": "workspaces.worktree", "args": {"repo": w["path"]}, "input": "Branch name"})
            menu += [{"label": "Refresh git state", "command": "workspaces.refresh"},
                     {"label": "Remove from workspaces", "command": "workspaces.remove", "args": {"path": w["path"]}}]
            items.append({
                "id": f"ws:{w['path']}", "text": w["name"], "expanded": True,
                "icon": ICON["repo" if w.get("git") else "folder"],
                "icon_style": "error" if w.get("missing") else ("warn" if w.get("dirty") else "accent") if w.get("git") else "muted",
                "style": "error" if w.get("missing") else "bold",
                "badges": ([{"text": "missing", "style": "error"}] if w.get("missing") else git_badges(w)) + rollup(kids),
                "children": kids, "command": "workspaces.open", "args": {"path": w["path"]}, "menu": menu,
            })
        items.append({"id": "add", "text": "add workspace…", "style": "dim", "icon": ICON["add"], "icon_style": "dim",
                      "command": "workspaces.add", "input": "Folder path"})
        if sh["stray"] and stray:
            items.append({"type": "header", "text": "Other terminals"})
            items += stray
    elif sh["terminals"] or sh["stray"]:
        items.append({"type": "header", "text": "Terminals"})
        items += stray or [{"type": "text", "text": "no terminals"}]
    if not items:
        items = [{"type": "text", "text": "nothing to show"}]
    return items


def paginate(items, page, size=PAGE_SIZE):
    """Slice logical tree rows, retaining ancestor context and grouped bridge children.

    A resource and its one bridge child are a unit. Folder ancestors can repeat
    across pages; no resource is omitted, and per-page filtering is explicit.
    """
    def children(row):
        return [] if row.get("id", "").startswith("term:") else row.get("children", [])
    def count(rows):
        return sum((0 if row.get("type") == "header" else 1) + count(children(row)) for row in rows)
    total = count(items)
    pages = max(1, (total + size - 1) // size)
    page = max(0, min(page, pages-1))
    start, end = page*size, (page+1)*size
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


def toolbar():
    """One line: what is shown (opens the view popup) and the filter."""
    short = SHORT.get(current_preset(), "custom")
    rows = [{"type": "chips", "chips": [
        {"text": f"Show: {short} ▾", "command": "sidebar.view"},
        {"text": "", "icon": ICON["filter"], "command": "client.panel", "args": {"action": "filter"}},
    ]}]
    if page_state[1] > 1:
        rows.append({"type": "chips", "chips": [
            {"text": "‹", "command": "sidebar.page", "args": {"delta": -1}},
            {"text": f"{page_state[0]+1}/{page_state[1]}", "command": "sidebar.page", "input": "Sidebar page number"},
            {"text": "›", "command": "sidebar.page", "args": {"delta": 1}},
        ]})
    return rows


def view_panel():
    """The `sidebar.view` popup: show toggles, presets, then actions too rare for the toolbar."""
    sh = show()
    have_ws = ext.live.get("workspaces") is not None
    rows = [{"type": "header", "text": "Show"}]
    for k, label, what in SHOW_LABELS:
        row = {"type": "toggle", "text": label, "checked": bool(sh[k]), "detail": what,
               "command": "sidebar.preset", "args": {"show": {k: not sh[k]}}}
        # Say why a toggle can't matter right now instead of hiding it.
        if not have_ws and k in ("workspaces", "worktrees"):
            row.update(disabled=True, detail="workspaces extension off")
        elif k == "worktrees" and not sh["workspaces"]:
            row.update(disabled=True, detail="needs Workspaces")
        rows.append(row)
    rows += [{"type": "separator"}, {"type": "header", "text": "Presets"}]
    current = current_preset()
    rows += [{"text": ("● " if name == current else "○ ") + name, "style": "accent" if name == current else None,
              "command": "sidebar.preset", "args": {"preset": name}} for name in PRESETS]
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


def refresh(*_):
    # The worker reads a coherent cache, but never holds its lock across RPC/publication.
    with ext.cache_lock:
        all_items = build()
        items, page_state[0], page_state[1], total = paginate(all_items, page_state[0])
        content = {"requires": ["tree", "badges", "menu", "toolbar", "chips", "icon", "icon_names"], "toolbar": toolbar(), "items": items,
                   "page": {"number": page_state[0]+1, "count": page_state[1], "rows": total}}
        view = view_panel()
        c = counts_of(all_items)
        seg = {"label": f"{c['blocked']} blocked", "icon": ICON["blocked"], "style": "warn"} if c.get("blocked") else {"label": ""}
    for index, name, value in ((0, "sidebar", content), (2, "sidebar.view", view), (1, "sidebar.status", seg)):
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
    if t in ("resource", "resource.removed", "provider", "missed") or (t == "view.content" and d.get("view") == "workspaces") \
            or (t == "entries" and any(c["key"] == PREFS for c in d["changes"])):
        refresh_pending.set()


@ext.command("sidebar.toggle", "Show or hide the sidebar")
def toggle(args):
    def attempt():
        # Recomputed from the reloaded cache on each try: a compare-and-set conflict with a concurrent
        # layout write is normal and must not surface to the user as an error.
        lay = ext.active_layout()
        if lay is None:
            raise VqroError("no active layout yet", "invalid")
        key, ver, root = lay
        if root.get("split") == "h" and root["children"][0].get("view") == "sidebar":
            new = root["children"][1]
        else:
            new = {"split": "h", "children": [{"view": "sidebar"}, root], "sizes": [1, 3]}
        return ext.apply([{"op": "set", "key": key, "value": new, "expect": ver}], "sidebar: toggle")
    return ext.retrying(attempt, attempts=8)


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
    refresh_pending.set()


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


@ext.command("sidebar.kill", "Kill a terminal's program (args: resource)")
def kill(args):
    return ext.vqro.call("resource.kill", {"resource": args["resource"]})


@ext.command("sidebar.preset", "Choose what the sidebar shows (args: preset, or show: {...})")
def preset(args):
    if isinstance(args.get("show"), dict):
        new = dict(show(), **args["show"])
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
    "binding/sidebar.toggle": {"key": "s", "command": "sidebar.toggle", "label": "toggle sidebar"},
    "menu/sidebar.toggle": {"where": "status", "label": "sidebar", "icon": ICON["sidebar"], "command": "sidebar.toggle", "order": 10},
    "menu/sidebar.blocked": {"where": "status", "label": "", "view": "sidebar.status", "command": "sidebar.toggle", "order": 5},
})
refresh()
threading.Thread(target=refresh_worker, daemon=True, name="sidebar-publish").start()
ext.run()
