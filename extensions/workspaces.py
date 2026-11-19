#!/usr/bin/env python3
"""vqro extension: workspaces. Folders you add by hand: git repos (with their worktrees) or plain folders.

Owns the list (`doc/workspaces/list`) and the git facts. It publishes them as live data under the name
`workspaces` (`{"data": [...]}`) for any extension to draw; the sidebar turns them into a tree. Git is
read on changes and on a slow timer, never continuously.

Each workspace (and each worktree) has its own tabs: its own layout, `layout/ws-<id>`, where `<id>` comes
from the folder's real path, so the same folder always finds the same layout. Opening a workspace makes its
layout the active one (`workspace/active`), creating it with one shell in the folder the first time. The
layout's settings (`doc/layout/<name>` = {cwd, title}) say which folder it starts in; tabs, panes and the
Pi launcher read that, never this extension. Terminals not in any workspace stay in the `main` layout.
Removing a workspace removes its layout, never its terminals: they keep running and stay reachable from
the sidebar and the resource chooser.
"""
import copy
import hashlib
import os
import subprocess
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from vqro_ext import Extension, VqroError, views_in  # noqa: E402
from vqro_views import title  # noqa: E402

ext = Extension("workspaces", event_types=["entries"])  # no resource events: not used here
LIST = "doc/workspaces/list"
MAIN = "main"
REFRESH_SECS = 20
lock = threading.Lock()
last_status = [None]
last_data = [[]]  # what scan() last published (this extension doesn't receive its own live data)


def git(path, *args):
    r = subprocess.run(["git", "-C", path, *args], capture_output=True, text=True, timeout=10)
    return r.stdout if r.returncode == 0 else None


def worktrees(repo):
    out = git(repo, "worktree", "list", "--porcelain")
    if out is None:
        return []
    wts, cur = [], {}
    for line in out.splitlines() + [""]:
        if not line:
            if cur:
                wts.append(cur)
            cur = {}
        elif line.startswith("worktree "):
            cur["path"] = line[9:]
        elif line.startswith("branch "):
            cur["branch"] = line[7:].replace("refs/heads/", "")
        elif line == "detached":
            cur["branch"] = "(detached)"
        elif line == "bare":
            cur["bare"] = True
    for w in wts:
        if not w.get("bare"):
            status = git(w["path"], "status", "--porcelain")
            w["dirty"] = len(status.splitlines()) if status is not None else None
    return wts


def entries():
    return list((ext.value(LIST) or {}).get("items", []))


def norm(path):
    return os.path.abspath(os.path.expanduser(path))


def layout_name(path):
    """The layout of the workspace at `path`: stable for the folder, the same for every caller."""
    return "ws-" + hashlib.sha256(os.path.realpath(norm(path)).encode()).hexdigest()[:12]


def folder_title(path):
    return os.path.basename(norm(path).rstrip("/")) or norm(path)


def scan():
    with lock:
        data = []
        for e in entries():
            path = os.path.expanduser(e["path"])
            item = {"path": path, "name": e.get("name") or os.path.basename(path.rstrip("/")) or path}
            if not os.path.isdir(path):
                item["missing"] = True
            elif os.path.realpath((git(path, "rev-parse", "--show-toplevel") or "").strip() or "/nonexistent") == os.path.realpath(path):
                # Only a repo's own root counts as a repo; a folder inside one stays a plain folder.
                item["git"] = True
                top = path
                wts = worktrees(top)
                main = next((w for w in wts if os.path.realpath(w["path"]) == os.path.realpath(top)), None)
                item["path"] = top
                item["branch"] = (main or {}).get("branch")
                item["dirty"] = (main or {}).get("dirty")
                item["worktrees"] = [dict(w, layout=layout_name(w["path"])) for w in wts if w is not main and not w.get("bare")]
            # The layout this workspace's tabs live in (it may not exist until first opened).
            item["layout"] = layout_name(item["path"])
            data.append(item)
        last_data[0] = data
        ext.publish("workspaces", {"data": data, "at": time.time()})


def status_segment():
    """The status bar names the current workspace; clicking it opens the switcher."""
    active = (ext.value("workspace/active") or {}).get("layout")
    settings = ext.layout_settings(active) if active else {}
    label = settings.get("title") or active or ""
    return {"label": label, "icon": {"name": "folder", "text": "▫"}, "style": "muted"} if label else {"label": ""}


def publish_status():
    seg = status_segment()
    if seg != last_status[0]:
        ext.publish("workspaces.current", seg)
        last_status[0] = seg


@ext.on_event
def on_event(t, d):
    if t == "missed" or t == "entries" and any(c["key"] == LIST for c in d["changes"]):
        threading.Thread(target=scan, daemon=True).start()
    if t == "missed" or t == "entries" and any(c["key"] == "workspace/active" or c["key"].startswith("doc/layout/")
                                                for c in d["changes"]):
        publish_status()


def save(items, note, extra=()):
    ext.apply([{"op": "set", "key": LIST, "value": {"items": items}, "expect": ext.version(LIST)}, *extra], note)


# -- layouts --------------------------------------------------------------------------------------

def select(name):
    """Ops that make `name` the active layout. The one place a workspace is chosen: a per-window active
    workspace would replace this shared write with a reply asking the invoking client to show `name`."""
    return [{"op": "set", "key": "workspace/active", "value": {"layout": name}, "expect": ext.version("workspace/active")}]


def has_tabs(node):
    return "tabs" in node or any(has_tabs(c) for c in node.get("children", []))


def is_panel(view):
    return (ext.value(f"view/{view}") or {}).get("kind") == "panel"


def frame(node, tabs):
    """The active layout's panels around its tabs (a sidebar, say), holding `tabs` where its tabs were.

    Only panels carry over: terminals and other views beside the tabs stay in their own layout. Without
    a tabs node to replace, the new layout is just `tabs`."""
    if node is None or "tabs" in node or not has_tabs(node):
        return tabs
    kids, sizes, placed = [], [], False
    for i, child in enumerate(node.get("children", [])):
        if not placed and has_tabs(child):
            kept, placed = frame(child, tabs), True
        elif views_in(child) and all(is_panel(v) for v in views_in(child)):
            kept = copy.deepcopy(child)
        else:
            continue
        kids.append(kept)
        if "sizes" in node and i < len(node["sizes"]):
            sizes.append(node["sizes"][i])
    if len(kids) == 1:
        return kids[0]
    out = {"split": node["split"], "children": kids}
    if len(sizes) == len(kids):
        out["sizes"] = sizes
    return out


def create(name, cwd, label, resource):
    """Ops for a new layout `name` with one tab showing `resource`, started in `cwd`, and selected."""
    view = f"term-{resource['id']}"
    active = ext.active_layout()
    tabs = {"tabs": [{"title": title(resource, {}), "node": {"view": view}}], "active": 0}
    ops = [] if ext.value(f"view/{view}") else [{"op": "set", "key": f"view/{view}", "value": {"kind": "terminal", "resource": resource["id"]}}]
    ops += [{"op": "set", "key": f"layout/{name}", "value": frame(active[2] if active else None, tabs), "expect": 0}]
    settings = {k: v for k, v in (("cwd", cwd), ("title", label)) if v}
    if settings and ext.value(f"doc/layout/{name}") != settings:
        ops.append({"op": "set", "key": f"doc/layout/{name}", "value": settings, "expect": ext.version(f"doc/layout/{name}")})
    return ops + select(name)


def add_tab(name, resource):
    """Ops that add a tab showing `resource` to the existing layout `name` (a concurrent creator won)."""
    view = f"term-{resource['id']}"
    root = copy.deepcopy(ext.value(f"layout/{name}"))
    tabs = root if "tabs" in root else next((n for n in walk(root) if "tabs" in n), None)
    ops = [] if ext.value(f"view/{view}") else [{"op": "set", "key": f"view/{view}", "value": {"kind": "terminal", "resource": resource["id"]}}]
    if tabs is None:
        root = {"tabs": [{"title": "main", "node": root}], "active": 0}
        tabs = root
    tabs["tabs"].append({"title": title(resource, {}), "node": {"view": view}})
    tabs["active"] = len(tabs["tabs"]) - 1
    return ops + [{"op": "set", "key": f"layout/{name}", "value": root, "expect": ext.version(f"layout/{name}")}] + select(name)


def walk(node):
    yield node
    for c in node.get("children", []):
        yield from walk(c)
    for t in node.get("tabs", []):
        yield from walk(t["node"])


def open_layout(name, cwd, label):
    """Make layout `name` active, creating it with one shell (in `cwd`) if missing. Returns (result, created).

    The shell is spawned at most once; only the layout write is retried (compare-and-set), recomputed
    from fresh state, never a second spawn."""
    if ext.value(f"layout/{name}") is not None:
        def switch():
            if ext.value(f"layout/{name}") is None:
                raise VqroError("that workspace's layout was removed meanwhile", "conflict")
            if (ext.value("workspace/active") or {}).get("layout") == name:
                return {}
            return ext.apply(select(name), f"workspaces: switch to {label}")
        return ext.retrying(switch, attempts=8), False
    resource = ext.vqro.call("resource.spawn", {"cwd": cwd} if cwd else {})["resource"]

    def attempt():
        if ext.value(f"layout/{name}") is None:
            return ext.apply(create(name, cwd, label, resource), f"workspaces: open {label}")
        return ext.apply(add_tab(name, resource), f"workspaces: open {label}")
    return ext.retrying(attempt, attempts=8), True


# -- commands -------------------------------------------------------------------------------------

@ext.command("workspaces.add", "Add a workspace folder (args: text = path)")
def add(args):
    raw = (args.get("text") or args.get("path") or "").strip()
    if not raw:
        raise VqroError("give a folder path", "invalid")
    path = norm(raw)
    if not os.path.isdir(path):
        raise VqroError(f"not a folder: {path}", "invalid")
    items = entries()
    if any(norm(i["path"]) == path for i in items):
        raise VqroError("already added", "conflict")
    items.append({"path": path})
    save(items, "workspaces: add")
    ext.notify(f"added {os.path.basename(path)}", "ok")


@ext.command("workspaces.remove", "Remove a workspace and its tabs (args: path; files and running programs untouched)")
def remove(args):
    path = norm(args.get("path") or "")

    # The workspace's own layout and its worktrees' layouts.
    item = next((w for w in last_data[0] if norm(w["path"]) == path), {})
    names = [layout_name(path)] + [t["layout"] for t in item.get("worktrees", [])]

    def attempt():
        items = [i for i in entries() if norm(i["path"]) != path]
        active = (ext.value("workspace/active") or {}).get("layout")
        doomed = [n for n in names if ext.value(f"layout/{n}") is not None]
        extra = []
        if active in doomed:
            if ext.value(f"layout/{MAIN}") is None:
                doomed.remove(active)  # nowhere to go: it stays, listed with the other layouts
            else:
                extra += select(MAIN)
        # Their terminal views go too unless another layout shows them; the programs keep running.
        kept = set()
        for key, entry in list(ext.entries.items()):
            if key.startswith("layout/") and key[len("layout/"):] not in doomed:
                kept.update(views_in(entry["value"]))
        gone = set()
        for n in doomed:
            gone.update(views_in(ext.value(f"layout/{n}")))
            extra.append({"op": "delete", "key": f"layout/{n}", "expect": ext.version(f"layout/{n}")})
            if ext.value(f"doc/layout/{n}") is not None:
                extra.append({"op": "delete", "key": f"doc/layout/{n}", "expect": ext.version(f"doc/layout/{n}")})
        extra += [{"op": "delete", "key": f"view/{v}", "expect": ext.version(f"view/{v}")}
                  for v in sorted(gone - kept) if ext.value(f"view/{v}") is not None and not is_panel(v)]
        save(items, "workspaces: remove", extra)
    ext.retrying(attempt)


@ext.command("workspaces.open", "Switch to a workspace's tabs, creating them with a shell there (args: path)")
def open_workspace(args):
    path = norm(args.get("path") or "")
    if not os.path.isdir(path):
        raise VqroError(f"not a folder: {path}", "invalid")
    result, _ = open_layout(layout_name(path), path, folder_title(path))
    return result


@ext.command("workspaces.shell", "Open a new shell tab in a workspace, switching to it (args: path)")
def shell(args):
    path = norm(args.get("path") or "")
    if not os.path.isdir(path):
        raise VqroError(f"not a folder: {path}", "invalid")
    result, created = open_layout(layout_name(path), path, folder_title(path))
    return result if created else ext.invoke("tabs.open", {"cwd": path})


@ext.command("workspaces.main", "Switch to the main tabs (terminals not in a workspace)")
def main_layout(args):
    result, _ = open_layout(MAIN, None, None)
    return result


def switcher():
    """The `workspaces.switch` popup: main, then every workspace and worktree; the current one marked."""
    active = (ext.value("workspace/active") or {}).get("layout")
    data = last_data[0]

    def row(text, command, name, args=None, depth=0):
        here = name == active
        out = {"id": f"switch:{name}", "text": ("  " * depth) + text, "command": command, "args": args or {},
               "style": "accent" if here else None, "badges": [{"text": "current", "style": "accent"}] if here else []}
        if ext.value(f"layout/{name}") is None:
            out["detail"] = "new"
        return out
    items = [row("main", "workspaces.main", MAIN)]
    for w in data:
        items.append(row(w["name"], "workspaces.open", w["layout"], {"path": w["path"]}))
        for t in w.get("worktrees", []):
            items.append(row(os.path.basename(t["path"].rstrip("/")), "workspaces.open", t["layout"], {"path": t["path"]}, 1))
    items.append({"type": "separator"})
    items.append({"text": "Add workspace…", "command": "workspaces.add", "input": "Folder path"})
    return {"title": "Switch workspace", "requires": ["badges"], "items": items}


@ext.command("workspaces.switch", "Choose a workspace to switch to (opens a popup)")
def switch_cmd(args):
    ext.publish("workspaces.switch", switcher())
    return {"client": {"popup": {"view": "workspaces.switch", "width": 52}}}


@ext.command("workspaces.worktree", "New git worktree (args: repo, text = branch)")
def new_worktree(args):
    repo, branch = args.get("repo"), (args.get("text") or "").strip()
    if not repo or not branch:
        raise VqroError("needs a repo and a branch name", "invalid")
    dest = os.path.join(os.path.dirname(repo.rstrip("/")), f"{os.path.basename(repo.rstrip('/'))}-{branch.replace('/', '-')}")
    exists = git(repo, "rev-parse", "--verify", "--quiet", f"refs/heads/{branch}") is not None
    cmd = ["git", "-C", repo, "worktree", "add", dest] + ([branch] if exists else ["-b", branch])
    r = subprocess.run(cmd, capture_output=True, text=True, timeout=60)
    if r.returncode != 0:
        ext.notify(f"worktree failed: {r.stderr.strip().splitlines()[-1] if r.stderr.strip() else r.returncode}", "error")
        raise VqroError(r.stderr.strip() or "git worktree add failed", "backend")
    scan()
    ext.notify(f"worktree {branch} at {dest}", "ok")


@ext.command("workspaces.refresh", "Re-read git state for all workspaces")
def refresh(args):
    scan()


def timer():
    while True:
        time.sleep(REFRESH_SECS)
        try:
            scan()
        except Exception as e:  # keep the timer alive
            ext.log("scan failed:", e)


ext.contribute({
    "view/workspaces.switch": {"kind": "panel", "title": "Switch workspace", "items": [{"type": "text", "text": "no content: the workspaces extension is stopped"}]},
    "binding/workspaces.switch": {"key": "w", "command": "workspaces.switch", "label": "switch workspace"},
    "menu/workspaces.switch": {"where": "tabbar", "label": "Switch workspace…", "command": "workspaces.switch", "order": 40},
    "menu/workspaces.current": {"where": "status", "label": "", "view": "workspaces.current", "command": "workspaces.switch", "order": 1},
})
scan()
publish_status()
threading.Thread(target=timer, daemon=True).start()
ext.run()
