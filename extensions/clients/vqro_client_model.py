"""The renderer-independent client model in Python (stdlib only): the second implementation of the `base` client
profile (D29). It follows the same contract as the Rust model (`src/ui_model`), written independently from the
spec and checked against the shared fixtures `tests/fixtures/client/*.json`:

- `Workspace`: the snapshot/event cache and contributions (visible menus and bindings, key precedence, gestures);
- `rows`, `missing_for`: panel flattening and `requires`;
- `prefix_rows`, `safe_chooser`: the shared recovery content;
- `Window`, `reply_layout`, `mark_current`: which layout (workspace) a window shows, its invocation context and
  `current_when` marks.
"""

BASE_FEATURES = ["items", "pages", "tree", "toolbar", "badges", "description", "menu", "input", "options", "select",
                 "chips", "icon", "wrap"]
BUILTIN = "builtin:"
SAFE_MODE_VIEW = "@vqro-safe-mode"
NAMED = ["Up", "Down", "Left", "Right", "Home", "End", "PageUp", "PageDown", "Insert", "Delete", "Tab", "BackTab",
         "Enter", "Esc", "Backspace", "Space"]


# ---- key names ------------------------------------------------------------------------------------------------

def normalize(name):
    """Canonical key name (`C-space` -> `C-Space`, `Ctrl-a` -> `C-a`), or None if it isn't one."""
    s = name
    for word, short in (("ctrl-", "C-"), ("control-", "C-"), ("alt-", "M-"), ("meta-", "M-"), ("option-", "M-"), ("shift-", "S-")):
        while True:
            lower = s.lower()
            i = lower.find(word)
            # Only at the front or after another modifier, and only if something follows.
            while i != -1 and not ((i == 0 or lower[:i].endswith("-")) and len(lower) > i + len(word)):
                i = lower.find(word, i + 1)
            if i == -1:
                break
            s = s[:i] + short + s[i + len(word):]
    ctrl = alt = shift = False
    rest = s
    while len(rest) > 2 and rest[1] == "-" and rest[0].upper() in "CMS":
        ctrl, alt, shift = ctrl or rest[0].upper() == "C", alt or rest[0].upper() == "M", shift or rest[0].upper() == "S"
        rest = rest[2:]
    if len(rest) == 1:
        if rest == " ":
            base = "Space"
        elif shift:
            return None  # a character already carries Shift
        else:
            base = rest.lower() if ctrl else rest
    else:
        lower = rest.lower()
        if lower[:1] == "f" and lower[1:].isdigit() and 1 <= int(lower[1:]) <= 12:
            base = "F" + str(int(lower[1:]))
        else:
            base = next((n for n in NAMED if n.lower() == lower), None)
            if base is None:
                return None
    return ("C-" if ctrl else "") + ("M-" if alt else "") + ("S-" if shift else "") + base


# ---- panels -----------------------------------------------------------------------------------------------------

def kind(item):
    return item.get("type") or "item"


def selectable(item):
    k = kind(item)
    return (k not in ("header", "separator", "text") or (k == "text" and item.get("focusable") is True)) and item.get("disabled") is not True


def missing_for(value, features):
    """Features a panel's outer `requires` names that a client rendering `features` lacks."""
    return [f for f in value.get("requires") or [] if isinstance(f, str)
            and f not in features and not (f.startswith("vqro.") and f[5:] in features)]


def _matches(item, f):
    hay = " ".join(str(item.get(k) or "") for k in ("text", "detail", "description", "summary"))
    hay += " " + " ".join(t for t in item.get("tags") or [] if isinstance(t, str))
    return f in hay.lower()


ROW_TYPES = ["item", "toggle", "header", "separator", "text", "chips", "select"]
MAX_CHAIN = 4


def _native(item, features):
    return (kind(item) in ROW_TYPES or kind(item) in features) and not missing_for(item, features)


def _chips(item, features):
    if kind(item) != "chips" or not isinstance(item.get("chips"), list):
        return item
    shown = []
    for chip in item["chips"]:
        for _ in range(4):
            if not isinstance(chip, dict) or not missing_for(chip, features):
                break
            fb = chip.get("fallback")
            if fb == "drop":
                chip = None
                break
            if not isinstance(fb, dict):
                break
            chip = fb
        if chip is not None:
            shown.append(chip)
    return dict(item, chips=shown)


def expand(item, rid, features, depth=0):
    """Element fallback: [(row, id)] shown for one item (D29)."""
    own = item["id"] if isinstance(item.get("id"), str) else rid
    if depth >= 4 or _native(item, features) or "fallback" not in item:
        return [(_chips(item, features), own)]
    fb = item["fallback"]
    if fb == "drop":
        return []
    if isinstance(fb, dict):
        return expand(fb, own, features, depth + 1)
    if isinstance(fb, list):
        out = []
        for n, row in enumerate(fb):
            if isinstance(row, dict):
                out += expand(row, own if len(fb) == 1 else f"{own}~{n}", features, depth + 1)
        return out
    return [(_chips(item, features), own)]


def _shown(items, prefix, features):
    return [pair for i, item in enumerate(items) if isinstance(item, dict) for pair in expand(item, f"{prefix}{i}", features)]


def _subtree(item, rid, f, features):
    return _matches(item, f) or any(_subtree(c, cid, f, features) for c, cid in _shown(item.get("children") or [], rid + "/", features))


def page_items(value, page=0):
    pages = value.get("pages")
    if isinstance(pages, list):
        if not pages:
            return []
        return pages[min(page, len(pages) - 1)].get("items") or []
    return value.get("items") or []


def rows(value, page=0, filter="", open=None, features=BASE_FEATURES):
    """Visible rows after toolbar, pages, element fallbacks, tree expansion and filtering: dicts {item, depth, id,
    parent, has_children, expanded, pinned}. `open` is the user's expansion by row id."""
    open, f, out = open or {}, filter.lower(), []

    def one(item, rid, depth, parent, f, bottom):
        if f and not _subtree(item, rid, f, features):
            return
        children = item.get("children") or []
        expanded = bool(children) and (bool(f) or open.get(rid, item.get("expanded") is True))
        shown = {k: v for k, v in item.items() if k != "children"}
        me = len(out)
        out.append({"item": shown, "depth": depth, "id": rid, "parent": parent, "has_children": bool(children),
                    "expanded": expanded, "pinned": False, "bottom": bottom})
        if expanded:
            for child, cid in _shown(children, rid + "/", features):
                one(child, cid, depth + (kind(item) != "header"), me, f, bottom)

    for n, (item, _) in enumerate(_shown(value.get("toolbar") or [], "toolbar/", features)):
        out.append({"item": item, "depth": 0, "id": f"toolbar/{n}", "parent": None, "has_children": False, "expanded": False,
                    "pinned": True, "bottom": False})
    top = _shown(page_items(value, page), "", features)
    pin = "pin" in features  # pin: "bottom" rows (feature pin) come last, never filtered out
    for item, rid in top:
        if not (pin and item.get("pin") == "bottom"):
            one(item, rid, 0, None, f, False)
    for item, rid in top:
        if pin and item.get("pin") == "bottom":
            one(item, rid, 0, None, "", True)
    return out


# ---- view-kind fallback -----------------------------------------------------------------------------------------

def merged(view, entries, live):
    """A view's entry with its live content merged over it, or None (`entries` maps `view/<id>` to values)."""
    v = entries.get(f"view/{view}")
    if not isinstance(v, dict):
        return None
    return dict(v, **live[view]) if isinstance(live.get(view), dict) else dict(v)


def renders(v, kinds, features):
    return v.get("kind") in kinds and not missing_for(v, features)


def shown_view(view, value, can_render):
    """(view to draw or None for the placeholder, views visited) following `fallback` (D29)."""
    chain, current = [], view
    while len(chain) <= MAX_CHAIN and current not in chain:
        chain.append(current)
        v = value(current)
        if v is None:
            return None, chain
        if can_render(v):
            return current, chain
        if not isinstance(v.get("fallback"), str):
            return None, chain
        current = v["fallback"]
    return None, chain


# ---- workspace cache and contributions ----------------------------------------------------------------------

def scope(binding):
    """`prefix` (default), `pane` or `composer`."""
    s = binding.get("scope")
    return s if isinstance(s, str) else "prefix"


def when_holds(entry, ctx):
    when = entry.get("when")
    return not isinstance(when, dict) or all(ctx.get(k) == v for k, v in when.items())


def rank(eid, b):
    priority = b.get("priority") if isinstance(b.get("priority"), int) and not isinstance(b.get("priority"), bool) else 0
    return (-priority, not eid.startswith(BUILTIN), eid)


def resolve(cands):
    """Canonical key -> index of the winning binding in `cands` [(id, binding)]; unparsable keys are ignored."""
    won = {}
    for i, (eid, b) in enumerate(cands):
        key = normalize(b["key"]) if isinstance(b.get("key"), str) else None
        if key is not None and (key not in won or rank(eid, b) < rank(*cands[won[key]])):
            won[key] = i
    return won


def builtin_bindings():
    b = [{"key": ":", "command": "client.palette", "label": "all commands", "recovery": True},
         {"key": "o", "command": "client.focus", "args": {"dir": "next"}, "label": "next pane"},
         {"key": "O", "command": "client.focus", "args": {"dir": "prev"}, "label": "previous pane", "hidden": True},
         {"key": "u", "command": "vqro.revert", "label": "undo last change", "recovery": True},
         {"key": "R", "command": "vqro.layout.reset", "label": "reset layout", "recovery": True},
         {"key": "!", "command": "client.safe_mode", "label": "safe mode / recovery choices", "recovery": True},
         {"key": "q", "command": "client.quit", "label": "quit this UI", "recovery": True}]
    return b + [{"key": str(n), "command": "client.tab", "args": {"to": n}, "label": f"tab {n}", "hidden": True} for n in range(1, 10)]


class Workspace:
    def __init__(self):
        self.load({})

    def load(self, snap):
        """Replace the cache with a snapshot; returns whether safe mode was on before."""
        was = getattr(self, "safe_mode", False)
        self.epoch, self.seq = snap.get("epoch", ""), snap.get("seq", 0)
        self.entries = {k: e.get("value") for k, e in (snap.get("entries") or {}).items()}
        self.versions = {k: e.get("version", 0) for k, e in (snap.get("entries") or {}).items()}
        self.resources = {r["id"]: r for r in snap.get("resources") or [] if isinstance(r.get("id"), str)}
        self.commands = {c["name"]: c for c in snap.get("commands") or [] if isinstance(c.get("name"), str)}
        self.extensions = dict(snap.get("extensions") or {})
        self.safe_mode = snap.get("safe_mode") is True
        self.safe_mode_allow = set(snap.get("safe_mode_allow") or [])
        self.provider_extensions = set(snap.get("provider_extensions") or [])
        self.live = dict(snap.get("live") or {})
        return was

    def apply(self, ev):
        """Apply one event record; returns a list of (change, data) for the renderer, ("resync", None) when the
        cache cannot follow."""
        if ev.get("epoch") != self.epoch:
            return [("resync", None)]
        if ev.get("seq", 0) <= self.seq:
            return []
        self.seq = ev["seq"]
        t, d, out = ev.get("type"), ev.get("data") or {}, []
        if t == "entries":
            for c in d.get("changes") or []:
                if c.get("value") is None:
                    self.entries.pop(c["key"], None)
                    self.versions.pop(c["key"], None)
                else:
                    self.entries[c["key"]] = c["value"]
                    self.versions[c["key"]] = c.get("version", 0)
            out.append(("entries", d))
        elif t == "resource":
            record = d
            if ev.get("partial"):
                base = self.resources.get(d.get("id"))
                if base is None:
                    return [("resync", None)]
                record = dict(base, **d)
            self.resources[record["id"]] = record
        elif t == "resource.removed":
            self.resources.pop(d.get("id"), None)
        elif t == "command":
            if d.get("available") is False:
                self.commands.pop(d.get("name"), None)
            else:
                self.commands[d["name"]] = d
        elif t == "extension":
            state = dict(d.get("state") or {})
            if state.get("dependencies") is None and d.get("name") in self.extensions:
                state["dependencies"] = self.extensions[d["name"]].get("dependencies")
            self.extensions[d["name"]] = state
            if state.get("status") == "failed":
                out.append(("failed", d["name"]))
        elif t == "safe_mode":
            was, self.safe_mode = self.safe_mode, d.get("on") is True
            self.safe_mode_allow = set(d.get("allow") or [])
            self.provider_extensions = set(d.get("provider_extensions") or [])
            out.append(("safe_mode", (self.safe_mode, was)))
        elif t == "view.content":
            if d.get("content") is None:
                self.live.pop(d.get("view"), None)
            else:
                self.live[d["view"]] = d["content"]
        elif t == "notify":
            out.append(("notify", d))
        return out

    def visible(self, e):
        owner = e.get("owner")
        if isinstance(owner, str):
            key = f"extension/{owner}"
            decl = self.entries.get(key)
            on = key in self.entries and not (isinstance(decl, dict) and decl.get("enabled") is False)
            if (owner not in self.safe_mode_allow) if self.safe_mode else not on:
                return False
        cmd = e.get("command") if isinstance(e.get("command"), str) else ""
        return cmd.startswith("client.") or (self.commands.get(cmd) or {}).get("available") is True

    def menu_entries(self, place, ctx):
        found = [(v["order"] if isinstance(v.get("order"), int) and not isinstance(v.get("order"), bool) else 50, k, v) for k, v in self.entries.items()
                 if k.startswith("menu/") and isinstance(v, dict) and v.get("where") == place and self.visible(v) and when_holds(v, ctx)]
        return [v for _, _, v in sorted(found, key=lambda x: (x[0], x[1]))]

    def key_candidates(self, ctx):
        found = [(BUILTIN + b["key"], b) for b in builtin_bindings()]
        found += [(k, v) for k, v in sorted(self.entries.items()) if k.startswith("binding/") and isinstance(v, dict)
                  and scope(v) == "prefix" and not str(v.get("key", "")).startswith("mouse.") and self.visible(v) and when_holds(v, ctx)]
        return found

    def scoped_binding(self, key, scope_name, ctx):
        """The binding a key runs without the prefix: scope "pane" (not typing) or "composer" (typing); never in
        terminal panes."""
        if (ctx or {}).get("kind") == "terminal":
            return None
        found = [(rank(k, b), b) for k, b in self.entries.items() if k.startswith("binding/") and isinstance(b, dict)
                 and scope(b) == scope_name and self.visible(b) and when_holds(b, ctx)
                 and isinstance(b.get("key"), str) and normalize(b["key"]) == key]
        if not found:
            return None
        b = min(found, key=lambda x: x[0])[1]
        return None if b.get("command") == "client.none" else b

    def binding_for(self, key, ctx):
        cands = self.key_candidates(ctx)
        i = resolve(cands).get(key)
        if i is None or cands[i][1].get("command") == "client.none":
            return None
        return cands[i][1]

    def gesture(self, key, place):
        found = [(rank(k, b), b) for k, b in self.entries.items() if k.startswith("binding/") and isinstance(b, dict)
                 and b.get("key") == key and b.get("where", "pane") == place and self.visible(b)]
        if not found:
            return None
        b = min(found, key=lambda x: x[0])[1]
        return None if b.get("command") == "client.none" else b

    def binding_label(self, b):
        cmd = b.get("command") or ""
        if cmd == "vqro.safe_mode" and self.safe_mode:
            return "leave safe mode"
        return b.get("label") or (self.commands.get(cmd) or {}).get("title") or cmd


# ---- recovery content -------------------------------------------------------------------------------------------

def prefix_rows(ws, ctx):
    """The prefix menu: winning binding per key (client defaults first), restart rows for failed extensions after
    the first row, then every recovery action no key reaches any more. Rows are (key or None, label, binding)."""
    cands = ws.key_candidates(ctx)
    winners = resolve(cands)
    out = []
    for i, (_, b) in enumerate(cands):
        key = normalize(b["key"]) if isinstance(b.get("key"), str) else None
        if key is not None and winners.get(key) == i and b.get("command") != "client.none" and b.get("hidden") is not True:
            out.append((key, ws.binding_label(b), b))
    failed = []
    for name, state in sorted(ws.extensions.items()):
        if (state or {}).get("status") != "failed":
            continue
        if ws.safe_mode:
            allowed = name in ws.safe_mode_allow
        else:
            key = f"extension/{name}"
            decl = ws.entries.get(key)
            allowed = key in ws.entries and not (isinstance(decl, dict) and decl.get("enabled") is False)
        label = f"restart {name} (failed)" if allowed else f"clear failure: {name} (stays stopped)"
        failed.append((None, label, {"command": "vqro.extension.restart", "args": {"name": name}}))
    at = min(len(out), 1)
    out[at:at] = failed
    for b in builtin_bindings():
        if b.get("recovery") and not any(r[2].get("command") == b["command"] for r in out):
            out.append((None, ws.binding_label(b), b))
    return out


def needs_text(dependencies):
    out = []
    for need in (dependencies or {}).get("needs") or []:
        cap = need.get("cap")
        if not isinstance(cap, str) or cap.startswith("ui/"):
            continue
        who = [p.get("name") for p in need.get("providers") or [] if isinstance(p.get("name"), str)]
        if "vqro" in who:
            continue
        text = " or ".join(who) if who else (need.get("suggest") or cap)
        if text not in out:
            out.append(text)
    return ", ".join(out)[:120]


def safe_chooser(ws, selected):
    """The safe-mode chooser as panel data (the same content as every other client shows)."""
    declared = sorted((k[len("extension/"):], v) for k, v in ws.entries.items() if k.startswith("extension/"))
    items = [{"type": "text", "text": "Nothing starts automatically. Choose this session's recovery set; saved enabled settings are unchanged."}]
    for provider, title in ((True, "Providers / backends"), (False, "UI extensions")):
        items.append({"type": "header", "text": title})
        count = 0
        for name, value in declared:
            value = value if isinstance(value, dict) else {}
            if (value.get("kind") == "provider" or name in ws.provider_extensions) != provider:
                continue
            argv = value.get("argv") if isinstance(value.get("argv"), list) else []
            description = (value.get("description") if isinstance(value.get("description"), str)
                           else argv[0] if argv and isinstance(argv[0], str) else "Declared extension")[:160]
            state = ws.extensions.get(name)
            if state is not None:
                if state.get("status") == "failed":
                    description += " -- failed: " + (state.get("last_exit") if isinstance(state.get("last_exit"), str) else "unknown failure")
                needs = needs_text(state.get("dependencies"))
                if needs:
                    description += " -- needs " + needs
            items.append({"id": f"safe/{name}", "text": ("[x] " if name in selected else "[ ] ") + name, "description": description,
                          "command": "client.safe_mode.toggle", "args": {"name": name}})
            count += 1
        if not count:
            items.append({"type": "text", "text": "(none declared)"})
    items += [{"type": "separator"}, {"id": "safe/apply", "text": "Enable selected", "command": "client.safe_mode.apply"},
              {"id": "safe/leave", "text": "Leave safe mode", "command": "client.safe_mode.leave"}]
    return {"title": "Safe mode recovery", "items": items}


# ---- the window's layout ---------------------------------------------------------------------------------------

def default_layout(ws):
    """The shared default `workspace/active`: the layout anyone last switched to."""
    active = ws.entries.get("workspace/active")
    name = active.get("layout") if isinstance(active, dict) else None
    return name if isinstance(name, str) else None


class Window:
    """Which layout this window shows: window state, never workspace state. It starts on the shared default and
    then moves only on its own switches (a reply's `{"client": {"layout"}}`, `client.layout`, its own reset) or
    when its layout is deleted (then it falls back to the default)."""

    def __init__(self, pinned=None):
        self.pinned = pinned

    def _exists(self, ws, name):
        return isinstance(name, str) and f"layout/{name}" in ws.entries

    def shown(self, ws):
        return self.pinned if self._exists(ws, self.pinned) else default_layout(ws)

    def settle(self, ws):
        """After a snapshot and every `entries` event: "Same", {"Took": name} or {"Closed": {"was", "now"}}."""
        if self._exists(ws, self.pinned):
            return "Same"
        now = default_layout(ws)
        now = now if self._exists(ws, now) else None
        if self.pinned is not None:
            was, self.pinned = self.pinned, now
            return {"Closed": {"was": was, "now": now}}
        if now is None:
            return "Same"
        self.pinned = now
        return {"Took": now}

    def show(self, ws, name):
        """Show `name` here only: (True, changed) or (False, message)."""
        if not self._exists(ws, name):
            return False, f"no layout {name}"
        changed, self.pinned = self.pinned != name, name
        return True, changed

    def client_layout(self, ws, args):
        name = (args or {}).get("name") or (args or {}).get("layout")
        if not isinstance(name, str):
            name = default_layout(ws)
            if name is None:
                return False, "client.layout: no layout to show"
        return self.show(ws, name)

    def context(self, ws):
        layout = self.shown(ws)
        settings = ws.entries.get(f"doc/layout/{layout}") if layout else None
        title = settings.get("title") if isinstance(settings, dict) and isinstance(settings.get("title"), str) else layout
        return {"layout": layout, "layout_version": ws.versions.get(f"layout/{layout}") if layout else None, "layout_title": title}

    def invocation_context(self, ws, ctx):
        """The context an invocation carries: the place's own, with this window's layout when it names none."""
        ctx = dict(ctx) if isinstance(ctx, dict) else {}
        if ctx.get("layout") is None:
            window = self.context(ws)
            ctx["layout"], ctx["layout_version"] = window["layout"], window["layout_version"]
        return ctx


def reply_layout(command, reply):
    """The layout a reply asks the invoking window to show (its hint), or `main` after this window's own reset."""
    hints = reply.get("client") if isinstance(reply, dict) else None
    layout = hints.get("layout") if isinstance(hints, dict) else None
    if isinstance(layout, str):
        return layout
    return "main" if command == "vqro.layout.reset" else None


def mark_current(value, layout):
    """Rows with `current_when: {"layout": name}` get the accent style (unless styled) and a leading "current"
    badge in the window showing `name`. Returns a marked copy; panel content is shared by every window."""
    import copy
    value = copy.deepcopy(value)

    def rows(items):
        for row in items if isinstance(items, list) else []:
            if not isinstance(row, dict):
                continue
            when = row.get("current_when")
            if layout is not None and isinstance(when, dict) and when.get("layout") == layout:
                if row.get("style") is None:
                    row["style"] = "accent"
                row["badges"] = [{"text": "current", "style": "accent"}] + list(row.get("badges") or [])
            if "children" in row:
                rows(row["children"])
    if isinstance(value, dict):
        rows(value.get("items"))
        rows(value.get("toolbar"))
        for page in value.get("pages") or []:
            if isinstance(page, dict):
                rows(page.get("items"))
    return value


# ---- formatted text (format: "markdown", feature markdown) ---------------------------------------------------

import re as _re
import unicodedata as _ud


def _cw(c):
    if _ud.combining(c):
        return 0
    return 2 if _ud.east_asian_width(c) in ("W", "F") else 1


def _width(s):
    return sum(_cw(c) for c in s)


def _md_parse(text):
    blocks, para, in_item = [], None, False
    lines = iter(text.replace("\r", "").replace("\t", "    ").split("\n"))

    def flush():
        nonlocal para
        if para is not None:
            blocks.append(("para", para))
            para = None
    for line in lines:
        trimmed = line.lstrip(" ")
        if line.lstrip().startswith("```"):
            flush()
            code = []
            for l in lines:
                if l.lstrip().startswith("```"):
                    break
                code.append(l)
            blocks.append(("code", code))
            in_item = False
            continue
        if not line.strip():
            flush()
            if blocks and blocks[-1][0] != "blank":
                blocks.append(("blank", None))
            in_item = False
            continue
        hashes = len(line) - len(line.lstrip("#"))
        if 1 <= hashes <= 3 and line[hashes:].startswith(" "):
            flush()
            blocks.append(("heading", line[hashes:].strip()))
            in_item = False
            continue
        indent = len(line) - len(trimmed)
        marker = None
        if trimmed.startswith(("- ", "* ")):
            marker = ("•", 2)
        else:
            m = _re.match(r"([0-9]{1,9})\. ", trimmed)
            if m:
                marker = (m.group(1) + ".", len(m.group(1)) + 2)
        if marker:
            flush()
            blocks.append(("item", [min(indent // 2, 4), marker[0], trimmed[marker[1]:].strip()]))
            in_item = True
            continue
        if in_item and indent >= 2:
            blocks[-1][1][2] += " " + line.strip()
            continue
        in_item = False
        para = line.strip() if para is None else para + " " + line.strip()
    flush()
    return blocks


def _md_inline(s, plain):
    out, buf, i = [], "", 0
    while i < len(s):
        if s[i] == "`":
            j = s.find("`", i + 1)
            if j != -1:
                if buf:
                    out.append((buf, plain))
                    buf = ""
                out.append((s[i + 1:j], "c"))
                i = j + 1
                continue
        if s.startswith("**", i):
            j = s.find("**", i + 2)
            if j > i + 2:
                if buf:
                    out.append((buf, plain))
                    buf = ""
                out.append((s[i + 2:j], "h" if plain == "h" else "b"))
                i = j + 2
                continue
        buf += s[i]
        i += 1
    if buf:
        out.append((buf, plain))
    return out


def _md_merge(line):
    out = []
    for t, s in line:
        if out and out[-1][1] == s:
            out[-1] = (out[-1][0] + t, s)
        else:
            out.append((t, s))
    return out


def _md_wrap(spans, w):
    w = max(1, w)
    units, space = [], True
    for text, style in spans:
        word = ""
        for c in text:
            if c.isspace():
                if word:
                    if space or not units:
                        units.append([(word, style)])
                    else:
                        units[-1].append((word, style))
                    word = ""
                space = True
            else:
                word += c
        if word:
            if space or not units:
                units.append([(word, style)])
            else:
                units[-1].append((word, style))
            space = False
    out, line, used = [], [], 0
    for unit in units:
        uw = sum(_width(t) for t, _ in unit)
        if used > 0 and used + 1 + uw > w:
            out.append(line)
            line, used = [], 0
        if used > 0:
            prev = line[-1][1] if line else ""
            line.append((" ", prev if prev == unit[0][1] else ""))
            used += 1
        for text, style in unit:
            for c in text:
                cw = _cw(c)
                if used + cw > w and used > 0:
                    out.append(line)
                    line, used = [], 0
                line.append((c, style))
                used += cw
    if line or not out:
        out.append(line)
    return [_md_merge(l) for l in out]


def _md_cut(s, w):
    out, used = "", 0
    for c in s:
        if used + _cw(c) > max(0, w - 1):
            break
        out += c
        used += _cw(c)
    return out + "…"


def markdown_lines(text, width):
    """Formatted text laid out at `width` cells: a list of lines, each a list of (text, style) spans."""
    w = max(1, width)
    out, prev_item, gap = [], False, False
    for kind_, value in _md_parse(text):
        if kind_ == "blank":
            gap = True
            continue
        is_item = kind_ == "item"
        if out and (gap or not (prev_item and is_item)):
            out.append([])
        gap, prev_item = False, is_item
        if kind_ == "heading":
            out += _md_wrap(_md_inline(value, "h"), w)
        elif kind_ == "para":
            out += _md_wrap(_md_inline(value, ""), w)
        elif kind_ == "item":
            level, marker, text_ = value
            prefix = "  " * level + marker + " "
            pw = _width(prefix)
            for n, l in enumerate(_md_wrap(_md_inline(text_, ""), max(1, w - pw))):
                out.append(_md_merge([(prefix if n == 0 else " " * pw, "")] + l))
        else:
            for l in value:
                shown = "  " + l
                out.append([(_md_cut(shown, w) if _width(shown) > w else shown, "pre")])
    return out


# ---- composer and multi-line prompts (features composer, multiline) --------------------------------------------

DEFAULT_MAX_BYTES, MAX_BYTES = 16 * 1024, 64 * 1024


class Editor:
    """A multi-line text buffer with a cursor (a character index); key handling shared by every client."""

    def __init__(self, text="", max_bytes=None, submit=None):
        self.text = list(text)
        self.cursor = len(self.text)
        self.max_bytes = min(int(max_bytes), MAX_BYTES) if isinstance(max_bytes, int) and not isinstance(max_bytes, bool) else DEFAULT_MAX_BYTES
        self.submit_ctrl = submit == "C-Enter"

    def string(self):
        return "".join(self.text)

    def insert(self, s):
        s = s.replace("\r\n", "\n").replace("\r", "\n").replace("\t", "    ")
        clean = [c for c in s if c == "\n" or _ud.category(c) != "Cc"]
        if not clean:
            return "ignored"
        if len(self.string().encode()) + len("".join(clean).encode()) > self.max_bytes:
            return "refused"
        self.text[self.cursor:self.cursor] = clean
        self.cursor += len(clean)
        return "edit"

    def _start(self, at):
        i = at
        while i > 0 and self.text[i - 1] != "\n":
            i -= 1
        return i

    def _end(self, at):
        i = at
        while i < len(self.text) and self.text[i] != "\n":
            i += 1
        return i

    def key(self, key):
        if (key == "Enter" and not self.submit_ctrl) or (key == "C-Enter" and self.submit_ctrl):
            return "ignored" if not self.string().strip() else "submit"
        if key in ("Enter", "S-Enter", "M-Enter", "C-j"):
            return self.insert("\n")
        if key == "Esc":
            return "leave"
        if key == "Backspace":
            if self.cursor == 0:
                return "ignored"
            self.cursor -= 1
            del self.text[self.cursor]
            return "edit"
        if key == "Delete":
            if self.cursor >= len(self.text):
                return "ignored"
            del self.text[self.cursor]
            return "edit"
        if key == "Left":
            if self.cursor == 0:
                return "ignored"
            self.cursor -= 1
            return "edit"
        if key == "Right":
            if self.cursor >= len(self.text):
                return "ignored"
            self.cursor += 1
            return "edit"
        if key == "Home":
            self.cursor = self._start(self.cursor)
            return "edit"
        if key == "End":
            self.cursor = self._end(self.cursor)
            return "edit"
        if key in ("Up", "Down"):
            start = self._start(self.cursor)
            column = self.cursor - start
            if key == "Up":
                if start == 0:
                    return "ignored"
                target = self._start(start - 1)
            else:
                end = self._end(self.cursor)
                if end >= len(self.text):
                    return "ignored"
                target = end + 1
            self.cursor = min(target + column, self._end(target))
            return "edit"
        if key == "Space":
            return self.insert(" ")
        if len(key) == 1:
            return self.insert(key)
        return "ignored"


def composer_controls(row):
    return [c for c in row.get("controls") or [] if isinstance(c, dict) and isinstance(c.get("arg"), str) and c.get("type") in ("select", "chips")]


class Composer:
    """A composer row's client-local state: draft, cursor and control values, until Send."""

    def __init__(self, row):
        self.values = {}
        for c in composer_controls(row):
            if c.get("type") == "chips":
                self.values[c["arg"]] = next((x.get("value") for x in c.get("chips") or [] if isinstance(x, dict) and x.get("checked") is True), None)
            else:
                self.values[c["arg"]] = c.get("value")
        self.editor = Editor(row.get("prefill") if isinstance(row.get("prefill"), str) else "", row.get("max_bytes"), row.get("submit"))

    def submit_args(self, row):
        args = dict(row["args"]) if isinstance(row.get("args"), dict) else {}
        for k, v in self.values.items():
            if v is not None:
                args[k] = v
        args["text"] = self.editor.string()
        return args
