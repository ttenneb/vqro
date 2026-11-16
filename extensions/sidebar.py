#!/usr/bin/env python3
"""prim extension: the sidebar, one tree of workspaces → worktrees → terminals.

Data comes from public sources only: resources (with their current folder) from the core, and the
`workspaces` extension's live data. A terminal sits under the deepest workspace or worktree folder it is
working in; terminals outside all of them are "other terminals". Status badges come from each
terminal's OSC 7501 report and roll up to its parents.

What to show is a preference in `doc/settings/sidebar` (`show`: workspaces, worktrees, terminals,
stray). The sidebar's pinned toolbar is one line so it fits a narrow sidebar: a `Show: <preset> ▾` chip
naming what is shown, and `/` to filter. The chip opens the `sidebar.view` popup: show toggles,
presets, and rarely used actions (refresh git state, add workspace). The popup is live content too,
so its checks update in place while it is open.
Rows are live content (`view.publish`): not saved, not in history.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from prim_ext import Extension, PrimError  # noqa: E402

ext = Extension("sidebar")
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
last = [None, None, None]  # what was last published: sidebar, status segment, view popup


def show():
    return dict(SHOW_DEFAULT, **((ext.value(PREFS) or {}).get("show") or {}))


def current_preset():
    return next((name for name, p in PRESETS.items() if p == show()), None)


def status_of(r):
    st = r.get("status")
    return st if isinstance(st, dict) else {}


def term_row(rid, r):
    label = r.get("name") or (r.get("argv") or ["?"])[0].split("/")[-1]
    st = status_of(r)
    badges = []
    if st.get("state"):
        badges.append({"text": st["state"], "style": STATUS.get(st["state"])})
    if isinstance(st.get("progress"), (int, float)):
        badges.append({"text": f"{int(st['progress'])}%", "style": "accent"})
    live = r.get("state") == "live"
    return {
        "id": f"term:{rid}", "text": ("● " if live else "○ ") + label, "style": None if live else "dim",
        "badges": badges,
        "command": "sidebar.show", "args": {"resource": rid},
        "menu": [
            {"label": "Show", "command": "sidebar.show", "args": {"resource": rid}},
            {"label": "Kill program", "command": "sidebar.kill", "args": {"resource": rid}},
        ],
    }


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
    for rid, r in sorted(ext.resources.items(), key=lambda kv: int(kv[0][1:]) if kv[0][1:].isdigit() else 0):
        cwd = os.path.realpath(r["cwd"]) if r.get("cwd") else None
        home = next((k for p, k in homes if cwd and (cwd == p or cwd.startswith(p + os.sep))), None)
        if tree and home:
            placed.setdefault(home, []).append(term_row(rid, r))
        elif tree or data is None or sh["terminals" if home else "stray"]:
            stray.append(term_row(rid, r))

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
                "style": "error" if w.get("missing") else "bold",
                "badges": ([{"text": "missing", "style": "error"}] if w.get("missing") else git_badges(w)) + rollup(kids),
                "children": kids, "command": "workspaces.open", "args": {"path": w["path"]}, "menu": menu,
            })
        items.append({"id": "add", "text": "+ add workspace…", "style": "dim", "command": "workspaces.add", "input": "Folder path"})
        if sh["stray"] and stray:
            items.append({"type": "header", "text": "Other terminals"})
            items += stray
    elif sh["terminals"] or sh["stray"]:
        items.append({"type": "header", "text": "Terminals"})
        items += stray or [{"type": "text", "text": "no terminals"}]
    if not items:
        items = [{"type": "text", "text": "nothing to show"}]
    return items


def toolbar():
    """One line: what is shown (opens the view popup) and the filter."""
    short = SHORT.get(current_preset(), "custom")
    return [{"type": "chips", "chips": [
        {"text": f"Show: {short} ▾", "command": "sidebar.view"},
        {"text": "/", "command": "client.panel", "args": {"action": "filter"}},
    ]}]


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
    if have_ws:
        rows += [{"type": "separator"},
                 {"text": "↻ Refresh git state", "command": "workspaces.refresh"},
                 {"text": "+ Add workspace…", "command": "workspaces.add", "input": "Folder path"}]
    return {"title": "Sidebar view", "items": rows}


def refresh(*_):
    items = build()
    content = {"requires": ["tree", "badges", "menu", "toolbar", "chips"], "toolbar": toolbar(), "items": items}
    if content != last[0]:
        last[0] = content
        ext.publish("sidebar", content)
    view = view_panel()
    if view != last[2]:
        last[2] = view
        ext.publish("sidebar.view", view)
    c = counts_of(items)
    seg = {"label": f"⚠ {c['blocked']} blocked", "style": "warn"} if c.get("blocked") else {"label": ""}
    if seg != last[1]:
        last[1] = seg
        ext.publish("sidebar.status", seg)


@ext.on_event
def on_event(t, d):
    if t in ("resource", "resource.removed", "missed") or (t == "view.content" and d.get("view") == "workspaces") \
            or (t == "entries" and any(c["key"] == PREFS for c in d["changes"])):
        refresh()


@ext.command("sidebar.toggle", "Show or hide the sidebar")
def toggle(args):
    lay = ext.active_layout()
    if lay is None:
        raise PrimError("no active layout yet", "invalid")
    key, ver, root = lay
    if root.get("split") == "h" and root["children"][0].get("view") == "sidebar":
        new = root["children"][1]
    else:
        new = {"split": "h", "children": [{"view": "sidebar"}, root], "sizes": [1, 3]}
    ext.apply([{"op": "set", "key": key, "value": new, "expect": ver}], "sidebar: toggle")


@ext.command("sidebar.view", "Choose what the sidebar shows (opens a popup)")
def view_cmd(args):
    return {"client": {"popup": {"view": "sidebar.view", "width": 52}}}


@ext.command("sidebar.show", "Show a terminal in a tab (args: resource)")
def show_cmd(args):
    return ext.invoke("tabs.show", {"resource": args["resource"]})


@ext.command("sidebar.kill", "Kill a terminal's program (args: resource)")
def kill(args):
    return ext.prim.call("resource.kill", {"resource": args["resource"]})


@ext.command("sidebar.preset", "Choose what the sidebar shows (args: preset, or show: {...})")
def preset(args):
    if isinstance(args.get("show"), dict):
        new = dict(show(), **args["show"])
    elif any(args.get(k) in PRESETS for k in ("preset", "value", "text")):
        new = PRESETS[next(args[k] for k in ("preset", "value", "text") if args.get(k) in PRESETS)]
    else:
        raise PrimError(f"presets: {', '.join(PRESETS)}", "invalid")
    ext.apply([{"op": "set", "key": PREFS, "value": {"show": new}, "expect": ext.version(PREFS)}], "sidebar: show")


if "binding/sidebar-toggle" in ext.entries:
    ext.apply([{"op": "delete", "key": "binding/sidebar-toggle"}], "sidebar: migrate binding")
ext.contribute({
    "view/sidebar": {"kind": "panel", "title": "sidebar", "items": [{"type": "text", "text": "no content: the sidebar extension is starting or stopped"}]},
    "view/sidebar.view": {"kind": "panel", "title": "Sidebar view", "items": [{"type": "text", "text": "no content: the sidebar extension is stopped"}]},
    "binding/sidebar.toggle": {"key": "s", "command": "sidebar.toggle", "label": "toggle sidebar"},
    "menu/sidebar.toggle": {"where": "status", "label": "☰ sidebar", "command": "sidebar.toggle", "order": 10},
    "menu/sidebar.blocked": {"where": "status", "label": "", "view": "sidebar.status", "command": "sidebar.toggle", "order": 5},
})
refresh()
ext.run()
