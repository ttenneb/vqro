#!/usr/bin/env python3
"""prim extension: tabs. New tab with a shell, show a resource or view in a tab, rename and close tabs.

Contributes the tab bar `+`, the tab context menu items, and the `n` prefix key.
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from prim_ext import Extension, PrimError, first_tabs, node_at  # noqa: E402

ext = Extension("tabs")


def show(view, title, view_value=None):
    """Activate the tab showing `view`, or add one."""
    def attempt():
        ops = []
        if view_value is not None and ext.value(f"view/{view}") != view_value:
            ops.append({"op": "set", "key": f"view/{view}", "value": view_value})
        tab = {"title": title, "node": {"view": view}}
        lay = ext.active_layout()
        if lay is None:
            ops += [
                {"op": "set", "key": "layout/main", "value": {"tabs": [tab], "active": 0}},
                {"op": "set", "key": "workspace/active", "value": {"layout": "main"}},
            ]
        else:
            key, ver, root = lay
            t = first_tabs(root)
            if t is None:
                root = {"tabs": [{"title": "main", "node": root}, tab], "active": 1}
            else:
                found = [i for i, x in enumerate(t["tabs"]) if x["node"].get("view") == view]
                if found:
                    t["active"] = found[0]
                else:
                    t["tabs"].append(tab)
                    t["active"] = len(t["tabs"]) - 1
            ops.append({"op": "set", "key": key, "value": root, "expect": ver})
        return ext.apply(ops, f"tabs: show {view}")
    return ext.retrying(attempt)


def title_of(r):
    return r.get("name") or (r.get("argv") or ["shell"])[0].split("/")[-1]


@ext.command("tabs.open", "New tab with a shell (args: argv, cwd, name)")
def open_tab(args):
    spawn = {k: args[k] for k in ("argv", "cwd", "name") if k in args}
    r = ext.prim.call("resource.spawn", spawn)["resource"]
    return show(f"term-{r['id']}", title_of(r), {"kind": "terminal", "resource": r["id"]})


@ext.command("tabs.show", "Show a resource (args: resource, or picked) or view (args: view, title) in a tab")
def show_cmd(args):
    rid = args.get("resource") or args.get("picked")  # `picked`: from a client's resource picker
    if rid:
        return show(f"term-{rid}", title_of(ext.resources.get(rid, {})), {"kind": "terminal", "resource": rid})
    if "view" in args:
        return show(args["view"], args.get("title") or args["view"])
    raise PrimError("needs `resource` or `view`", "invalid")


def tab_target(args):
    ctx = args.get("context") or {}
    if "tab_path" not in ctx:
        raise PrimError("no tab here", "invalid")
    key = f"layout/{ctx['layout']}"
    root = ext.value(key)
    tabs = node_at(root, ctx["tab_path"]) if root else None
    if not tabs or "tabs" not in tabs:
        raise PrimError("that tab is gone", "conflict")
    return key, ctx.get("layout_version"), root, tabs, ctx["tab_index"]


@ext.command("tabs.close", "Close a tab (its programs keep running)")
def close_tab(args):
    key, ver, root, tabs, i = tab_target(args)
    if len(tabs["tabs"]) <= 1:
        raise PrimError("can't close the last tab", "invalid")
    tabs["tabs"].pop(i)
    a = tabs.get("active", 0)
    tabs["active"] = min(a - 1 if a >= i and a > 0 else a, len(tabs["tabs"]) - 1)
    ext.apply([{"op": "set", "key": key, "value": root, "expect": ver}], "tabs: close")


@ext.command("tabs.rename", "Rename a tab (args: text)")
def rename_tab(args):
    key, ver, root, tabs, i = tab_target(args)
    text = (args.get("text") or "").strip()
    if not text:
        raise PrimError("empty name", "invalid")
    tabs["tabs"][i]["title"] = text
    ext.apply([{"op": "set", "key": key, "value": root, "expect": ver}], "tabs: rename")


ext.contribute({
    # An icon alone: the label stays empty so a set's `add` glyph replaces the `+`.
    "menu/tabs.new": {"where": "tabbar", "label": "", "icon": {"name": "add", "text": "+"}, "command": "tabs.open", "order": 0},
    "menu/tabs.rename": {"where": "tab", "label": "Rename tab", "command": "tabs.rename", "input": "Tab name", "order": 10},
    "menu/tabs.close": {"where": "tab", "label": "Close tab", "command": "tabs.close", "order": 20},
    "binding/tabs.new": {"key": "n", "command": "tabs.open", "label": "new shell in a tab"},
})
ext.run()
