"""Tiny helper for writing prim extensions in Python (stdlib only).

An extension connects to the core, keeps a cache of workspace entries and resources, registers
commands, and contributes entries (menu items, bindings, views) that carry `owner` so clients hide
them while the extension is off. Everything goes through the public core protocol.

    ext = Extension()

    @ext.command("hello.say", "Say hello")
    def say(args):
        return {"hi": args.get("context", {}).get("view")}

    ext.contribute({"menu/hello.say": {"where": "pane", "label": "Say hello", "command": "hello.say"}})
    ext.run()
"""
import json
import os
import queue
import socket
import sys
import threading


class Prim:
    """JSON-RPC over lines. The core may send us requests (command.invoke) and notifications."""

    def __init__(self, path, handler):
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.connect(path)
        self.file = self.sock.makefile("rwb", buffering=0)
        self.lock = threading.Lock()
        self.next = 1
        self.waiting = {}
        self.handler = handler
        self.notes = queue.Queue()  # notifications are handled in order, on one thread
        threading.Thread(target=self._read, daemon=True).start()
        threading.Thread(target=self._notes, daemon=True).start()

    def _send(self, msg):
        with self.lock:
            self.file.write((json.dumps(msg) + "\n").encode())

    def call(self, method, params=None):
        ev = threading.Event()
        with self.lock:
            i = self.next
            self.next += 1
            self.waiting[i] = [ev, None]
        self._send({"jsonrpc": "2.0", "id": i, "method": method, "params": params or {}})
        ev.wait()
        msg = self.waiting.pop(i)[1]
        if "error" in msg:
            raise PrimError(msg["error"].get("message"), (msg["error"].get("data") or {}).get("kind"))
        return msg.get("result")

    def _read(self):
        for line in self.file:
            msg = json.loads(line)
            if "method" in msg and "id" not in msg:
                self.notes.put(msg)
            elif "method" in msg:
                threading.Thread(target=self._dispatch, args=(msg,), daemon=True).start()
            elif msg.get("id") in self.waiting:
                slot = self.waiting[msg["id"]]
                slot[1] = msg
                slot[0].set()
        os._exit(0)  # the core went away; the supervisor restarts us

    def _notes(self):
        while True:
            self._dispatch(self.notes.get())

    def _dispatch(self, msg):
        try:
            result = self.handler(msg["method"], msg.get("params") or {})
            if "id" in msg:
                self._send({"jsonrpc": "2.0", "id": msg["id"], "result": result})
        except Exception as e:  # report, don't crash
            if "id" in msg:
                kind = getattr(e, "kind", None) or "extension"
                self._send({"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32000, "message": str(e), "data": {"kind": kind}}})


class PrimError(RuntimeError):
    def __init__(self, message, kind=None):
        super().__init__(message)
        self.kind = kind


class Extension:
    def __init__(self, name=None):
        self.name = os.environ.get("PRIM_EXTENSION", name or "ext")
        self.handlers = {}
        self.titles = {}
        self.listeners = []
        self.entries = {}
        self.resources = {}
        self.prim = Prim(os.environ["PRIM_SOCKET"], self._handle)
        self.info = self.prim.call("hello", {"extension": self.name})
        self._load(self.prim.call("workspace.snapshot"))

    # -- setup --------------------------------------------------------------------------------

    def command(self, name, title):
        def deco(fn):
            self.handlers[name] = fn
            self.titles[name] = title
            return fn
        return deco

    def on_event(self, fn):
        """Call fn(type, data) for every core event, after the caches are updated."""
        self.listeners.append(fn)
        return fn

    def contribute(self, entries):
        """Set entries (menu items, bindings, ...) owned by this extension; only writes changes."""
        ops = []
        for key, value in entries.items():
            value = dict(value, owner=self.name)
            if self.entries.get(key, {}).get("value") != value:
                ops.append({"op": "set", "key": key, "value": value})
        if ops:
            self.apply(ops, f"{self.name}: contribute")

    def run(self):
        for name, title in self.titles.items():
            self.prim.call("command.register", {"name": name, "title": title})
        self.prim.call("events.subscribe", {"from": self.seq})
        threading.Event().wait()

    # -- workspace helpers --------------------------------------------------------------------

    def apply(self, ops, note):
        return self.prim.call("workspace.apply", {"ops": ops, "note": note})

    def fresh(self):
        """Reload caches from the core (after a compare-and-set conflict)."""
        self._load(self.prim.call("workspace.snapshot"))

    def retrying(self, fn, attempts=3):
        """Run fn(); on a `conflict` reload the caches and try again."""
        for i in range(attempts):
            try:
                return fn()
            except PrimError as e:
                if e.kind != "conflict" or i == attempts - 1:
                    raise
                self.fresh()

    def publish(self, view, content):
        """Set a view's live content (merged over its entry; not saved, not in history,
        cleared if this extension disconnects). Use for derived, frequently changing UI."""
        return self.prim.call("view.publish", {"view": view, "content": content})

    def notify(self, text, style=None):
        """A short message every client shows briefly (style: ok, warn, error)."""
        return self.prim.call("ui.notify", {"text": text, "style": style})

    def invoke(self, name, args=None):
        return self.prim.call("command.invoke", {"name": name, "args": args or {}})

    def value(self, key, default=None):
        e = self.entries.get(key)
        return e["value"] if e else default

    def version(self, key):
        e = self.entries.get(key)
        return e["version"] if e else 0

    def active_layout(self):
        """(key, version, deep copy of the root) of the active layout, or None."""
        a = self.value("workspace/active")
        if not a:
            return None
        key = f"layout/{a['layout']}"
        if key not in self.entries:
            return None
        return key, self.version(key), json.loads(json.dumps(self.value(key)))

    def log(self, *a):
        print(f"[{self.name}]", *a, file=sys.stderr, flush=True)

    # -- internals ----------------------------------------------------------------------------

    def _load(self, snap):
        self.entries = dict(snap["entries"])
        # Live view content from every extension: also a way to share derived data.
        self.live = dict(snap.get("live") or {})
        self.resources = {r["id"]: r for r in snap["resources"]}
        self.seq = snap["seq"]

    def _handle(self, method, params):
        if method == "event":
            t, d = params["type"], params["data"]
            if t == "resource":
                self.resources[d["id"]] = d
            elif t == "resource.removed":
                self.resources.pop(d["id"], None)
            elif t == "view.content":
                if d["content"] is None:
                    self.live.pop(d["view"], None)
                else:
                    self.live[d["view"]] = d["content"]
            elif t == "entries":
                for c in d["changes"]:
                    if c["value"] is None:
                        self.entries.pop(c["key"], None)
                    else:
                        self.entries[c["key"]] = {"version": c["version"], "value": c["value"]}
            for fn in self.listeners:
                fn(t, d)
            return None
        if method == "missed":
            self._load(self.prim.call("workspace.snapshot"))
            for fn in self.listeners:
                fn("missed", {})
            return None
        if method == "command.invoke":
            fn = self.handlers.get(params["name"])
            if fn is None:
                raise PrimError(f"unknown command {params['name']}", "not_found")
            return fn(params.get("args") or {}) or {"ok": True}
        return None


# -- layout tree helpers (shared by the layout extensions) ----------------------------------------

def node_at(root, path):
    n = root
    for i in path:
        if "children" in n:
            n = n["children"][i]
        elif "tabs" in n:
            n = n["tabs"][i]["node"]
        else:
            return None
    return n


def replace_at(root, path, new):
    """Return root with the node at path replaced."""
    if not path:
        return new
    parent = node_at(root, path[:-1])
    i = path[-1]
    if "children" in parent:
        parent["children"][i] = new
    else:
        parent["tabs"][i]["node"] = new
    return root


def first_tabs(n):
    if "tabs" in n:
        return n
    for c in n.get("children", []):
        t = first_tabs(c)
        if t is not None:
            return t
    return None


def views_in(n, out=None):
    out = [] if out is None else out
    if "view" in n:
        out.append(n["view"])
    for c in n.get("children", []):
        views_in(c, out)
    for t in n.get("tabs", []):
        views_in(t["node"], out)
    return out
