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
import random
import socket
import sys
import threading
import time
import traceback


def _watch_lifetime():
    """Managed stdin is a lifetime pipe, never extension input or the RPC wire."""
    try:
        # The raw fd, not sys.stdin.buffer: a daemon thread blocked inside the buffered reader holds
        # its lock, and interpreter shutdown then aborts ("Fatal Python error: _enter_buffered_busy").
        while os.read(0, 4096):
            pass
    finally:
        # Exit every thread, even if startup/commands are blocked outside RPC.
        # The parent may have crashed, so no cleanup callback can be required.
        os._exit(0)


if os.environ.get("PRIM_EXTENSION_LIFETIME") == "stdin":
    threading.Thread(target=_watch_lifetime, daemon=True, name="prim-lifetime").start()

# A reply not received by then is `timeout` (outcome indeterminate). Tests shorten it; raising it
# would only hide a slow core. Callers distinguish it from protocol/state errors (TRANSIENT).
CALL_DEADLINE = float(os.environ.get("PRIM_EXT_DEADLINE_S") or 60)
TRANSIENT = ("timeout", "overloaded")  # no answer yet / not admitted: retry later, never a crash
RESEND_SAFE = {"hello", "workspace.snapshot", "command.register", "events.subscribe", "view.publish"}


def _resend_safe(method, params):
    """Resending cannot duplicate an effect: reads, registrations, live views, and workspace
    writes whose every op is compare-and-set (a duplicate then fails with `conflict`)."""
    if method in RESEND_SAFE:
        return True
    ops = (params or {}).get("ops") if isinstance(params, dict) else None
    return method == "workspace.apply" and bool(ops) and all(isinstance(op, dict) and op.get("expect") is not None for op in ops)


MAX_RECORD_BYTES = 64 * 1024 * 1024
MAX_REQUEST_BYTES = 4 * 1024 * 1024
NOTIFICATION_BYTES = 4 * 1024 * 1024
MAX_PENDING = 128


class Prim:
    """JSON-RPC over lines. The core may send us requests (command.invoke) and notifications."""

    def __init__(self, path, handler, *, recover_notifications=False):
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.connect(path)
        self.file = self.sock.makefile("rwb", buffering=0)
        self.lock = threading.Lock()
        self.next = 1
        self.waiting = {}
        self.handler = handler
        self._command_context = threading.local()
        self.notes = queue.Queue(maxsize=128)  # ordered; byte charge remains through callback execution
        self.note_lock = threading.Lock()
        self.note_bytes = 0
        # Extension opts in: its missed handler snapshots/fences cache state.
        # Raw Prim users retain explicit disconnect rather than silently dropping events.
        self.recover_notifications = recover_notifications
        self.note_gap = None
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
        """One request. While `patient` (extension startup), a TRANSIENT failure of a request that is
        safe to resend (reads, registrations, compare-and-set writes) is retried with jittered
        backoff (0.5 s doubling, 30 s cap) instead of raising: a slow core must not burn the crash
        budget before the extension is even up. Anything else raises as before."""
        n = 0
        while True:
            try:
                return self._call(method, params)
            except PrimError as e:
                if e.kind not in TRANSIENT or not getattr(self, "patient", False) or not _resend_safe(method, params):
                    raise
                n += 1
                delay = min(30.0, 0.5 * 2 ** n) * (0.5 + random.random())
                print(f"prim: {getattr(self, 'name', 'ext')}: {method}: {e.kind} (core slow); retrying in {delay:.1f}s", file=sys.stderr, flush=True)
                time.sleep(delay)

    def _call(self, method, params=None):
        proof = getattr(getattr(self, "_command_context", None), "invocation", None)
        if proof is not None and method in ("workspace.apply", "workspace.revert", "command.invoke") and (params is None or isinstance(params, dict)):
            params = dict(params or {})
            params.setdefault("on_behalf_of", proof)
        ev = threading.Event()
        with self.lock:
            if len(self.waiting) >= MAX_PENDING:
                raise PrimError("pending request limit; not sent", "overloaded")
            i = self.next
            self.next += 1
            slot = self.waiting[i] = [ev, None]
        try:
            self._send({"jsonrpc": "2.0", "id": i, "method": method, "params": params or {}})
            if not ev.wait(CALL_DEADLINE):
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
            recover = getattr(self, "recover_notifications", False)
            if recover and self.note_gap is not None:
                return True  # One pending snapshot will cover this dropped prefix.
            # Reserve a small gap marker even while a maximum-size callback is active.
            limit = NOTIFICATION_BYTES - (64 if recover else 0)
            if self.note_bytes + charge <= limit:
                try:
                    self.notes.put_nowait((msg, charge))
                    self.note_bytes += charge
                    return True
                except queue.Full:
                    pass
            if not recover:
                return False
            # Keep the active callback charged; discard only callbacks not yet dispatched.
            while True:
                try:
                    _, old_charge = self.notes.get_nowait()
                    self.note_bytes -= old_charge
                except queue.Empty:
                    break
            marker = {"method": "missed", "params": {"reason": "notification_backlog"}}
            self.note_gap = marker
            self.notes.put_nowait((marker, 64))
            self.note_bytes += 64
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

    def defer(self, msg, delay):
        """Queue a local notification after `delay` seconds, behind every event already queued, so
        its handler sees caches those events updated. A recovery gap may discard it; the gap's own
        `missed` then reruns everything."""
        def put():
            with self.note_lock:
                self.note_bytes += 64
            self.notes.put((msg, 64))
        timer = threading.Timer(delay, put)
        timer.daemon = True
        timer.start()

    def _note(self, msg, charge):
        with self.note_lock:
            local_gap = getattr(self, "note_gap", None) is msg
            if local_gap:
                # Buffer events that arrive during snapshot acquisition. The cache
                # fences them against snapshot.seq; overflow schedules another gap.
                self.note_gap = None
        try:
            if not self._dispatch(msg) and local_gap:
                print("prim: notification recovery failed; disconnecting rather than using a divergent cache", file=sys.stderr, flush=True)
                self.sock.shutdown(socket.SHUT_RDWR)
                return False
            return True
        finally:
            with self.note_lock:
                self.note_bytes -= charge

    def _notes(self):
        while True:
            if not self._note(*self.notes.get()):
                return  # Failed recovery closed the transport; never apply stale queued deltas.

    def _dispatch(self, msg):
        context = getattr(self, "_command_context", None)
        if context is None:
            context = self._command_context = threading.local()
        previous = getattr(context, "invocation", None)
        params = msg.get("params") or {}
        context.invocation = params.get("invocation") if msg.get("method") == "command.invoke" and isinstance(params, dict) else None
        try:
            result = self.handler(msg["method"], msg.get("params") or {})
            if "id" in msg:
                self._send({"jsonrpc": "2.0", "id": msg["id"], "result": result})
            return True
        except Exception as e:  # report, don't crash
            if "id" in msg:
                kind = getattr(e, "kind", None) or "extension"
                self._send({"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32000, "message": str(e), "data": {"kind": kind}}})
            return False
        finally:
            context.invocation = previous


# Namespaces an activation's contribute() reconciles (owner-tagged entries only).
CONTRIBUTION_NAMESPACES = ("menu/", "binding/", "view/")


class PrimError(RuntimeError):
    def __init__(self, message, kind=None):
        super().__init__(message)
        self.kind = kind


class Extension:
    def __init__(self, name=None, event_types=None, view_content=None):
        """event_types: only these types; None for all. view_content: optional {names, prefixes}
        allowlist for live view events. Caches follow only subscribed facts between snapshots."""
        self.name = os.environ.get("PRIM_EXTENSION", name or "ext")
        self.event_types = event_types
        self.view_content = view_content
        self.handlers = {}
        self.titles = {}
        self.listeners = []
        self.entries = {}
        self.resources = {}
        self.providers = {}
        self.cache_lock = threading.RLock()
        self.prim = Prim(os.environ["PRIM_SOCKET"], self._handle, recover_notifications=True)
        # A slow core delays startup (until run() subscribes); it must not burn the crash budget.
        # A dead connection still exits (the reader's EOF).
        self.prim.name, self.prim.patient = self.name, True
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
        """Call fn(type, data) for every core event, after the caches are updated.

        fn must be safe to rerun: after a `missed` gap (or a write conflict) it is called as
        fn("missed", {...}) and should recompute from the caches. A `conflict` from its writes is
        retried that way with bounded backoff; other exceptions are logged. Neither stops other
        listeners or the extension."""
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
        if self.view_content is not None:
            sub["view_content"] = self.view_content
        try:
            self.prim.call("events.subscribe", sub)
        except PrimError as e:
            if e.kind != "conflict":
                raise  # `conflict`: an earlier attempt that timed out did subscribe
        self.prim.patient = False  # running: later calls report timeouts to their callers
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
        """Run fn(); on a `conflict` reload the caches and try again, with a short jittered backoff.

        Inside an event listener whose caches follow `entries` events, a conflict is raised at
        once instead: the listener is rerun after the conflicting write's events arrive, which
        costs no snapshot reload (`_listen`)."""
        listening = getattr(self.__dict__.get("_listening"), "active", False)
        types = getattr(self, "event_types", None)
        if listening and (types is None or "entries" in types):
            attempts = 1
        for i in range(attempts):
            try:
                return fn()
            except PrimError as e:
                if e.kind != "conflict" or i == attempts - 1:
                    raise
                time.sleep(0.02 * 2 ** i * (0.5 + random.random()))
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
            # Only a cache failure is a failed recovery (it raises: disconnect, never diverge).
            # A slow core is not one: keep the last cache, degrade, retry (`_degrade`).
            with self.cache_lock:
                try:
                    event = self._notification(method, params)
                except PrimError as e:
                    if e.kind not in TRANSIENT:
                        raise
                    event = None
                    self._degrade(f"{e.kind} reloading the workspace")
                if event is not None and event[0] == "missed" and self.__dict__.get("_degraded"):
                    self._recovered()  # this reload already made the cache current
            # Extension code may call commands on other threads; never hold the cache lock there.
            # While degraded the cache is known to miss events: no reactions on stale facts.
            if event is not None and not self.__dict__.get("_degraded"):
                for fn in self.listeners:
                    self._listen(fn, event)
            return None
        if method == "prim.recover":
            self._recover()
            return None
        if method == "prim.retry":
            # Deferred reactions rerun as `missed`: recompute from the (now current) caches.
            for fn in list(getattr(self, "_deferred", {})):
                self._listen(fn, ("missed", {"reason": "retry"}), retry=True)
            return None
        if method == "command.invoke":
            fn = self.handlers.get(params["name"])
            if fn is None:
                raise PrimError(f"unknown command {params['name']}", "not_found")
            return fn(params.get("args") or {}) or {"ok": True}
        return None

    def _degrade(self, reason):
        """Keep the last cache (events still apply on top: newer full values, never regressions),
        suppress listeners, and schedule a snapshot reload with bounded jittered backoff."""
        if self.__dict__.get("_degraded"):
            return
        self._degraded = reason
        self._recover_attempts = 0
        print(f"prim: {getattr(self, 'name', 'ext')}: degraded: {reason}; keeping the last state and retrying", file=sys.stderr, flush=True)
        threading.Thread(target=self._say, args=(f"{getattr(self, 'name', 'ext')}: core is slow; keeping the last state and retrying", "warn"), daemon=True).start()
        self._schedule_recover()

    def _say(self, text, style):
        try:
            self.notify(text, style)
        except Exception:
            pass  # best effort on a slow core

    def _schedule_recover(self):
        n = self._recover_attempts = self.__dict__.get("_recover_attempts", 0) + 1
        self.prim.defer({"method": "prim.recover", "params": {}}, min(30.0, 0.5 * 2 ** n) * (0.5 + random.random()))

    def _recover(self):
        if not self.__dict__.get("_degraded"):
            return  # another reload already recovered
        try:
            with self.cache_lock:
                self._load(self.prim.call("workspace.snapshot"))
        except PrimError as e:
            if e.kind in TRANSIENT:
                print(f"prim: {getattr(self, 'name', 'ext')}: still degraded ({e.kind}); retrying", file=sys.stderr, flush=True)
                self._schedule_recover()
                return
            # A state/protocol error: never continue on a divergent cache; supervision restarts us.
            print(f"prim: {getattr(self, 'name', 'ext')}: notification recovery failed ({e.kind}: {e}); disconnecting", file=sys.stderr, flush=True)
            self.prim.sock.shutdown(socket.SHUT_RDWR)
            return
        self._recovered()
        for fn in self.listeners:
            self._listen(fn, ("missed", {"reason": "recovered"}))

    def _recovered(self):
        print(f"prim: {getattr(self, 'name', 'ext')}: recovered after: {self._degraded}", file=sys.stderr, flush=True)
        self._degraded = None
        self._recover_attempts = 0

    @property
    def degraded(self):
        """None, or why the caches are known stale (a slow core); reactions resume on recovery."""
        return self.__dict__.get("_degraded")

    def _listen(self, fn, event, retry=False):
        """Run one listener. Its writes are policy reacting to facts, not cache recovery: a failure
        never stops other listeners or counts as a failed recovery. A compare-and-set conflict
        (someone else wrote first) is normal under concurrent edits: rerun it later with bounded
        backoff, after the events of that write have updated the caches."""
        deferred = self.__dict__.setdefault("_deferred", {})
        local = self.__dict__.setdefault("_listening", threading.local())
        local.active = True
        try:
            fn(*event)
            if retry or event[0] == "missed":
                deferred.pop(fn, None)  # a full recompute succeeded; other events may not redo it
        except PrimError as e:
            if e.kind != "conflict" and e.kind not in TRANSIENT:
                print(f"prim: {getattr(self, 'name', 'ext')}: listener failed: {e.kind}: {e}", file=sys.stderr, flush=True)
                return
            # A conflict, or no answer yet from a slow core: rerun later (writes are CAS-guarded).
            n = deferred[fn] = deferred.get(fn, 0) + 1
            delay = min(5.0 if e.kind == "conflict" else 30.0, 0.1 * 2 ** n) * (0.5 + random.random())
            what = "conflicted" if e.kind == "conflict" else f"got {e.kind}"
            print(f"prim: {getattr(self, 'name', 'ext')}: listener write {what} ({n}); retrying in {delay:.2f}s", file=sys.stderr, flush=True)
            self.prim.defer({"method": "prim.retry", "params": {}}, delay)
        except Exception:
            print(f"prim: {getattr(self, 'name', 'ext')}: listener failed:\n{traceback.format_exc()}", file=sys.stderr, flush=True)
        finally:
            local.active = False

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
            return "missed", params
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
    """Every view named in a layout node, depth-first."""
    out = [] if out is None else out
    if "view" in n:
        out.append(n["view"])
    for c in n.get("children", []):
        views_in(c, out)
    for t in n.get("tabs", []):
        views_in(t["node"], out)
    return out
