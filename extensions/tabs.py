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


def show(view, title, view_value=None, args=None):
    """Activate the tab showing `view`, or add one, in the invoking window's layout (`args.context`)."""
    def attempt():
        ops = []
        if view_value is not None and ext.value(f"view/{view}") != view_value:
            ops.append({"op": "set", "key": f"view/{view}", "value": view_value, "expect": ext.version(f"view/{view}")})
        tab = {"title": title, "node": {"view": view}}
        mark_ended({"tabs": [tab]})
        lay = ext.context_layout(args)  # the invoking window's layout (or the shared one)
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


@ext.command("tabs.open", "New tab with a shell (args: argv, cwd, name; cwd defaults to the layout's folder)")
def open_tab(args):
    spawn = {k: args[k] for k in ("argv", "cwd", "name") if k in args}
    if "cwd" not in spawn and ext.layout_folder(args):
        spawn["cwd"] = ext.layout_folder(args)
    r = ext.vqro.call("resource.spawn", spawn)["resource"]
    return show(f"term-{r['id']}", title_of(r), {"kind": "terminal", "resource": r["id"]}, args)


@ext.command("tabs.show", "Show a resource (args: resource, or picked) or view (args: view, title) in a tab")
def show_cmd(args):
    rid = args.get("resource") or args.get("picked")  # `picked`: from a client's resource picker
    if rid:
        ext.fresh()
        resource, result = terminal_target(ext, rid)
        if resource is None:
            return result
        rid = resource["id"]
        return show(f"term-{rid}", title_of(resource), {"kind": "terminal", "resource": rid}, args)
    if "view" in args:
        value = ext.value("view/" + args["view"]) or {}
        if value.get("kind") == "terminal":
            return show_cmd({"resource": value["resource"]})
        return show(args["view"], args.get("title") or args["view"], None, args)
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


class Shell:
    """At most one new shell per command, spawned only when a close would leave no tab.

    The spawn is a backend effect (never repeated by CAS retries); the layout change that shows it
    is one transaction with the close. If that transaction finally fails, the shell keeps running
    unshown and the user is told where it is.
    """
    def __init__(self):
        self.resource = None

    def tab(self, cwd=None):
        """A tab showing the new shell, started in `cwd` (the layout's folder) when it is spawned here."""
        if self.resource is None:
            self.resource = ext.vqro.call("resource.spawn", {"cwd": cwd} if cwd else {})["resource"]
        return {"title": title_of(self.resource), "node": {"view": self.view()}}

    def view(self):
        return None if self.resource is None else f"term-{self.resource['id']}"

    def ops(self, layouts):
        """Declare the new shell's view if a resulting layout shows it (a retry may no longer need it)."""
        if self.resource is None or not any(self.view() in views_in(root) for root in layouts):
            return []
        key = f"view/{self.view()}"
        return [{"op": "set", "key": key, "value": {"kind": "terminal", "resource": self.resource["id"]}, "expect": ext.version(key)}]

    def orphaned(self, error):
        if self.resource is not None:
            ext.notify(f"New shell {self.resource['id']} started but not shown ({error.kind}); open it from the sidebar", "warn")


@ext.command("tabs.close", "Close a tab (its programs keep running; closing the last tab opens a new shell)")
def close_tab(args):
    key, ver, root, tabs, i = tab_target(args)
    if not 0 <= i < len(tabs["tabs"]):
        raise VqroError("that tab is gone", "conflict")
    shell = Shell()
    removed = views_in(tabs["tabs"].pop(i)["node"])
    if not tabs["tabs"]:
        if ver is not None and ext.version(key) != ver:
            raise VqroError("the layout changed meanwhile; nothing closed, try again", "conflict")  # before spawning
        tabs["tabs"], tabs["active"] = [shell.tab(ext.layout_folder(args))], 0  # replace the last tab, as `tabs.open` would
    else:
        a = tabs.get("active", 0)
        tabs["active"] = min(a - 1 if a >= i and a > 0 else a, len(tabs["tabs"]) - 1)
    ops = shell.ops([root]) + [{"op": "set", "key": key, "value": root, "expect": ver}]
    ops += orphan_view_deletes(removed, {key: root})
    try:
        result = ext.apply(ops, "tabs: close")
    except VqroError as error:
        shell.orphaned(error)
        raise
    if shell.resource is not None:
        return dict(result or {}, client={"focus": {"view": f"term-{shell.resource['id']}"}})
    return result


@ext.command("tabs.rename", "Rename a tab (args: text)")
def rename_tab(args):
    key, ver, root, tabs, i = tab_target(args)
    text = (args.get("text") or "").strip()
    if not text:
        raise VqroError("empty name", "invalid")
    tabs["tabs"][i]["title"] = text
    ext.apply([{"op": "set", "key": key, "value": root, "expect": ver}], "tabs: rename")


@ext.command("tabs.move", "Move a tab (args: to = left | right | <n> counting from 1; a tab drag gives its drop place)")
def move_tab(args):
    key, ver, root, tabs, i = tab_target(args)
    group = tabs["tabs"]
    if not 0 <= i < len(group):
        raise VqroError("that tab is gone", "conflict")
    ctx = args.get("context") or {}
    to = args.get("to", args.get("text"))
    if to is None and isinstance(ctx.get("drop_index"), int):  # the generic mouse.drag gesture (core-protocol.md)
        to = ctx["drop_index"] + 1
    if to in ("left", "right"):
        j = i - 1 if to == "left" else i + 1
        if not 0 <= j < len(group):
            return None  # already at that end: nothing to do
    else:
        try:
            j = int(str(to).strip()) - 1
        except ValueError:
            raise VqroError("move the tab left, right or to a number from 1", "invalid") from None
        if not 0 <= j < len(group):
            raise VqroError(f"there are {len(group)} tabs; pick 1 to {len(group)}", "invalid")
    if j == i:
        return None
    active = group[min(tabs.get("active", 0), len(group) - 1)]
    group.insert(j, group.pop(i))
    tabs["active"] = next(n for n, tab in enumerate(group) if tab is active)  # the shown tab stays shown
    ext.apply([{"op": "set", "key": key, "value": root, "expect": ver}], "tabs: move")


def prune_ended(node, shell, cwd=None):
    """Close wholly ended tabs, preserving mixed tabs. A group left with no tab gets a new shell tab
    (started in `cwd`, the layout's folder)."""
    for child in node.get("children", []):
        prune_ended(child, shell, cwd)
    tabs = node.get("tabs", [])
    for tab in tabs:
        prune_ended(tab["node"], shell, cwd)
    if not tabs:
        return
    active = tabs[min(node.get("active", 0), len(tabs)-1)]
    def dead(tab):
        resources = [view_resource(ext, view) for view in views_in(tab["node"])]
        return bool(resources) and all(rid and ended(ext.resources.get(rid)) for rid in resources)
    kept = [tab for tab in tabs if not dead(tab)] or [shell.tab(cwd)]
    node["tabs"] = kept
    node["active"] = next((i for i, tab in enumerate(kept) if tab is active), 0)


@ext.command("tabs.close_ended", "Close ended/gone resource tabs (programs are never killed; a closed last tab becomes a new shell)")
def close_ended(args):
    shell = Shell()  # shared by retries and layouts: one new shell at most, reused if it is needed again
    def attempt():
        ext.fresh()
        ops, removed, layouts = [], set(), {}
        for key, entry in list(ext.entries.items()):
            if key.startswith("layout/"):
                root = copy.deepcopy(entry["value"])
                prune_ended(root, shell, ext.layout_settings(key[len("layout/"):]).get("cwd"))
                if root != entry["value"]:
                    ops.append({"op": "set", "key": key, "value": root, "expect": entry["version"]})
                    removed.update(set(views_in(entry["value"]))-set(views_in(root)))
                    layouts[key] = root
        ops = shell.ops(layouts.values()) + ops + orphan_view_deletes(removed, layouts)
        result = ext.apply(ops, "tabs: close ended") if ops else {}
        if shell.resource is not None and not any(shell.view() in views_in(root) for root in layouts.values()):
            shell.orphaned(VqroError("no longer needed after a concurrent change", "conflict"))
        return result
    try:
        return ext.retrying(attempt)
    except VqroError as error:
        shell.orphaned(error)
        raise


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


# Tab bar look: the shared setting doc/settings/tabs (core-protocol.md "Tab bar"); every client reads it with a
# fallback. Missing means subscript numbers after the name, in chips.
TAB_SETTING = "doc/settings/tabs"
NUMBERS = [("subscript", "Subscript ₁"), ("superscript", "Superscript ¹"), ("plain", "Plain 1"), ("none", "None")]
POSITIONS = [("after", "After the name"), ("before", "Before the name")]


def settings_panel():
    current = ext.value(TAB_SETTING) if isinstance(ext.value(TAB_SETTING), dict) else {}
    numbers = current.get("numbers") if current.get("numbers") in dict(NUMBERS) else "subscript"
    position = current.get("position") if current.get("position") in dict(POSITIONS) else "after"
    chips = current.get("chips") if isinstance(current.get("chips"), bool) else True
    program = current.get("program_titles") if isinstance(current.get("program_titles"), bool) else True
    return {"title": "Tabs", "requires": ["select"], "items": [
        {"type": "select", "text": "Number style", "value": numbers, "description": "the small number before each tab's name",
         "options": [{"label": label, "value": value} for value, label in NUMBERS], "command": "tabs.set", "args": {"field": "numbers"}},
        {"type": "select", "text": "Number position", "value": position, "description": "Liaison₁ (after) or ₁ Liaison (before)",
         "options": [{"label": label, "value": value} for value, label in POSITIONS], "command": "tabs.set", "args": {"field": "position"}},
        {"type": "select", "text": "Chips", "value": chips, "description": "padded tabs with a gap (off: the flat 1:name bar)",
         "options": [{"label": "On", "value": True}, {"label": "Off", "value": False}], "command": "tabs.set", "args": {"field": "chips"}},
        {"type": "select", "text": "Program titles", "value": program,
         "description": "a tab still named after its command (zsh) shows the program's own title; renamed tabs keep their names",
         "options": [{"label": "On", "value": True}, {"label": "Off", "value": False}], "command": "tabs.set", "args": {"field": "program_titles"}}]}


@ext.command("tabs.settings", "Tab bar settings (number style and position, chips, program titles)")
def tab_settings(args):
    ext.publish("tabs-settings", settings_panel())
    return {"client": {"popup": {"view": "tabs-settings", "width": 64}}}


@ext.command("tabs.set", "Set a tab bar option (args: field numbers|position|chips|program_titles, value)")
def tab_set(args):
    field, value = args.get("field"), args.get("value")
    if not ((field == "numbers" and value in dict(NUMBERS)) or (field == "position" and value in dict(POSITIONS))
            or (field in ("chips", "program_titles") and isinstance(value, bool))):
        raise VqroError("choose numbers subscript|superscript|plain|none, position after|before, chips or program_titles true|false", "invalid")
    def attempt():
        current = ext.value(TAB_SETTING)
        value_now = dict(current if isinstance(current, dict) else {}, **{field: value})
        ext.apply([{"op": "set", "key": TAB_SETTING, "value": value_now, "expect": ext.version(TAB_SETTING)}], "tabs: bar " + field)
    ext.retrying(attempt)
    ext.publish("tabs-settings", settings_panel())


ext.contribute({
    "view/tabs-settings": {"kind": "panel", "title": "Tabs", "items": []},
    # An icon alone: the label stays empty so a set's `add` glyph replaces the `+`.
    "menu/tabs.new": {"where": "tabbar", "label": "", "icon": {"name": "add", "text": "+"}, "command": "tabs.open", "order": 0},
    "menu/tabs.rename": {"where": "tab", "label": "Rename tab", "command": "tabs.rename", "input": "Tab name", "prefill": {"context": "tab_title"}, "order": 10},
    "menu/tabs.close": {"where": "tab", "label": "Close tab", "command": "tabs.close", "order": 20},
    # The tab bar keeps only "+"; other actions live in menus and the command list.
    "menu/tabs.close-ended": {"where": "tab", "label": "Close ended tabs", "command": "tabs.close_ended", "order": 30},
    "menu/tabs.move-left": {"where": "tab", "label": "Move tab left", "command": "tabs.move", "args": {"to": "left"}, "order": 15},
    "menu/tabs.move-right": {"where": "tab", "label": "Move tab right", "command": "tabs.move", "args": {"to": "right"}, "order": 16},
    "binding/tabs.new": {"key": "n", "command": "tabs.open", "label": "new shell in a tab"},
    "binding/tabs.rename": {"key": "r", "command": "tabs.rename", "label": "rename tab", "input": "Tab name", "prefill": {"context": "tab_title"}},
    "binding/tabs.move-left": {"key": "<", "command": "tabs.move", "args": {"to": "left"}, "label": "move tab left"},
    "binding/tabs.move-right": {"key": ">", "command": "tabs.move", "args": {"to": "right"}, "label": "move tab right"},
    "binding/tabs.move-to": {"key": ".", "command": "tabs.move", "label": "move tab to number", "input": "Move tab to (1, 2, …)"},
})
resource_state("missed", {})
ext.run()
