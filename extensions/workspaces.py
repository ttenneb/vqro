#!/usr/bin/env python3
"""vqro extension: workspaces. Folders you add by hand: git repos (with their worktrees) or plain folders.

Owns the list (`doc/workspaces/list`) and the git facts. It publishes them as live data under the name
`workspaces` (`{"data": [...]}`) for any extension to draw; the sidebar turns them into a tree. Git is
read on changes and on a slow timer, never continuously.

Each workspace (and each worktree) has its own tabs: its own layout, `layout/ws-<id>`, where `<id>` comes
from the folder's real path, so the same folder always finds the same layout. Opening a workspace shows its
layout in the window you opened it from (the reply's `{"client": {"layout"}}`; other windows keep theirs)
and makes it the shared default where new windows open (`workspace/active`), creating it with one shell in
the folder the first time. The
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
from vqro_process import background_kwargs  # noqa: E402
from vqro_scratch import CLAIM, FirstShell, clear_moot_claim, empty as scratch_empty, new_shell_hint, shell_tree, unconfirmed_notice

ext = Extension("workspaces", event_types=["entries", "provider"])  # no resource events: not used here
LIST = "doc/workspaces/list"
MAIN = "main"
PREFS = "doc/settings/workspaces"
bootstrap_lock = threading.Lock()
decorate_pending = [False]
REFRESH_SECS = 20
lock = threading.Lock()
last_data = [[]]  # what scan() last published (this extension doesn't receive its own live data)


def git(path, *args):
    """git's output, or None when it fails or git isn't installed (folders are then plain folders)."""
    try:
        r = subprocess.run(["git", "-C", path, *args], capture_output=True, text=True, timeout=10, **background_kwargs())
    except (OSError, subprocess.TimeoutExpired):
        return None
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


def ahead_of(wt, branch):
    """{ahead: n}: commits in the worktree not on the repo's own branch (empty when unknown)."""
    if not branch or branch == "(detached)":
        return {}
    out = git(wt["path"], "rev-list", "--count", f"{branch}..HEAD")
    return {"ahead": int(out)} if out is not None and out.strip().isdigit() else {}


def entries():
    return list((ext.value(LIST) or {}).get("items", []))


def norm(path):
    return os.path.abspath(os.path.expanduser(path))


def layout_name(path):
    """The layout of the workspace at `path`: stable for the folder, the same for every caller."""
    return "ws-" + hashlib.sha256(os.path.realpath(norm(path)).encode()).hexdigest()[:12]


def folder_title(path):
    return os.path.basename(norm(path).rstrip("/" + os.sep)) or norm(path)


def scan():
    with lock:
        data = []
        for e in entries():
            path = os.path.expanduser(e["path"])
            item = {"path": path, "name": e.get("name") or os.path.basename(path.rstrip("/" + os.sep)) or path}
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
                item["worktrees"] = [dict(w, layout=layout_name(w["path"]), **ahead_of(w, item["branch"]))
                                     for w in wts if w is not main and not w.get("bare")]
            # The layout this workspace's tabs live in (it may not exist until first opened).
            item["layout"] = layout_name(item["path"])
            data.append(item)
        last_data[0] = data
        ext.publish("workspaces", {"data": data, "at": time.time()})


@ext.on_event
def on_event(t, d):
    if t == "missed" or t == "entries" and any(c["key"] == LIST for c in d["changes"]):
        threading.Thread(target=scan, daemon=True).start()
    if t in ("provider", "missed") or t == "entries" and any(c["key"] in (PREFS, "layout/main") for c in d["changes"]):
        # Do not perform effects on the SDK listener: it must keep caches moving, and layout-only
        # CAS retries after an observed spawn must not be reduced to a deferred listener replay.
        threading.Thread(target=refresh_startup, daemon=True).start()
    if t == "entries" and any(c["key"] in ("view/sidebar", "layout/main") for c in d["changes"]):
        decorate_scratch()


def save(items, note, extra=()):
    ext.apply([{"op": "set", "key": LIST, "value": {"items": items}, "expect": ext.version(LIST)}, *extra], note)


# -- layouts --------------------------------------------------------------------------------------

def select(name):
    """Ops that make `name` the shared default: the workspace last switched to, where new windows open
    (and what clients without per-window workspaces show). Only if it isn't already."""
    if (ext.value("workspace/active") or {}).get("layout") == name:
        return []
    return [{"op": "set", "key": "workspace/active", "value": {"layout": name}, "expect": ext.version("workspace/active")}]


def shown_in_window(result, name):
    """Ask the invoking window to show layout `name` (other windows keep theirs)."""
    return dict(result or {}, client=dict((result or {}).get("client") or {}, layout=name))


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


def create(name, cwd, label, resource, args=None):
    """Ops for a new layout `name` with one tab showing `resource`, started in `cwd`, and selected."""
    view = f"term-{resource['id']}"
    active = ext.context_layout(args)  # the invoking window's layout lends its panels (the sidebar)
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


def open_layout(name, cwd, label, args=None, first_shell=None):
    """Show layout `name` in the invoking window and make it the shared default, creating it with one shell
    (in `cwd`) if missing. Returns (result, created).

    The shell is spawned at most once; only the layout write is retried (compare-and-set), recomputed
    from fresh state, never a second spawn."""
    if ext.value(f"layout/{name}") is not None and first_shell is None:
        def switch():
            if ext.value(f"layout/{name}") is None:
                raise VqroError("that workspace's layout was removed meanwhile", "conflict")
            ops = select(name)
            return ext.apply(ops, f"workspaces: switch to {label}") if ops else {}
        return shown_in_window(ext.retrying(switch, attempts=8), name), False
    spawn = {"cwd": cwd} if cwd else {}
    resource = first_shell.spawn(spawn) if first_shell is not None else ext.vqro.call("resource.spawn", spawn)["resource"]

    def attempt():
        if ext.value(f"layout/{name}") is None:
            ops = create(name, cwd, label, resource, args)
        elif first_shell is not None:
            view = f"term-{resource['id']}"
            root = shell_tree(ext, ext.value(f"layout/{name}"), {"title": title(resource, {}), "node": {"view": view}})
            ops = [{"op": "set", "key": f"view/{view}", "value": {"kind": "terminal", "resource": resource["id"]}},
                   {"op": "set", "key": f"layout/{name}", "value": root, "expect": ext.version(f"layout/{name}")}] + select(name)
        else:
            ops = add_tab(name, resource)
        if first_shell is not None:
            ops += first_shell.finish_ops()
        return ext.apply(ops, f"workspaces: open {label}")
    return shown_in_window(ext.retrying(attempt, attempts=8), name), True


# -- empty Scratch startup policy -----------------------------------------------------------------

startup_notice = [""]


def startup_prefs():
    prefs = ext.value(PREFS)
    return prefs if isinstance(prefs, dict) else {}


def open_when_empty():
    return startup_prefs().get("open_shell_when_empty", True) is not False


def publish_settings():
    ext.publish("settings.page.workspaces", {"title": "Workspaces", "items": [
        {"type": "toggle", "text": "Open a shell in an empty Scratch", "checked": open_when_empty(),
         "detail": startup_notice[0] or "Also applies when the core starts without a UI",
         "command": "workspaces.startup", "args": {"value": not open_when_empty()}}]})


def decorate_scratch():
    if not decorate_pending[0] or ext.value("view/sidebar") is None:
        return
    def attempt():
        root = ext.value("layout/main")
        if root is None or scratch_empty(ext):
            return
        if "sidebar" not in views_in(root):
            ext.apply([{"op": "set", "key": "layout/main", "value": {"split": "h", "children": [
                {"view": "sidebar"}, root], "sizes": [1, 3]}, "expect": ext.version("layout/main")}], "Scratch: initial sidebar")
        decorate_pending[0] = False
    ext.retrying(attempt, attempts=8)


def refresh_startup():
    bootstrap()
    publish_settings()


def bootstrap():
    # Queue availability behind an in-flight attempt rather than dropping the only ready event.
    # Centrally owned by this extension, not once per client. Never override a selected workspace.
    bootstrap_lock.acquire()
    try:
        ext.fresh()
        if not scratch_empty(ext):
            clear_moot_claim(ext)
            startup_notice[0] = ""
            return
        if ext.value(CLAIM) is not None:
            startup_notice[0] = unconfirmed_notice(ext)
            ext.notify(startup_notice[0], "warn")
            return
        selected = (ext.value("workspace/active") or {}).get("layout")
        if not open_when_empty() or selected not in (None, MAIN) and ext.value(f"layout/{selected}") is not None:
            decorate_pending[0] = False
            return
        decorate_pending[0] = True
        try:
            with FirstShell(ext) as first:
                if first is None:
                    startup_notice[0] = unconfirmed_notice(ext) if ext.value(CLAIM) is not None else ""
                    return
                open_layout(MAIN, None, "Scratch", first_shell=first)
            startup_notice[0] = ""
            ext.fresh()
            decorate_scratch()
        except VqroError as error:
            startup_notice[0] = f"No Scratch shell opened: {error}; {new_shell_hint(ext)}"
            ext.notify(startup_notice[0], "warn")
    finally:
        bootstrap_lock.release()


@ext.command("workspaces.startup", "Set whether empty Scratch starts with a shell (args: value)")
def set_startup(args):
    if not isinstance(args.get("value"), bool):
        raise VqroError("value must be true or false", "invalid")
    def attempt():
        prefs = dict(startup_prefs(), open_shell_when_empty=args["value"])
        return ext.apply([{"op": "set", "key": PREFS, "value": prefs, "expect": ext.version(PREFS)}], "workspaces: startup preference")
    result = ext.retrying(attempt, attempts=8)
    ext.fresh()
    bootstrap()
    publish_settings()
    return result


# -- commands -------------------------------------------------------------------------------------

@ext.command("workspaces.add", "Add a workspace folder (args: text = path)")
def add(args):
    raw = (args.get("text") or args.get("path") or "").strip()
    if not raw:
        raise VqroError("give a folder path", "invalid")
    path = norm(raw)
    if not os.path.isdir(path):
        raise VqroError(f"not a folder: {path}", "invalid")

    def attempt():
        # From the cache, which may not have seen a write that just happened (two adds in a row): a conflict
        # reloads it and tries again, so the list is never written without an entry it already has.
        items = entries()
        if any(norm(i["path"]) == path for i in items):
            return False
        items.append({"path": path})
        save(items, "workspaces: add")
        return True
    if not ext.retrying(attempt):
        raise VqroError("already added", "conflict")
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
    result, _ = open_layout(layout_name(path), path, folder_title(path), args)
    return result


@ext.command("workspaces.shell", "Open a new shell tab in a workspace, switching to it (args: path)")
def shell(args):
    path = norm(args.get("path") or "")
    if not os.path.isdir(path):
        raise VqroError(f"not a folder: {path}", "invalid")
    name = layout_name(path)
    result, created = open_layout(name, path, folder_title(path), args)
    # The new tab goes to that workspace, wherever the command came from.
    return result if created else shown_in_window(ext.invoke("tabs.open", {"cwd": path, "context": {"layout": name}}), name)


@ext.command("workspaces.main", "Switch to the main tabs (terminals not in a workspace)")
def main_layout(args):
    result, _ = open_layout(MAIN, None, None, args)
    return result


def switcher():
    """The `workspaces.switch` popup: main, then every workspace and worktree. Each window marks the one it
    shows itself (`current_when`): this content is shared."""
    data = last_data[0]

    def row(text, command, name, args=None, depth=0):
        out = {"id": f"switch:{name}", "text": ("  " * depth) + text, "command": command, "args": args or {},
               "current_when": {"layout": name}}
        if ext.value(f"layout/{name}") is None:
            out["detail"] = "new"
        return out
    items = [row("main", "workspaces.main", MAIN)]
    for w in data:
        items.append(row(w["name"], "workspaces.open", w["layout"], {"path": w["path"]}))
        for t in w.get("worktrees", []):
            items.append(row(os.path.basename(t["path"].rstrip("/" + os.sep)), "workspaces.open", t["layout"], {"path": t["path"]}, 1))
    items.append({"type": "separator"})
    items.append({"text": "Add workspace…", "command": "workspaces.add", "input": "Folder path"})
    return {"title": "Switch workspace", "items": items}


@ext.command("workspaces.switch", "Choose a workspace to switch to (opens a popup)")
def switch_cmd(args):
    ext.publish("workspaces.switch", switcher())
    return {"client": {"popup": {"view": "workspaces.switch", "width": 52}}}


@ext.command("workspaces.worktree", "New git worktree (args: repo, text = branch)")
def new_worktree(args):
    repo, branch = args.get("repo"), (args.get("text") or "").strip()
    if not repo or not branch:
        raise VqroError("needs a repo and a branch name", "invalid")
    dest = os.path.join(os.path.dirname(repo.rstrip("/" + os.sep)), f"{os.path.basename(repo.rstrip('/' + os.sep))}-{branch.replace('/', '-')}")
    exists = git(repo, "rev-parse", "--verify", "--quiet", f"refs/heads/{branch}") is not None
    cmd = ["git", "-C", repo, "worktree", "add", dest] + ([branch] if exists else ["-b", branch])
    r = subprocess.run(cmd, capture_output=True, text=True, timeout=60, **background_kwargs())
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
    # Each window names the workspace it shows (its layout's title), from its own context.
    "menu/workspaces.current": {"where": "status", "label": "", "label_context": "layout_title",
                                "icon": {"name": "folder", "text": "▫"}, "command": "workspaces.switch", "order": 1},
})
scan()
bootstrap()
publish_settings()
threading.Thread(target=timer, daemon=True).start()
ext.run()
