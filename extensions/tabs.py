#!/usr/bin/env python3
"""vqro extension: tabs. New tab with a shell, show a resource or view in a tab, rename and close tabs.

Contributes the tab bar `+`, the tab context menu items, and the `n` prefix key.
"""
import copy
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from vqro_ext import Extension, VqroError, first_tabs, node_at, views_in  # noqa: E402
from vqro_views import ended, terminal_target, title, view_resource

ext = Extension("tabs", event_types=["entries", "resource", "resource.removed"])


def focus_view(node, view):
    if node.get("view") == view:
        return True
    for i, tab in enumerate(node.get("tabs", [])):
        if focus_view(tab["node"], view):
            node["active"] = i
            return True
    return any(focus_view(child, view) for child in node.get("children", []))


def show(view, title, view_value=None):
    """Activate the tab showing `view`, or add one."""
    def attempt():
        ops = []
        if view_value is not None and ext.value(f"view/{view}") != view_value:
            ops.append({"op": "set", "key": f"view/{view}", "value": view_value, "expect": ext.version(f"view/{view}")})
        tab = {"title": title, "node": {"view": view}}
        mark_ended({"tabs": [tab]})
        lay = ext.active_layout()
        if lay is None:
            ops += [
                {"op": "set", "key": "layout/main", "value": {"tabs": [tab], "active": 0}, "expect": ext.version("layout/main")},
                {"op": "set", "key": "workspace/active", "value": {"layout": "main"}, "expect": 0},
            ]
        else:
            key, ver, root = lay
            t = first_tabs(root)
            if focus_view(root, view):
                pass
            elif t is None:
                root = {"tabs": [{"title": "main", "node": root}, tab], "active": 1}
            else:
                t["tabs"].append(tab)
                t["active"] = len(t["tabs"]) - 1
            ops.append({"op": "set", "key": key, "value": root, "expect": ver})
        return ext.apply(ops, f"tabs: show {view}")
    # Navigation only: bounded CAS retries recompute from fresh state, never repeat spawn/input.
    # Three attempts proved too little during concurrent layout churn (soak-4).
    result = ext.retrying(attempt, attempts=8)
    return dict(result or {}, client={"focus": {"view": view}})


def title_of(r):
    return title(r, ext.resources)


@ext.command("tabs.open", "New tab with a shell (args: argv, cwd, name)")
def open_tab(args):
    spawn = {k: args[k] for k in ("argv", "cwd", "name") if k in args}
    r = ext.vqro.call("resource.spawn", spawn)["resource"]
    return show(f"term-{r['id']}", title_of(r), {"kind": "terminal", "resource": r["id"]})


@ext.command("tabs.show", "Show a resource (args: resource, or picked) or view (args: view, title) in a tab")
def show_cmd(args):
    rid = args.get("resource") or args.get("picked")  # `picked`: from a client's resource picker
    if rid:
        ext.fresh()
        resource, result = terminal_target(ext, rid)
        if resource is None:
            return result
        rid = resource["id"]
        return show(f"term-{rid}", title_of(resource), {"kind": "terminal", "resource": rid})
    if "view" in args:
        value = ext.value("view/" + args["view"]) or {}
        if value.get("kind") == "terminal":
            return show_cmd({"resource": value["resource"]})
        return show(args["view"], args.get("title") or args["view"])
    raise VqroError("needs `resource` or `view`", "invalid")


def tab_target(args):
    ctx = args.get("context") or {}
    if "tab_path" not in ctx:
        raise VqroError("no tab here", "invalid")
    key = f"layout/{ctx['layout']}"
    root = copy.deepcopy(ext.value(key))
    tabs = node_at(root, ctx["tab_path"]) if root else None
    if not tabs or "tabs" not in tabs:
        raise VqroError("that tab is gone", "conflict")
    return key, ctx.get("layout_version"), root, tabs, ctx["tab_index"]


def orphan_view_deletes(candidates, layouts):
    """Delete only views this action removed, after checking every resulting layout.

    Unreferenced popup/declaration views not closed by the action are not garbage.
    The layout writes and CAS-guarded deletes are committed in one transaction.
    """
    candidates = set(candidates)
    if not candidates:
        return []
    used = set()
    for key, entry in list(ext.entries.items()):
        if key.startswith("layout/"):
            used.update(views_in(layouts.get(key, entry["value"])))
    return [{"op": "delete", "key": "view/"+view, "expect": ext.version("view/"+view)}
            for view in sorted(candidates-used) if "view/"+view in ext.entries]


@ext.command("tabs.close", "Close a tab (its programs keep running)")
def close_tab(args):
    key, ver, root, tabs, i = tab_target(args)
    if len(tabs["tabs"]) <= 1:
        raise VqroError("can't close the last tab", "invalid")
    removed = views_in(tabs["tabs"].pop(i)["node"])
    a = tabs.get("active", 0)
    tabs["active"] = min(a - 1 if a >= i and a > 0 else a, len(tabs["tabs"]) - 1)
    ops = [{"op": "set", "key": key, "value": root, "expect": ver}]
    ops += orphan_view_deletes(removed, {key: root})
    ext.apply(ops, "tabs: close")


@ext.command("tabs.rename", "Rename a tab (args: text)")
def rename_tab(args):
    key, ver, root, tabs, i = tab_target(args)
    text = (args.get("text") or "").strip()
    if not text:
        raise VqroError("empty name", "invalid")
    tabs["tabs"][i]["title"] = text
    ext.apply([{"op": "set", "key": key, "value": root, "expect": ver}], "tabs: rename")


def prune_ended(node):
    """Close wholly ended tabs, preserving mixed tabs and the last tab."""
    for child in node.get("children", []):
        prune_ended(child)
    tabs = node.get("tabs", [])
    for tab in tabs:
        prune_ended(tab["node"])
    if len(tabs) < 2:
        return
    active = tabs[min(node.get("active", 0), len(tabs)-1)]
    def dead(tab):
        resources = [view_resource(ext, view) for view in views_in(tab["node"])]
        return bool(resources) and all(rid and ended(ext.resources.get(rid)) for rid in resources)
    kept = [tab for tab in tabs if not dead(tab)] or [active]
    node["tabs"] = kept
    node["active"] = next((i for i, tab in enumerate(kept) if tab is active), 0)


@ext.command("tabs.close_ended", "Close ended/gone resource tabs (programs are never killed)")
def close_ended(args):
    def attempt():
        ext.fresh()
        ops, removed, layouts = [], set(), {}
        for key, entry in list(ext.entries.items()):
            if key.startswith("layout/"):
                root = copy.deepcopy(entry["value"])
                prune_ended(root)
                if root != entry["value"]:
                    ops.append({"op": "set", "key": key, "value": root, "expect": entry["version"]})
                    removed.update(set(views_in(entry["value"]))-set(views_in(root)))
                    layouts[key] = root
        ops += orphan_view_deletes(removed, layouts)
        return ext.apply(ops, "tabs: close ended") if ops else {}
    return ext.retrying(attempt)


def mark_ended(node):
    for child in node.get("children", []):
        mark_ended(child)
    for tab in node.get("tabs", []):
        mark_ended(tab["node"])
        ids = [view_resource(ext, view) for view in views_in(tab["node"])]
        suffix = ""
        if ids and all(rid and ended(ext.resources.get(rid)) for rid in ids):
            suffix = " [gone]" if any(rid not in ext.resources for rid in ids) else " [ended]"
        text = tab.get("title", "")
        for old in (" [gone]", " [ended]"):
            if text.endswith(old):
                text = text[:-len(old)]
                break
        tab["title"] = text + suffix


resource_states = {}


@ext.on_event
def resource_state(kind, event):
    if kind not in ("resource", "resource.removed", "missed"):
        return
    rid = event.get("id")
    if kind == "resource":
        state = ended(ext.resources.get(rid))
        if rid in resource_states and resource_states[rid] == state:
            return  # High-frequency working/progress facts never scan or write layouts.
        resource_states[rid] = state
    elif kind == "resource.removed":
        resource_states.pop(rid, None)
    else:
        resource_states.clear()
        resource_states.update((rid, ended(r)) for rid, r in ext.resources.items())
    def attempt():
        ops = []
        for key, entry in list(ext.entries.items()):
            if key.startswith("layout/"):
                root = copy.deepcopy(entry["value"])
                mark_ended(root)
                if root != entry["value"]:
                    ops.append({"op": "set", "key": key, "value": root, "expect": entry["version"]})
        if ops:
            ext.apply(ops, "tabs: mark ended/gone")
    ext.retrying(attempt)


ext.contribute({
    # An icon alone: the label stays empty so a set's `add` glyph replaces the `+`.
    "menu/tabs.new": {"where": "tabbar", "label": "", "icon": {"name": "add", "text": "+"}, "command": "tabs.open", "order": 0},
    "menu/tabs.rename": {"where": "tab", "label": "Rename tab", "command": "tabs.rename", "input": "Tab name", "prefill": {"context": "tab_title"}, "order": 10},
    "menu/tabs.close": {"where": "tab", "label": "Close tab", "command": "tabs.close", "order": 20},
    "menu/tabs.close-ended": {"where": "tabbar", "label": "Close ended tabs", "command": "tabs.close_ended", "order": 30},
    "binding/tabs.new": {"key": "n", "command": "tabs.open", "label": "new shell in a tab"},
})
resource_state("missed", {})
ext.run()
