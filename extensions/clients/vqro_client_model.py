"""The renderer-independent client model in Python (stdlib only): the second implementation of the `base` client
profile (D29). It follows the same contract as the Rust model (`src/ui_model`), written independently from the
spec and checked against the shared fixtures `tests/fixtures/client/*.json`:

- `Workspace`: the snapshot/event cache and contributions (visible menus and bindings, key precedence, gestures);
- `rows`, `missing_for`: panel flattening and `requires`;
- `prefix_rows`, `safe_chooser`: the shared recovery content.
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


def _subtree(item, f):
    return _matches(item, f) or any(_subtree(c, f) for c in item.get("children") or [])


def page_items(value, page=0):
    pages = value.get("pages")
    if isinstance(pages, list):
        if not pages:
            return []
        return pages[min(page, len(pages) - 1)].get("items") or []
    return value.get("items") or []


def rows(value, page=0, filter="", open=None):
    """Visible rows after toolbar, pages, tree expansion and filtering: dicts {item, depth, id, parent,
    has_children, expanded, pinned}. `open` is the user's expansion by row id."""
    open, f, out = open or {}, filter.lower(), []

    def walk(items, depth, parent, prefix):
        for i, item in enumerate(items):
            if f and not _subtree(item, f):
                continue
            rid = item["id"] if isinstance(item.get("id"), str) else f"{prefix}{i}"
            children = item.get("children") or []
            expanded = bool(children) and (bool(f) or open.get(rid, item.get("expanded") is True))
            shown = {k: v for k, v in item.items() if k != "children"}
            me = len(out)
            out.append({"item": shown, "depth": depth, "id": rid, "parent": parent, "has_children": bool(children),
                        "expanded": expanded, "pinned": False})
            if expanded:
                walk(children, depth + (kind(item) != "header"), me, rid + "/")

    for i, item in enumerate(value.get("toolbar") or []):
        out.append({"item": item, "depth": 0, "id": f"toolbar/{i}", "parent": None, "has_children": False, "expanded": False, "pinned": True})
    walk(page_items(value, page), 0, None, "")
    return out


# ---- workspace cache and contributions ----------------------------------------------------------------------

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
                  and not str(v.get("key", "")).startswith("mouse.") and self.visible(v) and when_holds(v, ctx)]
        return found

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
