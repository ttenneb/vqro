#!/usr/bin/env python3
"""vqro text client: a plain, line-based client of the `base` profile (D29), stdlib only.

It renders the workspace as text: an outline of the active layout, panels as numbered rows, menus, the command
palette and the shared recovery content (prefix menu, safe-mode chooser). It is meant for scripts, agents and
screen readers, and it is the second, independent implementation of the client contract (the model is
`vqro_client_model.py`, checked against `tests/fixtures/client/*.json`). It does not show terminal contents (no
`terminal` profile) and never answers terminal queries.

    vqro ui text                      interactive: type `help`
    vqro ui text -- --once 'show; p'  run commands, print, exit
"""
import json
import os
import sys
import threading

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(1, os.path.dirname(HERE))  # vqro_ext.py beside the bundled extensions
import vqro_client_model as model  # noqa: E402
from vqro_ext import Vqro, VqroError  # noqa: E402

FEATURES = model.BASE_FEATURES + ["icon_names", "current_when", "markdown", "pin", "composer", "multiline", "facets", "sort", "columns"]
WIDTH = 100  # formatted text is laid out at this width
HELP = """commands (numbers refer to the last listing):
  show                    this client's layout (workspace): tabs and numbered panes (* = focused)
  focus <pane>            focus a pane; `view` then shows it
  view [<pane>|<view id>] a pane's content: panel rows numbered; terminals are named only
  do <row>[.<chip>] [<value or text>]   activate a row (or chip) of the shown panel; on a composer, send the text
                          (`\\n` is a new line in composers and multi-line prompts)
  set <row> <arg> <value> choose a composer control's value (kept here until sent)
  page <n> | filter <text> | expand <row> | collapse <row>     panel display state
  facet <key> <value|all> | sort [<n>|given]   the shown panel's Show choices and sort (where it offers them)
  menu <pane|tab|tabbar|status> [<n>]  contributed menu items for the focused pane, or run one
  p [<n>]                 the prefix menu: key bindings and recovery (always available); run row n
  commands [<filter>]     the command palette; run <name> [<json args>]
  safe [toggle <name> | apply | leave]  the safe-mode recovery chooser
  help | quit"""


def text_of(value):
    return value if isinstance(value, str) else ""


class TextClient:
    def __init__(self, socket_path, out=sys.stdout):
        self.out = out
        self.lock = threading.RLock()
        self.ws = model.Workspace()
        self.window = model.Window()  # the layout (workspace) this client shows: its own, never shared state
        self.focus = 0
        self.focus_last = None  # the pane focused before (client.focus {dir: last})
        self.shown = None          # the view whose rows were listed last (a pane's view or a popup)
        self.popup = None          # a view a command asked to show (reply hint), until closed
        self.state = {}            # per view: {page, filter, open}
        self.listed = []           # the last numbered rows of `self.shown`
        self.menu = []             # the last listed menu/prefix rows: (label, binding)
        self.safe_selected = set()
        self.composers = {}           # (view, composer row id) -> client-local control values
        self.vqro = Vqro(socket_path, self.on_message, recover_notifications=True)
        self.info = self.vqro.call("hello", model_hello())
        self.resync()

    # ---- protocol -------------------------------------------------------------------------------------------

    def resync(self):
        snap = self.vqro.call("workspace.snapshot")
        with self.lock:
            was = self.ws.load(snap)
            self.safe_mode_changed(self.ws.safe_mode, was)
            self.settle_window()
        try:
            self.vqro.call("events.subscribe", {"from": snap["seq"], "epoch": snap["epoch"], "partial_resources": True})
        except VqroError as e:
            if e.kind != "conflict":  # already subscribed on this connection
                raise

    def on_message(self, method, params):
        if method == "missed":
            self.resync()
            return None
        if method != "event":
            return None
        with self.lock:
            changes = self.ws.apply(params)
        for change, data in changes:
            if change == "resync":
                self.resync()
            elif change == "notify":
                self.say(("! " if data.get("style") in ("warn", "error") else "* ") + text_of(data.get("text")))
            elif change == "failed":
                self.say(f"! extension {data} failed (p lists its restart)")
            elif change == "safe_mode":
                self.safe_mode_changed(*data)
            elif change == "entries":
                with self.lock:
                    self.settle_window()
        return None

    def settle_window(self):
        settled = self.window.settle(self.ws)
        if isinstance(settled, dict) and "Closed" in settled:
            closed = settled["Closed"]
            self.say(f"* workspace {closed['was']} closed; showing {closed['now']}")

    def safe_mode_changed(self, on, was):
        if on and not was:
            self.safe_selected = set(self.ws.safe_mode_allow)
            self.say("! safe mode: nothing starts automatically. `safe` shows the recovery choices.")
        elif was and not on:
            self.say("* safe mode left")

    def say(self, line):
        with self.lock:
            print(line, file=self.out, flush=True)

    # ---- layout ---------------------------------------------------------------------------------------------

    def active_layout(self):
        """This client's layout: its own choice, starting from the shared default (vqro_client_model.Window)."""
        name = self.window.shown(self.ws)
        return name, self.ws.entries.get(f"layout/{name}") if name else None

    def leaves(self):
        """(view id, path, depth, tab info) for each pane of the active layout, in order; active tabs only."""
        out = []

        def walk(node, path, depth, tab):
            if not isinstance(node, dict):
                return
            if isinstance(node.get("view"), str):
                out.append((node["view"], list(path), depth, tab))
            elif isinstance(node.get("children"), list):
                for i, child in enumerate(node["children"]):
                    walk(child, path + [i], depth + 1, tab)
            elif isinstance(node.get("tabs"), list) and node["tabs"]:
                active = min(node.get("active", 0) if isinstance(node.get("active"), int) else 0, len(node["tabs"]) - 1)
                walk((node["tabs"][active] or {}).get("node"), path + [active], depth + 1, (list(path), active, node["tabs"]))
        walk(self.active_layout()[1], [], 0, None)
        return out

    def view_title(self, view):
        v = self.ws.entries.get(f"view/{view}") or {}
        if isinstance(v.get("title"), str):
            return v["title"]
        r = self.ws.resources.get(v.get("resource")) or {}
        return text_of(r.get("name")) or text_of((r.get("argv") or [""])[0]) or view

    def tab_resource(self, tab):
        """The resource of a tab's only view (program titles), or None."""
        node = (tab or {}).get("node") if isinstance(tab, dict) else None
        view = node.get("view") if isinstance(node, dict) else None
        v = self.ws.entries.get(f"view/{view}") if isinstance(view, str) else None
        return self.ws.resources.get(v.get("resource")) if isinstance(v, dict) else None

    def ctx(self, index=None):
        """`args.context` for a pane, as every client sends it."""
        leaves = self.leaves()
        index = self.focus if index is None else index
        if not leaves:
            return {}
        view, path, _, tab = leaves[min(index, len(leaves) - 1)]
        name, _ = self.active_layout()
        v = self.ws.entries.get(f"view/{view}") or {}
        resource = v.get("resource") if isinstance(v.get("resource"), str) else None
        r = self.ws.resources.get(resource) or {}
        ctx = {"layout": name, "layout_version": self.ws.versions.get(f"layout/{name}"), "path": path, "view": view,
               "kind": v.get("kind"), "resource": resource, "resource_kind": r.get("kind"), "live": r.get("state") == "live",
               "title": self.view_title(view), "tab_title": None}
        if tab:
            tab_path, tab_index, tabs = tab
            ctx.update(tab_path=tab_path, tab_index=tab_index, tab_title=(tabs[tab_index] or {}).get("title"))
        if self.shown_of(view) != view:
            ctx["shown_view"] = self.shown_of(view)
        return ctx

    def show(self):
        name, root = self.active_layout()
        if root is None:
            return ["no active layout (try `p` and reset layout)"]
        lines = [f"layout {name}"]

        def tabs_line(node, depth):
            tabs = node["tabs"]
            active = node.get("active", 0) if isinstance(node.get("active"), int) else 0
            setting = self.ws.entries.get(model.TABS_ENTRY)
            style = model.tab_style(setting or {})
            name = lambda t: model.tab_title(setting, text_of((t or {}).get("title")) or "tab", self.tab_resource(t))
            return "  " * depth + "tabs: " + "  ".join(f"[{model.tab_label(style, i, name(t)).strip()}{'*' if i == active else ''}]"
                                                      for i, t in enumerate(tabs))

        def walk(node, depth):
            if not isinstance(node, dict):
                return
            if isinstance(node.get("view"), str):
                n = len(numbered)
                numbered.append(node["view"])
                mark = "*" if n == self.focus else " "
                lines.append("  " * depth + f"{mark}pane {n + 1}: {self.view_title(node['view'])} ({self.kind_of(node['view'])})")
            elif isinstance(node.get("children"), list):
                lines.append("  " * depth + ("split side by side" if node.get("split") != "v" else "split stacked"))
                for child in node["children"]:
                    walk(child, depth + 1)
            elif isinstance(node.get("tabs"), list) and node["tabs"]:
                lines.append(tabs_line(node, depth))
                active = min(node.get("active", 0) if isinstance(node.get("active"), int) else 0, len(node["tabs"]) - 1)
                walk((node["tabs"][active] or {}).get("node"), depth + 1)
        numbered = []
        walk(root, 1)
        if self.ws.safe_mode:
            lines.append("! safe mode is on: `safe` shows the recovery choices")
        return lines

    def shown_of(self, view):
        """The view drawn for `view`: itself, or its fallback when this client can't render it (D29)."""
        if view == model.SAFE_MODE_VIEW:
            return view
        shown, _ = model.shown_view(view, lambda v: model.merged(v, self.ws.entries, self.ws.live),
                                    lambda v: model.renders(v, ["panel", "terminal"], FEATURES))
        return shown or view

    def kind_of(self, view):
        if view == model.SAFE_MODE_VIEW:
            return "panel"
        return text_of((self.ws.entries.get(f"view/{self.shown_of(view)}") or {}).get("kind")) or "missing view"

    # ---- panels ---------------------------------------------------------------------------------------------

    def panel_value(self, view):
        if view == model.SAFE_MODE_VIEW:
            return model.safe_chooser(self.ws, self.safe_selected)
        view = self.shown_of(view)
        value = dict(self.ws.entries.get(f"view/{view}") or {})
        live = self.ws.live.get(view)
        if isinstance(live, dict):
            value.update(live)
        return model.mark_current(value, self.window.shown(self.ws))

    def icon(self, icon):
        """A row's icon as text: the active icon set's glyph for its names (feature icon_names), else its own text."""
        if isinstance(icon, str):
            return icon
        if not isinstance(icon, dict) or self.ws.safe_mode:
            return text_of((icon or {}).get("text")) if isinstance(icon, dict) else ""
        glyphs = (self.ws.entries.get("doc/settings/icons") or {}).get("glyphs") or {}
        names = icon.get("name") if isinstance(icon.get("name"), list) else [icon.get("name")]
        for n in names:
            g = glyphs.get(n) if isinstance(n, str) else None
            if isinstance(g, str) and g and g.isprintable() and len(g) <= 2:
                return g
        return text_of(icon.get("text"))

    def view_lines(self, view):
        kind = self.kind_of(view)
        self.shown, self.listed = view, []
        title = self.view_title(view) if view != model.SAFE_MODE_VIEW else "Safe mode recovery"
        if kind == "terminal":
            r = self.ws.resources.get((self.ws.entries.get(f"view/{self.shown_of(view)}") or {}).get("resource")) or {}
            return [f"{title}: terminal {text_of(r.get('id'))} ({text_of(r.get('state')) or 'unknown'}); this client does not show terminal contents"]
        if kind != "panel":
            return [f"{title}: view kind `{kind}` is not supported by this client"]
        value = self.panel_value(view)
        st = self.state.setdefault(view, {"page": 0, "filter": "", "open": {}})
        st.setdefault("facets", {})
        st.setdefault("sort", None)
        lines = [f"== {text_of(value.get('title')) or title}"]
        missing = model.missing_for(value, FEATURES)
        if missing:
            return lines + [f"(missing: {', '.join(missing)}) this client cannot show this panel"]
        pages = value.get("pages") if isinstance(value.get("pages"), list) else []
        if pages:
            lines.append("pages: " + "  ".join(f"[{i + 1}:{text_of(p.get('title'))}{'*' if i == min(st['page'], len(pages) - 1) else ''}]" for i, p in enumerate(pages)))
        if st["filter"]:
            lines.append(f"filter: {st['filter']}")
        for c in model.facet_counts(value, st["page"], st["filter"], st["facets"], FEATURES):
            chips = [("all", "All", c["all"], c["chosen"] is None)] + [(v["value"], v["label"], v["count"], c["chosen"] == v["value"]) for v in c["values"]]
            lines.append(f"{text_of(c['label'])} ({c['key']}): " + "  ".join(f"[{l} {n}]" + ("*" if on else "") for _, l, n, on in chips)
                         + f"   (facet {c['key']} <value|all>)")
        sorts = model.declared(value, st["page"], "sorts")
        if sorts:
            options = ["given"] + [text_of(s.get("label") or s.get("key")) for s in sorts if isinstance(s, dict)]
            chosen = 0 if st["sort"] is None else st["sort"] + 1
            lines.append("sort: " + "  ".join(f"[{i}:{o}]" + ("*" if i == chosen else "") for i, o in enumerate(options)) + "   (sort <n>)")
        shown_rows = model.rows(value, st["page"], st["filter"], st["open"], FEATURES, st["facets"], st["sort"])
        spec = model.declared(value, st["page"], "columns")
        self.widths = model.column_widths(spec, shown_rows, WIDTH - 20) if spec else None
        self.aligns = [isinstance(c, dict) and c.get("align") == "right" for c in spec or []]
        if self.widths and any(isinstance(c, dict) and c.get("label") for c in spec):
            lines.append("       " + self.cells([text_of(c.get("label")) if isinstance(c, dict) else "" for c in spec]))
        bottom = False
        for r in shown_rows:
            if r.get("bottom") and not bottom:  # pin: "bottom" rows, below the list
                lines.append("    ===")
                bottom = True
            # Only rows you can act on get a number.
            if model.selectable(r["item"]):
                self.listed.append(r)
            lines.append(self.row_line(len(self.listed), r))
        return lines

    def cells(self, cells):
        """Cells padded (or cut with …) to this panel's column widths, one space apart (feature columns)."""
        out = []
        for i, w in enumerate(self.widths or []):
            c = cells[i] if i < len(cells) else ""
            c = c if len(c) <= w else c[:max(w - 1, 0)] + "…"
            out.append(c.rjust(w) if i < len(self.aligns) and self.aligns[i] else c.ljust(w))
        return " ".join(out).rstrip()

    def row_line(self, n, r):
        item, pad = r["item"], "  " * r["depth"]
        if getattr(self, "widths", None) and isinstance(item.get("columns"), list):
            item = dict(item, text=self.cells([text_of(c) for c in item["columns"]]))
        k = model.kind(item)
        if k == "separator":
            return f"{pad}    ---"
        if k == "composer":
            values = self.composer(r)["values"] if self.shown else {}
            controls = "  ".join(f"{c['arg']}={values.get(c['arg'])}" for c in model.composer_controls(item))
            return (f"{pad}{n:>3}. [composer] {text_of(item.get('placeholder')) or 'Message'}" + (f"  ({controls})" if controls else "")
                    + "  (do <row> <text> sends; set <row> <arg> <value>)")
        if k == "text" and item.get("format") == "markdown":  # formatted text: the shared layout, as plain lines
            shown = ["".join(t for t, _ in line) for line in model.markdown_lines(text_of(item.get("text")), WIDTH - 5 - len(pad))]
            first = f"{pad}{n:>3}. " if model.selectable(item) else f"{pad}     "
            return "\n".join((first if i == 0 else pad + "     ") + l for i, l in enumerate(shown))
        icon = self.icon(item.get("icon"))
        text = (icon + " " if icon else "") + text_of(item.get("text"))
        if k == "header":
            return f"{pad}    ## {text}"
        if k == "chips":
            chips = "  ".join(f"[{i + 1}:{'x ' if c.get('checked') else ''}{self.icon(c.get('icon'))}{text_of(c.get('text'))}]" for i, c in enumerate(item.get("chips") or []))
            return f"{pad}{n:>3}. {chips}"
        if k == "toggle":
            text = ("[x] " if item.get("checked") else "[ ] ") + text
        if k == "select":
            text += f": {item.get('value')}"
        if r["has_children"]:
            text = ("- " if r["expanded"] else "+ ") + text
        extra = [text_of(item.get("detail"))] + [text_of(b.get("text")) for b in item.get("badges") or [] if isinstance(b, dict)]
        line = f"{pad}{n:>3}. {text}" if model.selectable(item) else f"{pad}     {text}"
        line += "".join(f"  [{e}]" for e in extra if e)
        if isinstance(item.get("options"), list):
            line += "  (options: " + "; ".join(f"{i + 1}) " + (text_of(o.get("label")) or str(o.get("value"))) for i, o in enumerate(item["options"]) if isinstance(o, dict)) + ")"
        if item.get("input"):
            line += f"  (asks: {text_of(item['input'])})"
        if item.get("disabled"):
            line += "  (disabled)"
        if text_of(item.get("description")):
            line += "\n" + pad + "       " + item["description"]
        return line

    # ---- running things -------------------------------------------------------------------------------------

    def invoke(self, name, args, ctx):
        """Run a command: client.* here, others through the core with `args.context`; then follow reply hints."""
        if name.startswith("client."):
            return self.client_command(name, args or {}, ctx)
        args = dict(args) if isinstance(args, dict) else {}
        args["context"] = self.window.invocation_context(self.ws, ctx)
        try:
            reply = self.vqro.call("command.invoke", {"name": name, "args": args})
        except VqroError as e:
            return [f"{name}: {e} ({e.kind})"]
        out = [f"{name}: done"]
        layout = model.reply_layout(name, reply)
        if layout:
            ok, changed = self.window.show(self.ws, layout)
            if ok and changed:
                self.focus = 0
                out.append(f"showing layout {layout}")
        hints = (reply or {}).get("client") if isinstance(reply, dict) else None
        if isinstance(hints, dict):
            if hints.get("close"):
                self.popup = None
            popup = (hints.get("popup") or {}).get("view") if isinstance(hints.get("popup"), dict) else None
            if isinstance(popup, str):
                if self.kind_of(popup) == "terminal":
                    out.append("popup refused: terminals never pop up")
                else:
                    self.popup = popup
                    out += self.view_lines(popup)
            focus = (hints.get("focus") or {}).get("view") if isinstance(hints.get("focus"), dict) else None
            if isinstance(focus, str):
                for i, leaf in enumerate(self.leaves()):
                    if leaf[0] == focus:
                        self.focus = i
        return out

    def client_command(self, name, args, ctx):
        if name == "client.quit":
            raise SystemExit(0)
        if name == "client.palette":
            return self.commands("")
        if name in ("client.prefix", "client.menu"):
            return self.prefix() if name == "client.prefix" else self.menu_lines(args.get("where", "pane"))
        if name == "client.focus":
            leaves = [leaf[0] for leaf in self.leaves()]
            here = leaves[self.focus] if self.focus < len(leaves) else None
            if args.get("dir") == "last":
                if self.focus_last not in leaves:
                    return ["no other pane was focused here"]
                self.focus = leaves.index(self.focus_last)
            else:
                n = len(leaves) or 1
                self.focus = (self.focus + (1 if args.get("dir") != "prev" else -1)) % n
            self.focus_last = here
            return self.show()
        if name == "client.safe_mode":
            if self.ws.safe_mode:
                return self.safe([])
            return self.invoke("vqro.safe_mode", {"on": True}, ctx)
        if name == "client.safe_mode.toggle":
            self.safe_selected ^= {args.get("name")}
            return self.view_lines(model.SAFE_MODE_VIEW)
        if name == "client.safe_mode.apply":
            names = [k[len("extension/"):] for k in self.ws.entries if k.startswith("extension/")]
            for n in names:
                try:
                    self.vqro.call("command.invoke", {"name": "vqro.safe_mode.allow", "args": {"name": n, "on": n in self.safe_selected}})
                except VqroError as e:
                    return [f"vqro.safe_mode.allow {n}: {e}"]
            return ["Safe-mode choices applied; enabled declarations were not changed"]
        if name == "client.safe_mode.leave":
            return self.invoke("vqro.safe_mode", {"on": False}, ctx)
        if name == "client.layout":
            ok, result = self.window.client_layout(self.ws, args)
            if not ok:
                return [result]
            self.focus = 0
            return self.show()
        if name == "client.none":
            return []
        return [f"{name}: not supported by this client"]

    def activate(self, spec, rest):
        if not self.listed:
            return ["nothing listed: `view` a panel first"]
        row, _, chip = spec.partition(".")
        try:
            r = self.listed[int(row) - 1]
        except (ValueError, IndexError):
            return [f"no row {spec}"]
        item = r["item"]
        if chip:
            try:
                item = (item.get("chips") or [])[int(chip) - 1]
            except (ValueError, IndexError):
                return [f"no chip {spec}"]
        elif not model.selectable(item):
            return [f"row {row} is not selectable"]
        if model.kind(item) == "composer":
            return self.compose(r, rest)
        if not isinstance(item.get("command"), str):
            if r["has_children"] and not chip:
                self.state[self.shown]["open"][r["id"]] = not r["expanded"]
                return self.view_lines(self.shown)
            return [f"row {spec} does nothing"]
        if item.get("disabled"):
            return [f"row {spec} is disabled"]
        args = dict(item.get("args") or {}) if isinstance(item.get("args"), dict) else {}
        options = item.get("options") if isinstance(item.get("options"), list) else None
        if options is not None:
            # By number, label or value.
            option = options[int(rest) - 1] if rest.isdigit() and 0 < int(rest) <= len(options) else \
                next((o for o in options if isinstance(o, dict) and rest and (rest == text_of(o.get("label")) or rest == str(o.get("value")))), None)
            if not isinstance(option, dict):
                return ["choose one (number, label or value): " + "; ".join(f"{i + 1}) " + (text_of(o.get("label")) or str(o.get("value"))) for i, o in enumerate(options) if isinstance(o, dict))]
            args["value"] = option.get("value")
            if option.get("input"):
                return [f"this option asks for text: use `run {item['command']} {json.dumps(dict(args, text='...'))}`"]
        elif item.get("input"):
            if not rest:
                return [f"asks: {item['input']} (give it after the row number)"]
            args["text"] = rest.replace("\\n", "\n") if item.get("multiline") is True else rest
        elif item.get("pick") == "resource":
            if rest not in self.ws.resources:
                return ["pick a resource id: " + ", ".join(sorted(self.ws.resources))]
            args["picked"] = rest
        ctx = self.ctx()
        if self.shown and self.shown == self.popup:
            ctx = dict(ctx, panel=self.shown, panel_view=self.shown)
        return self.invoke(item["command"], args, ctx)

    def composer(self, r):
        """A composer row's client-local state (draft, control values), kept per view and row id until sent."""
        key = (self.shown, r["id"])
        state = self.composers.get(key)
        if state is None:
            c = model.Composer(r["item"])
            state = self.composers[key] = {"values": c.values}
        return state

    def compose(self, r, rest):
        """Send a composer row's text (with its controls' current values); nothing is sent without text."""
        item = r["item"]
        if not rest.strip():
            return [f"composer: give the text after the row number (`\\n` for a new line); `set` chooses {', '.join(c['arg'] for c in model.composer_controls(item)) or 'controls'}"]
        c = model.Composer(item)
        c.values.update(self.composer(r)["values"])
        if c.editor.insert(rest.replace("\\n", "\n")) == "refused":
            return ["too long: over the composer's size limit; nothing sent"]
        if not isinstance(item.get("command"), str):
            return ["this composer has no command"]
        ctx = self.ctx()
        return self.invoke(item["command"], c.submit_args(item), ctx)

    def menu_lines(self, place, run=None):
        ctx = self.ctx()
        entries = self.ws.menu_entries(place, ctx)
        self.menu = [(text_of(e.get("label")) or text_of((e.get("icon") or {}).get("text") if isinstance(e.get("icon"), dict) else e.get("icon")), e) for e in entries]
        if run is not None:
            return self.run_listed(run, ctx)
        if not self.menu:
            return [f"no {place} menu items (are their extensions on?)"]
        return [f"{i + 1:>3}. {label}" for i, (label, _) in enumerate(self.menu)]

    def prefix(self, run=None):
        ctx = self.ctx()
        rows = model.prefix_rows(self.ws, ctx)
        self.menu = [(label, b) for _, label, b in rows]
        if run is not None:
            return self.run_listed(run, ctx)
        lines = ["prefix menu (keys are for keyboard clients; run a row with `p <n>`):"]
        lines += [f"{i + 1:>3}. {key or '-':<8} {label}" for i, (key, label, _) in enumerate(rows)]
        return lines

    def run_listed(self, spec, ctx):
        parts = spec.split(None, 1)
        try:
            label, b = self.menu[int(parts[0]) - 1]
        except (ValueError, IndexError):
            return [f"no row {spec}"]
        args = dict(b.get("args") or {}) if isinstance(b.get("args"), dict) else {}
        if b.get("input"):
            if len(parts) < 2:
                return [f"asks: {b['input']} (give it after the row number)"]
            args["text"] = parts[1]
        if b.get("pick") == "resource":
            if len(parts) < 2 or parts[1] not in self.ws.resources:
                return ["pick a resource id: " + ", ".join(sorted(self.ws.resources))]
            args["picked"] = parts[1]
        return self.invoke(b.get("command") or "", args, ctx)

    def commands(self, query):
        q = query.lower()
        rows = sorted((n, text_of(c.get("title"))) for n, c in self.ws.commands.items() if c.get("available") is True and (q in n.lower() or q in text_of(c.get("title")).lower()))
        return [f"  {n}  {t}" for n, t in rows] or ["no commands match"]

    def safe(self, args):
        if args[:1] == ["toggle"] and len(args) > 1:
            return self.client_command("client.safe_mode.toggle", {"name": args[1]}, {})
        if args[:1] in (["apply"], ["leave"]):
            return self.client_command("client.safe_mode." + args[0], {}, {})
        if not self.ws.safe_mode:
            return ["safe mode is off (`p` lists `safe mode / recovery choices`)"]
        self.popup = model.SAFE_MODE_VIEW
        return self.view_lines(model.SAFE_MODE_VIEW)

    # ---- the command line -----------------------------------------------------------------------------------

    def handle(self, line):
        words = line.strip().split(None, 1)
        if not words:
            return []
        cmd, rest = words[0], (words[1] if len(words) > 1 else "")
        with self.lock:
            if cmd in ("help", "?"):
                return HELP.split("\n")
            if cmd in ("quit", "exit", "q"):
                raise SystemExit(0)
            if cmd in ("show", "s"):
                return self.show()
            if cmd == "focus":
                n = len(self.leaves())
                if not rest.isdigit() or not 1 <= int(rest) <= n:
                    return [f"focus 1-{n}"]
                self.focus, self.popup = int(rest) - 1, None
                return self.show()
            if cmd in ("view", "v"):
                leaves = self.leaves()
                if not rest:
                    if self.popup:
                        return self.view_lines(self.popup)
                    if not leaves:
                        return ["no panes"]
                    return self.view_lines(leaves[min(self.focus, len(leaves) - 1)][0])
                if rest.isdigit():
                    if not 1 <= int(rest) <= len(leaves):
                        return [f"no pane {rest} ({len(leaves)} shown by `show`)"]
                    self.focus, self.popup = int(rest) - 1, None
                    return self.view_lines(leaves[self.focus][0])
                self.popup = rest
                return self.view_lines(rest)
            if cmd == "close":
                self.popup = None
                return ["popup closed"]
            if cmd == "do":
                spec, _, arg = rest.partition(" ")
                return self.activate(spec, arg.strip())
            if cmd == "set":
                parts = rest.split(None, 2)
                if len(parts) < 3 or not parts[0].isdigit() or not 0 < int(parts[0]) <= len(self.listed):
                    return ["set <row> <arg> <value>"]
                r = self.listed[int(parts[0]) - 1]
                if model.kind(r["item"]) != "composer":
                    return [f"row {parts[0]} is not a composer"]
                self.composer(r)["values"][parts[1]] = parts[2]
                return self.view_lines(self.shown)
            if cmd in ("facet", "sort"):
                if not self.shown:
                    return ["`view` a panel first"]
                st = self.state.setdefault(self.shown, {"page": 0, "filter": "", "open": {}})
                st.setdefault("facets", {})
                value = self.panel_value(self.shown)
                if cmd == "facet":
                    key, _, want = rest.partition(" ")
                    keys = [f.get("key") for f in model.declared(value, st["page"], "facets") or [] if isinstance(f, dict)]
                    if key not in keys:
                        return [f"facets here: {', '.join(keys) or 'none'}"]
                    if want.strip() in ("", "all"):
                        st["facets"].pop(key, None)
                    else:
                        st["facets"][key] = want.strip()
                else:
                    sorts = model.declared(value, st["page"], "sorts") or []
                    if rest in ("", "given", "0"):
                        st["sort"] = None
                    elif rest.isdigit() and 1 <= int(rest) <= len(sorts):
                        st["sort"] = int(rest) - 1
                    else:
                        return [f"sort 0-{len(sorts)} (0: given order)"]
                return self.view_lines(self.shown)
            if cmd in ("page", "filter", "expand", "collapse"):
                if not self.shown:
                    return ["`view` a panel first"]
                st = self.state.setdefault(self.shown, {"page": 0, "filter": "", "open": {}})
                if cmd == "page" and rest.isdigit():
                    st["page"] = max(0, int(rest) - 1)
                elif cmd == "filter":
                    st["filter"] = rest
                elif cmd in ("expand", "collapse") and rest.isdigit() and 0 < int(rest) <= len(self.listed):
                    st["open"][self.listed[int(rest) - 1]["id"]] = cmd == "expand"
                return self.view_lines(self.shown)
            if cmd == "menu":
                place, _, n = rest.partition(" ")
                return self.menu_lines(place or "pane", n.strip() or None)
            if cmd == "p":
                return self.prefix(rest or None)
            if cmd == "commands":
                return self.commands(rest)
            if cmd == "run":
                name, _, raw = rest.partition(" ")
                try:
                    args = json.loads(raw) if raw.strip() else {}
                except ValueError as e:
                    return [f"args must be JSON: {e}"]
                return self.invoke(name, args, self.ctx())
            if cmd == "safe":
                return self.safe(rest.split())
            return [f"unknown command {cmd!r}; `help` lists them"]


def model_hello():
    principal = os.environ.get("VQRO_PRINCIPAL")
    identity = {"kind": "principal", "principal": principal} if principal and principal != "undeclared" else {"kind": "owner"}
    return {"client": "text", "version": "0.1.0", "profiles": ["base"], "features": FEATURES, "identity": identity}


def main(argv):
    socket_path = os.environ.get("VQRO_SOCKET")
    if not socket_path:
        print("VQRO_SOCKET is not set: start this client with `vqro ui text`", file=sys.stderr)
        return 2
    client = TextClient(socket_path)
    if argv[:1] == ["--once"]:
        for command in " ".join(argv[1:]).split(";"):
            for line in client.handle(command):
                client.say(line)
        return 0
    interactive = sys.stdin.isatty()
    client.say(f"vqro text client · {client.info.get('server')} {client.info.get('version')} · `help` for commands, `p` for recovery")
    for line in client.show():
        client.say(line)
    while True:
        if interactive:
            print("vqro> ", end="", flush=True)
        line = sys.stdin.readline()
        if not line:
            return 0
        try:
            for out in client.handle(line):
                client.say(out)
        except SystemExit:
            return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
