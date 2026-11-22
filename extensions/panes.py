#!/usr/bin/env python3
"""vqro extension: panes. Split, rename, close, kill, show another terminal in a pane, and rearrange
panes: swap, rotate, resize, break out to a tab, move to another tab, and layout presets.

All actions arrive with the client's context (layout, layout_version, path, view, resource) and edit
the layout with compare-and-set, so a pane that moved meanwhile is reported instead of mangled.
"""
import copy
import math
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


@ext.command("panes.split", "Split a pane with a new shell in the layout's folder (args: dir = right | down; argv, cwd to start another program)")
def split(args):
    ctx, key, ver, root = target(args)
    folder = args.get("cwd") or ext.layout_folder(args)  # else the pane's layout's starting folder, if it names one
    spawn = {"cwd": folder} if folder else {}
    if isinstance(args.get("argv"), list) and args["argv"] and all(isinstance(a, str) for a in args["argv"]):
        spawn["argv"] = args["argv"]
    r = ext.vqro.call("resource.spawn", spawn)["resource"]
    view = f"term-{r['id']}"
    old = node_at(root, ctx["path"])
    new = {"split": "v" if args.get("dir") == "down" else "h", "children": [old, {"view": view}], "sizes": [1, 1]}
    ext.apply([
        {"op": "set", "key": f"view/{view}", "value": {"kind": "terminal", "resource": r["id"]}},
        {"op": "set", "key": key, "value": replace_at(root, ctx["path"], new), "expect": ver},
    ], "panes: split")
    if "argv" in spawn:  # a program asked for (an editor): it gets the keys
        return {"client": {"focus": {"view": view}}}


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


# ---- rearranging panes (all one layout transaction with compare-and-set) -----------------------------------

def scope_of(ctx):
    """The path of the node whose panes are rearranged: the pane's innermost tab, else the whole layout."""
    if isinstance(ctx.get("tab_path"), list) and isinstance(ctx.get("tab_index"), int):
        return ctx["tab_path"] + [ctx["tab_index"]]
    return []


def units(node, path):
    """Paths of the movable panes under a node, in reading order. A nested tab group moves as one."""
    if "children" in node:
        return [p for i, child in enumerate(node["children"]) for p in units(child, path + [i])]
    return [path]


def arrange(args):
    """(ctx, key, ver, root, scope, units, index of the invoking pane) for a rearranging command."""
    ctx, key, ver, root = target(args)
    root = copy.deepcopy(root)
    scope = scope_of(ctx)
    found = units(node_at(root, scope), scope)
    path = ctx["path"]
    here = next((i for i, p in enumerate(found) if path[:len(p)] == p), None)
    if here is None:
        raise VqroError("that pane is gone", "conflict")
    return ctx, key, ver, root, scope, found, here


def follow(ctx):
    """Keep focus on the pane that moved."""
    return {"client": {"focus": {"view": ctx["view"]}}} if ctx.get("view") else None


def put(root, paths, nodes):
    for p, n in zip(paths, nodes):
        root = replace_at(root, p, n)
    return root


@ext.command("panes.swap", "Swap a pane with the next or previous one in its tab (args: dir = next | prev)")
def swap(args):
    ctx, key, ver, root, _, found, i = arrange(args)
    if len(found) < 2:
        raise VqroError("only one pane here", "invalid")
    j = (i + (-1 if args.get("dir") == "prev" else 1)) % len(found)
    a, b = (copy.deepcopy(node_at(root, found[k])) for k in (i, j))
    ext.apply([{"op": "set", "key": key, "value": put(root, [found[i], found[j]], [b, a]), "expect": ver}], "panes: swap")
    return follow(ctx)


@ext.command("panes.rotate", "Rotate the panes in a tab one place (args: dir = next | prev)")
def rotate(args):
    ctx, key, ver, root, _, found, _ = arrange(args)
    if len(found) < 2:
        raise VqroError("only one pane here", "invalid")
    nodes = [copy.deepcopy(node_at(root, p)) for p in found]
    nodes = nodes[1:] + nodes[:1] if args.get("dir") == "prev" else nodes[-1:] + nodes[:-1]
    ext.apply([{"op": "set", "key": key, "value": put(root, found, nodes), "expect": ver}], "panes: rotate")
    return follow(ctx)


def take_out(root, path):
    """Remove the node at path from its split (dropping its size; a split left with one child becomes it)."""
    parent = node_at(root, path[:-1])
    i = path[-1]
    node = parent["children"].pop(i)
    if "sizes" in parent and i < len(parent["sizes"]):
        parent["sizes"].pop(i)
    if len(parent["children"]) == 1:
        root = replace_at(root, path[:-1], parent["children"][0])
    return root, node


def tab_group(root, ctx):
    """The pane's tab group and its tab index; a layout without tabs gets one (its content as tab `main`)."""
    if scope_of(ctx):
        return root, node_at(root, ctx["tab_path"]), ctx["tab_index"]
    group = {"tabs": [{"title": "main", "node": root}], "active": 0}
    return group, group, 0


@ext.command("panes.break", "Move a pane out to a new tab of its own")
def break_out(args):
    ctx, key, ver, root, scope, found, i = arrange(args)
    unit = found[i]
    if unit == scope:
        raise VqroError("this pane is already a whole tab", "invalid")
    root, node = take_out(root, unit)
    root, group, t = tab_group(root, ctx)
    # A terminal's tab gets the automatic name tabs.open would give, so it can show the program's title.
    resource = ext.resources.get(ctx.get("resource") or "")
    name = title(resource, ext.resources) if resource and ctx.get("kind") == "terminal" and "title" not in (ext.value(f"view/{ctx['view']}") or {}) else None
    group["tabs"].insert(t + 1, {"title": name or ctx.get("title") or "pane", "node": node})
    group["active"] = t + 1
    ext.apply([{"op": "set", "key": key, "value": root, "expect": ver}], "panes: break out")
    return follow(ctx)


MOVE_PICK = "panes.move-to"


@ext.command("panes.move", "Move a pane into another tab, beside its panes (args: to = <n> counting from 1, or text; "
             "none: choose the tab from a list)")
def move(args):
    ctx, key, ver, root, scope, found, i = arrange(args)
    if not scope:
        raise VqroError("this layout has no tabs to move a pane to", "invalid")
    tabs = node_at(root, ctx["tab_path"])["tabs"]
    if args.get("to") is None and args.get("text") is None:
        # From a right-click menu: the tabs as a list to click (no typing; human feedback batch 5). Each row runs
        # this command again with `to`, in the same pane's context (popups keep the context that opened them).
        here = ctx["tab_index"]
        ext.publish(MOVE_PICK, {"title": "Move pane to tab", "items": [
            {"text": f"{n + 1}  {t.get('title') or 'tab'}", "command": "panes.move", "args": {"to": n + 1, "close": True}}
            if n != here else {"text": f"{n + 1}  {t.get('title') or 'tab'}  (here)", "style": "dim", "disabled": True}
            for n, t in enumerate(tabs)]})
        return {"client": {"popup": {"view": MOVE_PICK, "width": 50}}}
    try:
        to = int(str(args.get("to", args.get("text"))).strip()) - 1
    except ValueError:
        raise VqroError("move the pane to a tab number from 1", "invalid") from None
    here = ctx["tab_index"]
    if not 0 <= to < len(tabs) or to == here:
        raise VqroError(f"pick another tab, 1 to {len(tabs)}", "invalid")
    unit = found[i]
    destination = tabs[to]
    if unit == scope:  # the pane is the whole tab: the tab goes away
        node = tabs.pop(here)["node"]
        to -= to > here
    else:
        root, node = take_out(root, unit)
        tabs = node_at(root, ctx["tab_path"])["tabs"]
    destination["node"] = {"split": "h", "children": [destination["node"], node], "sizes": [1, 1]}
    group = node_at(root, ctx["tab_path"])
    group["active"] = to
    ext.apply([{"op": "set", "key": key, "value": root, "expect": ver}], "panes: move to tab")
    reply = follow(ctx) or {"client": {}}
    if args.get("close"):
        reply["client"]["close"] = True
    return reply


RESIZE_AXIS = {"left": "h", "right": "h", "up": "v", "down": "v"}


@ext.command("panes.resize", "Move a pane's edge (args: dir = left | right | up | down, by = percent, default 5)")
def resize(args):
    """Like tmux resize-pane: the pane's right (bottom) divider moves that way, its left (top) one if it has none.

    A step is relative, so it applies to the newest layout (retried on a concurrent change) as long as the same pane
    is still at that path: repeated keys (binding `repeat`) arrive faster than the client sees each result."""
    ctx = args.get("context") or {}
    if "path" not in ctx or "layout" not in ctx:
        raise VqroError("no pane here", "invalid")
    axis = RESIZE_AXIS.get(args.get("dir"))
    if axis is None:
        raise VqroError("resize left, right, up or down", "invalid")
    by = args.get("by", 5)
    if not isinstance(by, int) or not 1 <= by <= 50:
        raise VqroError("`by` is a whole percent from 1 to 50", "invalid")
    key, path, scope = f"layout/{ctx['layout']}", ctx["path"], scope_of(ctx)

    def attempt():  # from the extension's cache; a conflict reloads it (ext.retrying)
        root = copy.deepcopy(ext.value(key))
        here = node_at(root, path) if root is not None else None
        if here is None or (ctx.get("view") and here.get("view") != ctx["view"]):
            raise VqroError("that pane moved; nothing resized", "conflict")
        for depth in range(len(path) - 1, len(scope) - 1, -1):
            split = node_at(root, path[:depth])
            if split.get("split") == axis and len(split["children"]) > 1:
                break
        else:
            raise VqroError("no divider that way", "invalid")
        n, c = len(split["children"]), path[depth]
        d = c if c + 1 < n else c - 1  # the divider between children d and d + 1
        sizes = split.get("sizes") if len(split.get("sizes") or []) == n else [1] * n
        total = sum(sizes)
        if total < 100:  # small weights such as [1, 1]: rescale so a step can be a few percent
            sizes = [max(1, round(x * 1000 / total)) for x in sizes]
            total = sum(sizes)
        step = max(1, total * by // 100)
        least = max(1, total // 20)
        grow, shrink = (d, d + 1) if args["dir"] in ("right", "down") else (d + 1, d)
        step = min(step, sizes[shrink] - least)
        if step <= 0:
            return None  # already as small as it goes
        sizes[grow] += step
        sizes[shrink] -= step
        split["sizes"] = sizes
        ext.apply([{"op": "set", "key": key, "value": root, "expect": ext.version(key)}], "panes: resize")
    return ext.retrying(attempt)


PRESETS = ["even-h", "even-v", "main-v", "main-h", "tiled"]


def even(axis, nodes):
    return nodes[0] if len(nodes) == 1 else {"split": axis, "children": nodes, "sizes": [1] * len(nodes)}


def preset_node(name, nodes):
    if name in ("even-h", "even-v"):
        return even(name[-1], nodes)
    if name in ("main-v", "main-h"):  # the first pane large, the rest beside (main-v) or below it (main-h)
        outer, inner = ("h", "v") if name == "main-v" else ("v", "h")
        return {"split": outer, "children": [nodes[0], even(inner, nodes[1:])], "sizes": [1, 1]}
    rows = math.ceil(math.sqrt(len(nodes)))
    cols = math.ceil(len(nodes) / rows)
    return even("v", [even("h", nodes[r * cols:(r + 1) * cols]) for r in range(rows) if nodes[r * cols:]])


@ext.command("panes.layout", "Arrange a tab's panes (args: preset = even-h | even-v | main-v | main-h | tiled | next)")
def layout(args):
    ctx, key, ver, root, scope, found, _ = arrange(args)
    if len(found) < 2:
        raise VqroError("only one pane here", "invalid")
    name = args.get("preset", "next")
    if name == "next":  # cycle from the preset this tab was last given (remembered on its node)
        last = node_at(root, scope).get("preset")
        name = PRESETS[(PRESETS.index(last) + 1) % len(PRESETS)] if last in PRESETS else PRESETS[0]
    if name not in PRESETS:
        raise VqroError("presets: " + ", ".join(PRESETS + ["next"]), "invalid")
    node = preset_node(name, [copy.deepcopy(node_at(root, p)) for p in found])
    node["preset"] = name
    ext.apply([{"op": "set", "key": key, "value": replace_at(root, scope, node), "expect": ver}], f"panes: layout {name}")
    ext.notify(f"layout: {name}", "ok")
    return follow(ctx)


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
    # Right-click menus hold only what the mouse can finish (human feedback batch 5): no item that asks for typing.
    # Rename stays in the command list; typing a tab number stays on the M key.
    # Clients that can zoom say whether the pane is zoomed (`zoomed` in the pane context); others show neither.
    "menu/panes.zoom": {"where": "pane", "label": "Zoom", "command": "client.zoom", "when": {"zoomed": False}, "order": 15},
    "menu/panes.unzoom": {"where": "pane", "label": "Unzoom", "command": "client.zoom", "when": {"zoomed": True}, "order": 15},
    "menu/panes.split-right": {"where": "pane", "label": "Split right (new shell)", "command": "panes.split", "args": {"dir": "right"}, "order": 20},
    "menu/panes.split-down": {"where": "pane", "label": "Split down (new shell)", "command": "panes.split", "args": {"dir": "down"}, "order": 21},
    "menu/panes.break": {"where": "pane", "label": "Move to a new tab", "command": "panes.break", "order": 30},
    "menu/panes.move": {"where": "pane", "label": "Move to tab…", "command": "panes.move", "order": 31},
    "menu/panes.show": {"where": "pane", "label": "Show another resource", "command": "panes.show", "pick": "resource", "when": {"kind": "terminal"}, "order": 35},
    "menu/panes.close": {"where": "pane", "label": "Close pane (program keeps running)", "command": "panes.close", "order": 40},
    "menu/panes.kill": {"where": "pane", "label": "Kill program", "command": "panes.kill", "when": {"live": True, "kind": "terminal"}, "order": 50},
    "binding/panes.menu": {"key": "m", "command": "client.menu", "args": {"where": "pane"}, "label": "pane menu"},
    # h/v follow the layout's split names: "h" puts panes side by side, "v" stacks them.
    "binding/panes.split-h": {"key": "h", "command": "panes.split", "args": {"dir": "right"}, "label": "split side by side"},
    "binding/panes.split-v": {"key": "v", "command": "panes.split", "args": {"dir": "down"}, "label": "split stacked"},
    "binding/panes.close": {"key": "x", "command": "panes.close", "label": "close pane (program keeps running)"},
    "binding/panes.zoom": {"key": "z", "command": "client.zoom", "label": "zoom pane (toggle)"},
    "binding/panes.scroll": {"key": "[", "command": "client.scroll", "label": "scroll back and search (terminal)"},
    "binding/panes.last": {"key": ";", "command": "client.focus", "args": {"dir": "last"}, "label": "last pane"},
    "binding/panes.swap-prev": {"key": "{", "command": "panes.swap", "args": {"dir": "prev"}, "label": "swap with previous pane"},
    "binding/panes.swap-next": {"key": "}", "command": "panes.swap", "args": {"dir": "next"}, "label": "swap with next pane"},
    "binding/panes.rotate": {"key": "C-o", "command": "panes.rotate", "args": {"dir": "next"}, "label": "rotate panes"},
    "binding/panes.break": {"key": "b", "command": "panes.break", "label": "break pane out to a tab"},
    "binding/panes.move": {"key": "M", "command": "panes.move", "label": "move pane to tab", "input": "Move pane to tab (1, 2, …)"},
    "binding/panes.layout": {"key": "Space", "command": "panes.layout", "args": {"preset": "next"}, "label": "next pane layout"},
    **{f"binding/panes.resize-{d}": {"key": f"C-{d.capitalize()}", "command": "panes.resize", "args": {"dir": d}, "hidden": d != "left", "repeat": True,
                                     "label": "resize pane (Ctrl-arrows)" if d == "left" else f"resize {d}"} for d in ("left", "right", "up", "down")},
})
ext.run()
