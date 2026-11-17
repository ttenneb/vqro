"""Snapshot fencing regressions for the stdlib extension helper (no running core)."""
import pathlib
import queue
import sys
import threading
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "extensions"))
from prim_ext import Extension, Prim, NOTIFICATION_BYTES


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
