"""Snapshot fencing regressions for the stdlib extension helper (no running core)."""
import pathlib
import queue
import sys
import threading
import time
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "extensions"))
from prim_ext import Extension, Prim, PrimError, NOTIFICATION_BYTES


def snapshot(seq, value):
    return {"epoch": "boot", "seq": seq, "entries": {"doc/test/value": {"version": seq, "value": value}}, "resources": []}


def event(seq, value):
    return {"epoch": "boot", "seq": seq, "type": "entries", "data": {"changes": [{"key": "doc/test/value", "version": seq, "value": value}]}}


class InvocationAttribution(unittest.TestCase):
    def prim(self, handler):
        prim = Prim.__new__(Prim)
        prim.lock = threading.Lock()
        prim.next = 1
        prim.waiting = {}
        prim._command_context = threading.local()
        prim.handler = handler
        messages = []
        def send(message):
            messages.append(message)
            if "method" in message:
                slot = prim.waiting[message["id"]]
                slot[1] = {"result": {"ok": True}}
                slot[0].set()
        prim._send = send
        return prim, messages

    def test_command_context_covers_workspace_and_nested_calls_without_mutating_params(self):
        params = {"ops": []}
        def handler(method, args):
            prim.call("workspace.apply", params)
            prim.call("workspace.revert", {"txn": 1})
            prim.call("command.invoke", {"name": "nested"})
            prim.call("workspace.snapshot")
        prim, messages = self.prim(handler)
        prim._dispatch({"id": 100, "method": "command.invoke", "params": {"invocation": "proof"}})
        requests = [msg for msg in messages if "method" in msg]
        self.assertEqual([msg["params"].get("on_behalf_of") for msg in requests], ["proof", "proof", "proof", None])
        self.assertNotIn("on_behalf_of", params)
        prim.call("workspace.apply", params)
        self.assertNotIn("on_behalf_of", messages[-1]["params"], "scope ends with reply")

    def test_events_and_detached_threads_do_not_inherit_a_user_command(self):
        def handler(method, args):
            if method == "command.invoke":
                prim._dispatch({"method": "event", "params": {}})
                thread = threading.Thread(target=lambda: prim.call("workspace.apply", {"note": "background"}))
                thread.start(); thread.join(2)
                self.assertFalse(thread.is_alive())
                prim.call("workspace.apply", {"note": "command"})
            else:
                prim.call("workspace.apply", {"note": "event"})
        prim, messages = self.prim(handler)
        prim._dispatch({"id": 100, "method": "command.invoke", "params": {"invocation": "proof"}})
        requests = [msg["params"] for msg in messages if msg.get("method") == "workspace.apply"]
        self.assertEqual({p["note"]: p.get("on_behalf_of") for p in requests}, {"event": None, "background": None, "command": "proof"})

    def test_failed_handler_clears_context_and_explicit_claims_are_not_rewritten(self):
        def handler(method, args):
            prim.call("workspace.apply", {"on_behalf_of": "explicit-invalid-proof"})
            raise RuntimeError("failed after work")
        prim, messages = self.prim(handler)
        prim._dispatch({"id": 100, "method": "command.invoke", "params": {"invocation": "proof"}})
        self.assertEqual(messages[0]["params"]["on_behalf_of"], "explicit-invalid-proof")
        self.assertIn("error", messages[-1])
        prim.call("workspace.apply", {})
        self.assertNotIn("on_behalf_of", messages[-1]["params"])

    def test_concurrent_invocations_keep_their_own_context(self):
        from concurrent.futures import ThreadPoolExecutor
        barrier = threading.Barrier(2)
        def handler(method, args):
            barrier.wait(2)
            prim.call("workspace.apply", {"note": args["name"]})
        prim, messages = self.prim(handler)
        with ThreadPoolExecutor(max_workers=2) as pool:
            futures = [pool.submit(prim._dispatch, {"id": i, "method": "command.invoke", "params": {"invocation": name, "name": name}})
                       for i, name in enumerate(("a", "b"), 100)]
            for future in futures: future.result(3)
        requests = [msg["params"] for msg in messages if msg.get("method") == "workspace.apply"]
        self.assertEqual({p["note"]: p.get("on_behalf_of") for p in requests}, {"a": "a", "b": "b"})
        self.assertFalse(any("error" in msg for msg in messages))


class InboundBounds(unittest.TestCase):
    def test_notification_bytes_stay_charged_until_callback_finishes(self):
        prim = Prim.__new__(Prim)
        prim.notes = queue.Queue(maxsize=128)
        prim.note_lock = threading.Lock()
        prim.note_bytes = 0
        prim.handler = lambda method, params: None
        msg = {"method":"event", "params":{}}
        accepted = 0
        while prim._queue_note(msg, 65536):
            accepted += 1
        self.assertEqual(accepted, NOTIFICATION_BYTES // 65536)
        self.assertLessEqual(prim.note_bytes, NOTIFICATION_BYTES)
        prim._note(*prim.notes.get_nowait())
        self.assertTrue(prim._queue_note(msg, 65536))
        while not prim.notes.empty():
            prim._note(*prim.notes.get_nowait())
        self.assertEqual(prim.note_bytes, 0)

    def test_tiny_notifications_have_a_metadata_and_count_bound(self):
        prim = Prim.__new__(Prim)
        prim.notes = queue.Queue(maxsize=128)
        prim.note_lock = threading.Lock()
        prim.note_bytes = 0
        count = 0
        while prim._queue_note({}, 1):
            count += 1
        self.assertEqual(count, 128)
        self.assertEqual(prim.note_bytes, 128*64)


class NotificationRecovery(unittest.TestCase):
    def prim(self):
        prim = Prim.__new__(Prim)
        prim.notes = queue.Queue(maxsize=128)
        prim.note_lock = threading.Lock()
        prim.note_bytes = 0
        prim.recover_notifications = True
        prim.note_gap = None
        prim.handler = lambda *_: None
        return prim

    def test_overflow_collapses_waiting_notifications_into_one_local_gap(self):
        prim = self.prim()
        msg = {"method": "event", "params": event(2, "obsolete")}
        for _ in range(129):
            self.assertTrue(prim._queue_note(msg, 64), "recovering extensions must not disconnect")
        self.assertEqual(prim.notes.qsize(), 1)
        marker, charge = prim.notes.get_nowait()
        self.assertEqual(marker["method"], "missed")
        self.assertEqual(marker["params"]["reason"], "notification_backlog")
        self.assertEqual(prim.note_bytes, 64)
        for _ in range(100):
            self.assertTrue(prim._queue_note(msg, 64))
        self.assertTrue(prim.notes.empty(), "one pending gap replaces the dropped prefix")
        prim._note(marker, charge)
        self.assertEqual(prim.note_bytes, 0)

    def test_failed_recovery_disconnects_instead_of_continuing_with_a_divergent_cache(self):
        prim = self.prim()
        class Socket:
            closed = False
            def shutdown(self, _): self.closed = True
        prim.sock = Socket()
        def failed(*_): raise PrimError("snapshot overloaded", "overloaded")
        prim.handler = failed
        prim._queue_note({}, NOTIFICATION_BYTES)
        self.assertFalse(prim._note(*prim.notes.get_nowait()))
        self.assertTrue(prim.sock.closed)
        self.assertEqual(prim.note_bytes, 0)

    def test_snapshot_fences_dropped_events_but_retains_events_during_recovery(self):
        prim = self.prim()
        ext = Extension.__new__(Extension)
        ext.cache_lock = threading.RLock()
        ext.listeners = []
        ext._load(snapshot(1, "old"))
        def snapshot_call(method):
            self.assertIsNone(prim.note_gap, "recovery must buffer new deltas before requesting its snapshot")
            prim._queue_note({"method": "event", "params": event(11, "after snapshot")}, 64)
            return snapshot(10, "snapshot")
        ext.prim = type("Core", (), {"call": staticmethod(snapshot_call)})()
        prim.handler = ext._handle
        prim._queue_note({"method": "event", "params": event(2, "in flight")}, 2*1024*1024)
        active = prim.notes.get_nowait()
        prim._queue_note({"method": "event", "params": event(3, "dropped")}, 3*1024*1024)
        self.assertLessEqual(prim.note_bytes, NOTIFICATION_BYTES)
        prim._note(*active)
        prim._note(*prim.notes.get_nowait())
        self.assertEqual(ext.seq, 10)
        prim._note(*prim.notes.get_nowait())
        self.assertEqual(ext.seq, 11)
        self.assertEqual(ext.value("doc/test/value"), "after snapshot")
        self.assertEqual(prim.note_bytes, 0)


class SnapshotFence(unittest.TestCase):
    def extension(self):
        ext = Extension.__new__(Extension)
        ext.cache_lock = threading.RLock()
        ext.listeners = []
        ext._load(snapshot(1, "old"))
        return ext

    def test_queued_pre_snapshot_event_cannot_regress_cache(self):
        ext = self.extension()

        class Prim:
            def call(self, method):
                return snapshot(3, "current")

        ext.prim = Prim()
        ext._handle("missed", {})
        ext._handle("event", event(2, "stale"))
        self.assertEqual(ext.value("doc/test/value"), "current")
        ext._handle("event", event(4, "new"))
        self.assertEqual(ext.value("doc/test/value"), "new")
        self.assertEqual(ext.seq, 4)

    def test_listeners_do_not_hold_cache_lock_across_extension_code(self):
        ext = self.extension()

        class Prim:
            def call(self, method):
                return snapshot(3, "fresh")

        ext.prim = Prim()

        def listener(kind, data):
            thread = threading.Thread(target=ext.fresh)
            thread.start()
            thread.join(1)
            self.assertFalse(thread.is_alive(), "a listener must not block command-thread refreshes")

        ext.listeners.append(listener)
        ext._handle("event", event(2, "event"))
        self.assertEqual(ext.seq, 3)

    def test_command_thread_snapshot_is_serialized_with_notifications(self):
        ext = self.extension()
        requested = threading.Event()
        release = threading.Event()
        attempted = threading.Event()

        class Prim:
            def call(self, method):
                requested.set()
                if not release.wait(2):
                    raise AssertionError("snapshot was not released")
                return snapshot(2, "snapshot")

        ext.prim = Prim()
        fresh = threading.Thread(target=ext.fresh)
        fresh.start()
        self.assertTrue(requested.wait(2))

        def notify():
            attempted.set()
            ext._handle("event", event(3, "new"))

        notification = threading.Thread(target=notify)
        notification.start()
        self.assertTrue(attempted.wait(2))
        release.set()
        fresh.join(2)
        notification.join(2)
        self.assertFalse(fresh.is_alive())
        self.assertFalse(notification.is_alive())
        self.assertEqual(ext.value("doc/test/value"), "new")
        self.assertEqual(ext.seq, 3)


class PartialResources(unittest.TestCase):
    def test_partial_resource_events_merge_onto_the_cached_record(self):
        ext = SnapshotFence.extension(self)
        ext._handle("event", {"epoch": "boot", "seq": 2, "type": "resource", "data": {"id": "r1", "argv": ["pi"], "status": {"state": "idle"}}})
        ext._handle("event", {"epoch": "boot", "seq": 3, "type": "resource", "partial": True, "data": {"id": "r1", "status": {"state": "working"}}})
        self.assertEqual(ext.resources["r1"], {"id": "r1", "argv": ["pi"], "status": {"state": "working"}})

    def test_partial_event_without_a_base_reloads_the_snapshot(self):
        ext = SnapshotFence.extension(self)

        class Prim:
            calls = 0

            def call(self, method):
                Prim.calls += 1
                return dict(snapshot(5, "x"), resources=[{"id": "r9", "argv": ["sh"], "status": None}])

        ext.prim = Prim()
        ext._handle("event", {"epoch": "boot", "seq": 2, "type": "resource", "partial": True, "data": {"id": "r9", "status": "busy"}})
        self.assertEqual(Prim.calls, 1)
        self.assertEqual(ext.resources["r9"]["argv"], ["sh"])


class FakeCore:
    """workspace.apply with versions and compare-and-set, like the core (src/store.rs)."""

    def __init__(self, entries):
        self.entries = {k: {"version": 1, "value": v} for k, v in entries.items()}
        self.applied = []
        self.interfere = None  # one-shot: a concurrent writer's (key, value) applied before the next apply

    def call(self, method, params=None):
        from prim_ext import PrimError
        if method == "workspace.snapshot":
            return {"epoch": "boot", "seq": 1, "entries": {k: dict(e) for k, e in self.entries.items()}, "resources": []}
        assert method == "workspace.apply", method
        if self.interfere:
            key, value = self.interfere
            self.interfere = None
            old = self.entries.get(key, {"version": 0})
            self.entries[key] = {"version": old["version"] + 1, "value": value}
        for op in params["ops"]:
            current = self.entries.get(op["key"], {"version": 0})["version"]
            if op.get("expect") is not None and op["expect"] != current:
                raise PrimError(f"{op['key']}: version {current}, expected {op['expect']}", "conflict")
        changes = []
        for op in params["ops"]:
            if op["op"] == "set":
                version = self.entries.get(op["key"], {"version": 0})["version"] + 1
                self.entries[op["key"]] = {"version": version, "value": op["value"]}
                changes.append({"key": op["key"], "version": version})
            else:
                self.entries.pop(op["key"], None)
                changes.append({"key": op["key"], "version": None})
        self.applied.append(params["ops"])
        return {"txn": len(self.applied), "changes": changes}


class Contributions(unittest.TestCase):
    def extension(self, entries, name="tabs"):
        core = FakeCore(entries)
        ext = Extension.__new__(Extension)
        ext.name = name
        ext.cache_lock = threading.RLock()
        ext.listeners = []
        ext.prim = core
        ext._load(core.call("workspace.snapshot"))
        return ext, core

    def test_first_contribution_removes_entries_this_extension_no_longer_declares(self):
        ext, core = self.extension({
            "menu/tabs.new": {"label": "New", "command": "tabs.open", "owner": "tabs"},
            "menu/tabs.old": {"label": "Old item", "command": "tabs.old", "owner": "tabs"},
            "binding/tabs.old": {"key": "o", "command": "tabs.old", "owner": "tabs"},
            "view/tabs.old": {"kind": "panel", "owner": "tabs"},
        })
        ext.contribute({"menu/tabs.new": {"label": "New", "command": "tabs.open"}})
        self.assertEqual(set(core.entries), {"menu/tabs.new"})
        self.assertEqual(len(core.applied), 1, "one transaction")
        self.assertTrue(all(op["op"] == "delete" and op["expect"] == 1 for op in core.applied[0]))
        self.assertEqual(set(ext.entries), {"menu/tabs.new"}, "the cache follows")

    def test_entries_of_others_and_unowned_or_other_namespaces_are_never_touched(self):
        ext, core = self.extension({
            "menu/panes.split": {"label": "Split", "command": "panes.split", "owner": "panes"},
            "binding/keys.n": {"key": "n", "command": "tabs.open", "owner": "keys"},
            "view/term-r1": {"kind": "terminal", "resource": "r1"},
            "doc/tabs/state": {"owner": "tabs", "last": 3},
            "layout/main": {"tabs": [], "owner": "tabs"},
        })
        before = {k: dict(e) for k, e in core.entries.items()}
        ext.contribute({"menu/tabs.new": {"label": "New", "command": "tabs.open"}})
        for key, e in before.items():
            self.assertEqual(core.entries[key], e, key)
        self.assertIn("menu/tabs.new", core.entries)

    def test_incremental_contributions_opt_out_and_later_calls_only_add(self):
        ext, core = self.extension({"menu/tabs.kept": {"label": "Kept", "command": "x", "owner": "tabs"}})
        ext.contribute({"menu/tabs.a": {"label": "A", "command": "a"}}, exclusive=False)
        ext.contribute({"menu/tabs.b": {"label": "B", "command": "b"}})  # not the first call: adds only
        self.assertEqual(set(core.entries), {"menu/tabs.kept", "menu/tabs.a", "menu/tabs.b"})
        # An explicit exclusive call with the full set reconciles.
        ext.contribute({"menu/tabs.a": {"label": "A", "command": "a"}, "menu/tabs.b": {"label": "B", "command": "b"}}, exclusive=True)
        self.assertEqual(set(core.entries), {"menu/tabs.a", "menu/tabs.b"})

    def test_unchanged_set_writes_nothing_and_a_conflict_refreshes_and_retries(self):
        ext, core = self.extension({"menu/tabs.new": {"label": "New", "command": "tabs.open", "owner": "tabs"}})
        ext.contribute({"menu/tabs.new": {"label": "New", "command": "tabs.open"}})
        self.assertEqual(core.applied, [])
        ext2, core2 = self.extension({"menu/tabs.old": {"label": "Old", "command": "o", "owner": "tabs"}})
        # Someone else edits the stale item just before our delete lands: we reload and retry.
        core2.interfere = ("menu/tabs.old", {"label": "Old (edited)", "command": "o", "owner": "tabs"})
        ext2.contribute({"menu/tabs.new": {"label": "New", "command": "tabs.open"}})
        self.assertEqual(set(core2.entries), {"menu/tabs.new"})
        self.assertEqual(len(core2.applied), 1, "the conflicting attempt applied nothing")


class ListenerConflicts(unittest.TestCase):
    """Soak 2 P1-A: a listener's compare-and-set conflict during missed-recovery marked tabs failed."""

    def setup(self, conflicts):
        prim = NotificationRecovery.prim(self)
        class Socket:
            closed = False
            def shutdown(self, _): self.closed = True
        prim.sock = Socket()
        core = FakeCore({"layout/main": {"view": "a"}})
        core.snapshots = 0
        apply = core.call
        busy = {"left": conflicts}
        def call(method, params=None):
            if method == "workspace.snapshot":
                core.snapshots += 1
            elif busy["left"] > 0:  # a concurrent writer lands just before each of our writes
                busy["left"] -= 1
                core.interfere = ("layout/main", {"view": "other", "n": busy["left"]})
            return apply(method, params)
        deferred = []
        transport = type("Transport", (), {"call": staticmethod(call), "defer": staticmethod(lambda msg, delay: deferred.append((msg, delay)))})()
        ext = Extension.__new__(Extension)
        ext.name = "tabs"
        ext.event_types = ["entries", "resource", "resource.removed"]
        ext.cache_lock = threading.RLock()
        ext.listeners = []
        ext.prim = transport
        ext._load(core.call("workspace.snapshot"))
        prim.handler = ext._handle
        writes = []
        @ext.on_event
        def mark(kind, data):
            # Like tabs.resource_state: rewrite layout/main from the cache with compare-and-set.
            def attempt():
                ext.apply([{"op": "set", "key": "layout/main", "value": {"view": "marked"}, "expect": ext.version("layout/main")}], "mark")
                writes.append(kind)
            ext.retrying(attempt)
        return prim, core, ext, deferred, writes

    def test_conflicts_during_local_gap_recovery_defer_instead_of_disconnecting(self):
        prim, core, ext, deferred, writes = self.setup(conflicts=5)
        prim._queue_note({}, NOTIFICATION_BYTES)  # overflow: one local gap marker
        marker = prim.notes.get_nowait()
        self.assertEqual(marker[0]["method"], "missed")
        self.assertTrue(prim._note(*marker), "a write conflict is not a failed cache recovery")
        self.assertFalse(prim.sock.closed)
        self.assertEqual(writes, [])
        self.assertEqual(len(deferred), 1, "the reaction is retried later, not dropped")
        # Later events bring the concurrent writes into the cache; the deferred retry converges.
        for _ in range(10):
            if writes:
                break
            ext._load(core.call("workspace.snapshot"))  # stands in for the entries events
            msg, delay = deferred.pop(0)
            self.assertLessEqual(delay, 7.5, "bounded backoff")
            prim._queue_note(msg, 64)
            self.assertTrue(prim._note(*prim.notes.get_nowait()))
        self.assertEqual(writes, ["missed"])
        self.assertEqual(core.entries["layout/main"]["value"], {"view": "marked"})
        self.assertEqual(deferred, [], "nothing left pending once it converged")

    def test_listener_conflicts_do_not_reload_the_snapshot_when_events_keep_the_cache(self):
        prim, core, ext, deferred, writes = self.setup(conflicts=1)
        before = core.snapshots
        prim._queue_note({"method": "event", "params": event(2, "x")}, 64)
        self.assertTrue(prim._note(*prim.notes.get_nowait()))
        self.assertEqual(core.snapshots, before, "entries events, not a multi-MB snapshot, catch the cache up")
        self.assertEqual(len(deferred), 1)

    def test_other_listeners_still_run_after_one_fails(self):
        prim, core, ext, deferred, writes = self.setup(conflicts=0)
        seen = []
        ext.listeners.insert(0, lambda *_: (_ for _ in ()).throw(ValueError("bug in a listener")))
        ext.on_event(lambda kind, data: seen.append(kind))
        prim._queue_note({"method": "event", "params": event(2, "x")}, 64)
        self.assertTrue(prim._note(*prim.notes.get_nowait()))
        self.assertEqual(seen, ["entries"])
        self.assertEqual(writes, ["entries"])


class ListenerConflictsOverASocket(unittest.TestCase):
    """The same soak sequence with real Prim threads and timers, against a scripted core: a slow
    listener lets 300 events overflow the queue (local gap), then the recovery's layout write
    conflicts 4 times with a concurrent writer. The child must stay connected and converge."""

    CHILD = r'''
import sys, time
sys.path.insert(0, sys.argv[1])
from prim_ext import Extension
ext = Extension("tabs", event_types=["entries"])
slow = [True]
@ext.on_event
def mark(kind, data):
    if slow[0]:
        slow[0] = False
        time.sleep(1.0)  # the queue overflows meanwhile
    if kind != "missed" or ext.value("layout/main") == {"marked": True}:
        return
    ext.retrying(lambda: ext.apply([{"op": "set", "key": "layout/main", "value": {"marked": True}, "expect": ext.version("layout/main")}], "mark"))
time.sleep(30)
'''

    def test_recovery_conflicts_converge_without_disconnecting(self):
        import json, os, socket, subprocess, tempfile
        d = tempfile.mkdtemp(prefix="pext", dir="/tmp")
        path = os.path.join(d, "s")
        srv = socket.socket(socket.AF_UNIX); srv.bind(path); srv.listen(1)
        ext_dir = str(pathlib.Path(__file__).resolve().parents[1] / "extensions")
        child = subprocess.Popen([sys.executable, "-c", self.CHILD, ext_dir], env={"PATH": os.environ.get("PATH", ""), "PRIM_SOCKET": path},
                                 stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        try:
            srv.settimeout(10); conn, _ = srv.accept()
            state = {"seq": 1, "version": 1, "value": {"tabs": []}, "conflicts": 4, "applied": None}
            def send(obj): conn.sendall((json.dumps(obj) + "\n").encode())
            buf = [b""]
            def readline():
                while b"\n" not in buf[0]:
                    data = conn.recv(65536)
                    if not data: return b""
                    buf[0] += data
                line, buf[0] = buf[0].split(b"\n", 1)
                return line
            def entries_event():
                state["seq"] += 1
                send({"jsonrpc": "2.0", "method": "event", "params": {"epoch": "E", "seq": state["seq"], "type": "entries", "data": {"changes": [
                    {"key": "layout/main", "version": state["version"], "value": state["value"]}]}}})
            def snap():
                return {"epoch": "E", "seq": state["seq"], "entries": {"layout/main": {"version": state["version"], "value": state["value"]}}, "resources": []}
            flooded = False
            deadline = time.time() + 25
            conn.settimeout(1)
            while time.time() < deadline and state["applied"] is None:
                try: line = readline()
                except socket.timeout: continue
                if not line: break
                m = json.loads(line)
                method, i = m.get("method"), m.get("id")
                if method == "hello": send({"jsonrpc": "2.0", "id": i, "result": {"epoch": "E", "seq": 1}})
                elif method == "workspace.snapshot":
                    send({"jsonrpc": "2.0", "id": i, "result": snap()})
                    if not flooded:
                        flooded = True
                        for _ in range(300):
                            state["version"] += 1; state["value"] = {"tabs": [], "n": state["version"]}; entries_event()
                elif method == "workspace.apply":
                    op = m["params"]["ops"][0]
                    if state["conflicts"] > 0:  # a concurrent writer lands first, its event arrives too
                        state["conflicts"] -= 1
                        state["version"] += 1; state["value"] = {"tabs": [], "other": state["conflicts"]}; entries_event()
                    if op["expect"] != state["version"]:
                        send({"jsonrpc": "2.0", "id": i, "error": {"code": -32000, "message": "layout/main moved", "data": {"kind": "conflict"}}})
                    else:
                        state["version"] += 1; state["value"] = op["value"]; state["applied"] = state["version"]
                        send({"jsonrpc": "2.0", "id": i, "result": {"txn": 1, "changes": [{"key": "layout/main", "version": state["version"]}]}})
                        entries_event()
                elif i is not None:
                    send({"jsonrpc": "2.0", "id": i, "result": {}})
            time.sleep(0.3)
            alive = child.poll() is None
        finally:
            child.kill(); out = child.communicate()[0].decode(errors="replace")
            srv.close()
            import shutil; shutil.rmtree(d, ignore_errors=True)
        self.assertTrue(flooded)
        self.assertNotIn("notification recovery failed", out)
        self.assertTrue(alive, "the extension must not exit over a write conflict:\n" + out)
        self.assertIsNotNone(state["applied"], "the deferred reaction converged:\n" + out)
        self.assertEqual(state["value"], {"marked": True})
        self.assertIn("listener write conflicted", out)


class LayoutHelpers(unittest.TestCase):
    def test_views_in_walks_splits_and_tabs(self):
        # Regression: views_in lost its recursion and return in f553c6c, which broke panes.close.
        from prim_ext import views_in
        layout = {"children": [{"view": "sidebar"}, {"active": 0, "tabs": [
            {"title": "a", "node": {"split": "h", "children": [{"view": "term-r1"}, {"view": "term-r2"}]}},
            {"title": "b", "node": {"view": "agents-r8"}}]}]}
        self.assertEqual(views_in(layout), ["sidebar", "term-r1", "term-r2", "agents-r8"])
        self.assertEqual(views_in({"view": "x"}), ["x"])


if __name__ == "__main__":
    unittest.main()
