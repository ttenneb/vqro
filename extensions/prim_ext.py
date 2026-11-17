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


def _watch_lifetime():
    """Managed stdin is a lifetime pipe, never extension input or the RPC wire."""
    try:
        while sys.stdin.buffer.read(4096):
            pass
    finally:
        # Exit every thread, even if startup/commands are blocked outside RPC.
        # The parent may have crashed, so no cleanup callback can be required.
        os._exit(0)


if os.environ.get("PRIM_EXTENSION_LIFETIME") == "stdin":
    threading.Thread(target=_watch_lifetime, daemon=True, name="prim-lifetime").start()


MAX_RECORD_BYTES = 64 * 1024 * 1024
MAX_REQUEST_BYTES = 4 * 1024 * 1024
NOTIFICATION_BYTES = 4 * 1024 * 1024
MAX_PENDING = 128


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
        self.notes = queue.Queue(maxsize=128)  # ordered; byte charge remains through callback execution
        self.note_lock = threading.Lock()
        self.note_bytes = 0
        self.handlers = threading.BoundedSemaphore(32)
        threading.Thread(target=self._read, daemon=True).start()
        threading.Thread(target=self._notes, daemon=True).start()

    def _send(self, msg):
        with self.lock:
            wire = (json.dumps(msg) + "\n").encode()
            cap = MAX_REQUEST_BYTES if "method" in msg else MAX_RECORD_BYTES
            if len(wire) > cap:
                raise PrimError("request exceeds 4 MiB; not sent" if "method" in msg else "reply exceeds recovery envelope; handler may have already run", "too_large")
            self.sock.sendall(wire)

    def call(self, method, params=None):
        ev = threading.Event()
        with self.lock:
            if len(self.waiting) >= MAX_PENDING:
                raise PrimError("pending request limit; not sent", "overloaded")
            i = self.next
            self.next += 1
            slot = self.waiting[i] = [ev, None]
        try:
            self._send({"jsonrpc": "2.0", "id": i, "method": method, "params": params or {}})
            if not ev.wait(60):
                raise PrimError("response deadline; outcome indeterminate, never resend automatically", "timeout")
            msg = slot[1]
            if "error" in msg:
                raise PrimError(msg["error"].get("message"), (msg["error"].get("data") or {}).get("kind"))
            return msg.get("result")
        finally:
            with self.lock:
                self.waiting.pop(i, None)

    def _queue_note(self, msg, wire_bytes):
        charge = max(64, wire_bytes)
        with self.note_lock:
            if self.note_bytes + charge > NOTIFICATION_BYTES:
                return False
            try:
                self.notes.put_nowait((msg, charge))
            except queue.Full:
                return False
            self.note_bytes += charge
            return True

    def _read(self):
        try:
            while True:
                line = self.file.readline(MAX_RECORD_BYTES + 1)
                if not line or len(line) > MAX_RECORD_BYTES:
                    break
                msg = json.loads(line)
                if "method" in msg:
                    if len(line) > MAX_REQUEST_BYTES:
                        break
                    if "id" not in msg:
                        if not self._queue_note(msg, len(line)):
                            break  # explicit disconnect/gap, never silent cache divergence
                    elif self.handlers.acquire(blocking=False):
                        threading.Thread(target=self._request, args=(msg,), daemon=True).start()
                    else:
                        self._send({"jsonrpc":"2.0", "id":msg["id"], "error":{"code":-32000,"message":"handler limit; not dispatched","data":{"kind":"overloaded"}}})
                else:
                    with self.lock:
                        slot = self.waiting.get(msg.get("id"))
                        if slot is not None:
                            slot[1] = msg
                            slot[0].set()
        finally:
            os._exit(0)  # core/transport failed; supervisor restarts, never replays calls

    def _request(self, msg):
        try:
            self._dispatch(msg)
        finally:
            self.handlers.release()

    def _note(self, msg, charge):
        try:
            self._dispatch(msg)
        finally:
            with self.note_lock:
                self.note_bytes -= charge

    def _notes(self):
        while True:
            self._note(*self.notes.get())

    def _dispatch(self, msg):
        try:
            result = self.handler(msg["method"], msg.get("params") or {})
            if "id" in msg:
                self._send({"jsonrpc": "2.0", "id": msg["id"], "result": result})
        except Exception as e:  # report, don't crash
            if "id" in msg:
                kind = getattr(e, "kind", None) or "extension"
                self._send({"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32000, "message": str(e), "data": {"kind": kind}}})


# Namespaces an activation's contribute() reconciles (owner-tagged entries only).
CONTRIBUTION_NAMESPACES = ("menu/", "binding/", "view/")


class PrimError(RuntimeError):
    def __init__(self, message, kind=None):
        super().__init__(message)
        self.kind = kind


class Extension:
    def __init__(self, name=None, event_types=None):
        """event_types: only these core event types (e.g. ["entries"]); None for all. Caches only
        follow the types received: without "resource", `resources` stays as of the last snapshot."""
        self.name = os.environ.get("PRIM_EXTENSION", name or "ext")
        self.event_types = event_types
        self.handlers = {}
        self.titles = {}
        self.listeners = []
        self.entries = {}
        self.resources = {}
        self.providers = {}
        self.cache_lock = threading.RLock()
        self.prim = Prim(os.environ["PRIM_SOCKET"], self._handle)
        self.info = self.prim.call("hello", {"extension": self.name, "extension_token": os.environ.get("PRIM_EXTENSION_TOKEN")})
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

    def contribute(self, entries, exclusive=None):
        """Set entries (menu items, bindings, views, ...) owned by this extension; only writes changes.

        Reconciled on activation: the first call of a run is, by default, the extension's whole declared
        set, so entries it owns (`owner` = this extension) under menu/, binding/ and view/ that are not in
        the set are deleted in the same transaction. A new version that stops contributing an item leaves
        no ghost menu item or binding. Later calls only add or update. An extension that builds its
        contributions over several calls passes exclusive=False (or reconciles later with
        exclusive=True and its full set). Every write is compare-and-set; on a conflict the caches are
        reloaded and the whole set is recomputed and retried. Entries owned by others are never touched.
        """
        if exclusive is None:
            exclusive = not getattr(self, "_contributed", False)
        self._contributed = True
        wanted = {key: dict(value, owner=self.name) for key, value in entries.items()}

        def attempt():
            with self.cache_lock:
                ops = [{"op": "set", "key": key, "value": value, "expect": self.version(key)}
                       for key, value in wanted.items() if self.value(key) != value]
                if exclusive:
                    ops += [{"op": "delete", "key": key, "expect": e["version"]}
                            for key, e in self.entries.items()
                            if key.startswith(CONTRIBUTION_NAMESPACES) and key not in wanted
                            and isinstance(e.get("value"), dict) and e["value"].get("owner") == self.name]
            if not ops:
                return None
            result = self.apply(ops, f"{self.name}: contribute")
            # Keep the cache current until the matching event arrives (a later call compares with it).
            versions = {c["key"]: c.get("version") for c in (result or {}).get("changes", [])}
            with self.cache_lock:
                for op in ops:
                    if op["op"] == "delete":
                        self.entries.pop(op["key"], None)
                    elif versions.get(op["key"]) is not None:
                        self.entries[op["key"]] = {"version": versions[op["key"]], "value": op["value"]}
            return result

        return self.retrying(attempt)

    def run(self):
        for name, title in self.titles.items():
            self.prim.call("command.register", {"name": name, "title": title})
        sub = {"from": self.seq, "epoch": self.epoch, "partial_resources": True}
        if self.event_types is not None:
            sub["types"] = list(self.event_types)
        self.prim.call("events.subscribe", sub)
        threading.Event().wait()

    # -- workspace helpers --------------------------------------------------------------------

    def apply(self, ops, note):
        return self.prim.call("workspace.apply", {"ops": ops, "note": note})

    def fresh(self):
        """Reload caches from the core (after a compare-and-set conflict)."""
        # Serialize snapshot acquisition/application with notifications, including command threads.
        with self.cache_lock:
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
        self.providers = dict(snap.get("providers") or {})
        self.seq = snap["seq"]
        self.epoch = snap["epoch"]

    def _handle(self, method, params):
        if method in ("event", "missed"):
            with self.cache_lock:
                event = self._notification(method, params)
            # Extension code may call commands on other threads; never hold the cache lock there.
            if event is not None:
                for fn in self.listeners:
                    fn(*event)
            return None
        if method == "command.invoke":
            fn = self.handlers.get(params["name"])
            if fn is None:
                raise PrimError(f"unknown command {params['name']}", "not_found")
            return fn(params.get("args") or {}) or {"ok": True}
        return None

    def _notification(self, method, params):
        if method == "event":
            if params["epoch"] != self.epoch:
                self.fresh()
            if params["epoch"] != self.epoch or params["seq"] <= self.seq:
                return None
            self.seq = params["seq"]
            t, d = params["type"], params["data"]
            if t == "resource":
                if params.get("partial"):
                    # Only changed fields (we subscribe with partial_resources): merge onto the cache.
                    base = self.resources.get(d["id"])
                    if base is None:
                        # No base: our cache is not what the core assumed. Reload.
                        self._load(self.prim.call("workspace.snapshot"))
                        return "missed", {}
                    d = dict(base, **d)
                self.resources[d["id"]] = d
            elif t == "resource.removed":
                self.resources.pop(d["id"], None)
            elif t == "provider":
                self.providers[d["name"]] = d["state"]
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
            return t, d
        if method == "missed":
            self._load(self.prim.call("workspace.snapshot"))
            return "missed", {}
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
