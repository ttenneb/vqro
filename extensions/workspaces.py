#!/usr/bin/env python3
"""prim extension: workspaces. Folders you add by hand: git repos (with their worktrees) or plain folders.

Owns the list (`doc/workspaces/list`) and the git facts. It publishes them as live data under the name
`workspaces` (`{"data": [...]}`) for any extension to draw; the sidebar turns them into a tree. Git is
read on changes and on a slow timer, never continuously.
"""
import os
import subprocess
import sys
import threading
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from prim_ext import Extension, PrimError  # noqa: E402

ext = Extension("workspaces", event_types=["entries"])  # no resource events: not used here
LIST = "doc/workspaces/list"
REFRESH_SECS = 20
lock = threading.Lock()


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
                item["worktrees"] = [w for w in wts if w is not main and not w.get("bare")]
            data.append(item)
        ext.publish("workspaces", {"data": data, "at": time.time()})


@ext.on_event
def on_event(t, d):
    if t == "entries" and any(c["key"] == LIST for c in d["changes"]):
        threading.Thread(target=scan, daemon=True).start()


def save(items, note):
    ext.apply([{"op": "set", "key": LIST, "value": {"items": items}, "expect": ext.version(LIST)}], note)


@ext.command("workspaces.add", "Add a workspace folder (args: text = path)")
def add(args):
    raw = (args.get("text") or args.get("path") or "").strip()
    if not raw:
        raise PrimError("give a folder path", "invalid")
    path = os.path.abspath(os.path.expanduser(raw))
    if not os.path.isdir(path):
        raise PrimError(f"not a folder: {path}", "invalid")
    items = entries()
    if any(os.path.abspath(os.path.expanduser(i["path"])) == path for i in items):
        raise PrimError("already added", "conflict")
    items.append({"path": path})
    save(items, "workspaces: add")
    ext.notify(f"added {os.path.basename(path)}", "ok")


@ext.command("workspaces.remove", "Remove a workspace from the list (args: path; files untouched)")
def remove(args):
    path = args.get("path")
    items = [i for i in entries() if os.path.abspath(os.path.expanduser(i["path"])) != path]
    save(items, "workspaces: remove")


@ext.command("workspaces.open", "Open a shell in a folder (args: path)")
def open_here(args):
    path = args["path"]
    return ext.invoke("tabs.open", {"cwd": path, "name": os.path.basename(path.rstrip("/"))})


@ext.command("workspaces.worktree", "New git worktree (args: repo, text = branch)")
def new_worktree(args):
    repo, branch = args.get("repo"), (args.get("text") or "").strip()
    if not repo or not branch:
        raise PrimError("needs a repo and a branch name", "invalid")
    dest = os.path.join(os.path.dirname(repo.rstrip("/")), f"{os.path.basename(repo.rstrip('/'))}-{branch.replace('/', '-')}")
    exists = git(repo, "rev-parse", "--verify", "--quiet", f"refs/heads/{branch}") is not None
    cmd = ["git", "-C", repo, "worktree", "add", dest] + ([branch] if exists else ["-b", branch])
    r = subprocess.run(cmd, capture_output=True, text=True, timeout=60)
    if r.returncode != 0:
        ext.notify(f"worktree failed: {r.stderr.strip().splitlines()[-1] if r.stderr.strip() else r.returncode}", "error")
        raise PrimError(r.stderr.strip() or "git worktree add failed", "backend")
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


scan()
threading.Thread(target=timer, daemon=True).start()
ext.run()
