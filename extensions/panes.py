#!/usr/bin/env python3
"""vqro extension: panes. Split, rename, close, kill, and show another terminal in a pane.

All actions arrive with the client's context (layout, layout_version, path, view, resource) and edit
the layout with compare-and-set, so a pane that moved meanwhile is reported instead of mangled.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from vqro_ext import Extension, VqroError, node_at, replace_at, views_in  # noqa: E402

from vqro_views import terminal_target, title

ext = Extension("panes", event_types=["entries", "resource", "resource.removed"])


def target(args):
    ctx = args.get("context") or {}
    if "path" not in ctx or "layout" not in ctx:
        raise VqroError("no pane here", "invalid")
    key = f"layout/{ctx['layout']}"
    root = ext.value(key)
    if root is None or node_at(root, ctx["path"]) is None:
        raise VqroError("that pane is gone", "conflict")
    return ctx, key, ctx.get("layout_version"), root


@ext.command("panes.split", "Split a pane with a new shell in the layout's folder (args: dir = right | down)")
def split(args):
    ctx, key, ver, root = target(args)
    folder = ext.layout_folder(args)  # the pane's layout's starting folder, if it names one
    r = ext.vqro.call("resource.spawn", {"cwd": folder} if folder else {})["resource"]
    view = f"term-{r['id']}"
    old = node_at(root, ctx["path"])
    new = {"split": "v" if args.get("dir") == "down" else "h", "children": [old, {"view": view}], "sizes": [1, 1]}
    ext.apply([
        {"op": "set", "key": f"view/{view}", "value": {"kind": "terminal", "resource": r["id"]}},
        {"op": "set", "key": key, "value": replace_at(root, ctx["path"], new), "expect": ver},
    ], "panes: split")


@ext.command("panes.rename", "Rename a pane (args: text; empty resets)")
def rename(args):
    ctx, key, ver, root = target(args)
    view = ctx.get("view")
    vkey = f"view/{view}"
    v = dict(ext.value(vkey) or {})
    text = (args.get("text") or "").strip()
    if text:
        v["title"] = text
    else:
        v.pop("title", None)
    ops = [{"op": "set", "key": vkey, "value": v, "expect": ext.version(vkey)}]
    # A pane that is a whole tab renames its tab too.
    path = ctx["path"]
    if path and text:
        parent = node_at(root, path[:-1])
        if "tabs" in parent:
            parent["tabs"][path[-1]]["title"] = text
            ops.append({"op": "set", "key": key, "value": root, "expect": ver})
    ext.apply(ops, "panes: rename")


def new_shell(key, ver, cwd=None):
    """Spawn the shell that replaces the only pane or last tab (once; only after the layout check), in the
    layout's folder when it names one."""
    if ver is not None and ext.version(key) != ver:
        raise VqroError("the layout changed meanwhile; nothing closed, try again", "conflict")
    r = ext.vqro.call("resource.spawn", {"cwd": cwd} if cwd else {})["resource"]
    return r, f"term-{r['id']}"


@ext.command("panes.close", "Close a pane (its program keeps running; closing the only pane opens a new shell)")
def close(args):
    ctx, key, ver, root = target(args)
    path = ctx["path"]
    shell = None
    parent = node_at(root, path[:-1]) if path else None
    i = path[-1] if path else None
    if not path or ("tabs" in parent and len(parent["tabs"]) <= 1):
        # The only pane (or the last tab's whole content): replace it with a new shell, as tabs.open would.
        shell, view_id = new_shell(key, ver, ext.layout_folder(args))
        if not path:
            root = {"view": view_id}
        else:
            parent["tabs"][i] = {"title": title(shell, ext.resources), "node": {"view": view_id}}
    elif "children" in parent:
        parent["children"].pop(i)
        if "sizes" in parent and i < len(parent["sizes"]):
            parent["sizes"].pop(i)
        if len(parent["children"]) == 1:
            root = replace_at(root, path[:-1], parent["children"][0])
    else:
        parent["tabs"].pop(i)
        a = parent.get("active", 0)
        parent["active"] = min(a - 1 if a >= i and a > 0 else a, len(parent["tabs"]) - 1)
    ops = [{"op": "set", "key": key, "value": root, "expect": ver}]
    if shell:
        ops.insert(0, {"op": "set", "key": f"view/{view_id}", "value": {"kind": "terminal", "resource": shell["id"]}})
    view = ctx.get("view")
    used = set(views_in(root))
    for k, e in ext.entries.items():
        if k.startswith("layout/") and k != key:
            used.update(views_in(e["value"]))
    if view and view not in used:
        ops.append({"op": "delete", "key": f"view/{view}"})
    try:
        ext.apply(ops, "panes: close")
    except VqroError as error:
        if shell:
            ext.notify(f"New shell {shell['id']} started but not shown ({error.kind}); open it from the sidebar", "warn")
        raise
    if shell:
        return {"client": {"focus": {"view": view_id}}}


@ext.command("panes.kill", "Kill the program in a pane")
def kill(args):
    ctx = args.get("context") or {}
    # Panels may name the resource they show (agent panels); only a terminal pane's program is killed.
    if not ctx.get("resource") or ctx.get("kind") != "terminal":
        raise VqroError("no program here", "invalid")
    return ext.vqro.call("resource.kill", {"resource": ctx["resource"]})


@ext.command("panes.show", "Show another resource in a pane (args: picked)")
def show(args):
    ctx = args.get("context") or {}
    vkey = f"view/{ctx.get('view')}"
    v = dict(ext.value(vkey) or {})
    if v.get("kind") != "terminal" or not args.get("picked"):
        raise VqroError("pick a terminal for a terminal pane", "invalid")
    ext.fresh()
    resource, result = terminal_target(ext, args["picked"])
    if resource is None:
        return result
    v["resource"] = resource["id"]
    ext.apply([{"op": "set", "key": vkey, "value": v, "expect": ext.version(vkey)}], "panes: show")


ext.contribute({
    "menu/panes.rename": {"where": "pane", "label": "Rename", "command": "panes.rename", "input": "Pane name", "prefill": {"context": "title"}, "order": 10},
    "menu/panes.split-right": {"where": "pane", "label": "Split right (new shell)", "command": "panes.split", "args": {"dir": "right"}, "order": 20},
    "menu/panes.split-down": {"where": "pane", "label": "Split down (new shell)", "command": "panes.split", "args": {"dir": "down"}, "order": 21},
    "menu/panes.show": {"where": "pane", "label": "Show another resource", "command": "panes.show", "pick": "resource", "when": {"kind": "terminal"}, "order": 30},
    "menu/panes.close": {"where": "pane", "label": "Close pane (program keeps running)", "command": "panes.close", "order": 40},
    "menu/panes.kill": {"where": "pane", "label": "Kill program", "command": "panes.kill", "when": {"live": True, "kind": "terminal"}, "order": 50},
    "binding/panes.menu": {"key": "m", "command": "client.menu", "args": {"where": "pane"}, "label": "pane menu"},
    # h/v follow the layout's split names: "h" puts panes side by side, "v" stacks them.
    "binding/panes.split-h": {"key": "h", "command": "panes.split", "args": {"dir": "right"}, "label": "split side by side"},
    "binding/panes.split-v": {"key": "v", "command": "panes.split", "args": {"dir": "down"}, "label": "split stacked"},
    "binding/panes.close": {"key": "x", "command": "panes.close", "label": "close pane (program keeps running)"},
    "binding/panes.zoom": {"key": "z", "command": "client.zoom", "label": "zoom pane (toggle)"},
})
ext.run()
