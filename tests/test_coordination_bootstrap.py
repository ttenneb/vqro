"""H15 async lifecycle ordering; fake public RPC only, not core certification."""
import copy
from pathlib import Path
import sys
import threading
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
from coordination import CoordinationService, COMMANDS, main
from coordination_store import StoreError
from vqro_ext import VqroError
from test_coordination_service import FakeExtension


class LifecycleExtension(FakeExtension):
    def __init__(self):
        super().__init__()
        self.titles = {}
        self.seq = 17
        self.event_types = ["entries", "resource", "missed", "operational"]
        self.view_content = {"prefixes": ["test-"]}
        self.vqro.patient = True

    def command(self, name, title):
        self.titles[name] = title
        return super().command(name, title)


class BootstrapTests(unittest.TestCase):
    def make(self, *, block=None, error=None):
        ext = LifecycleExtension()
        # Saved intent predates this owner. No test starts a core or native agent.
        for delivery, state in (("d:claimed", "claimed"), ("d:held", "held")):
            record = {"delivery": delivery, "state": state, "order": 1 if state == "claimed" else 2,
                      "to": {"agent": "r1"}, "from": {"principal": "p:owner"},
                      "content": [{"type": "text", "text": "saved"}], "attempt": {"id": "old"}}
            ext.vqro.store.apply([{"op": "set", "key": "message/" + delivery, "expect": 0, "value": record}], control=True)
        entered, release = threading.Event(), threading.Event()
        timeline, registered = [], set()
        original = ext.vqro.call
        blocked = [False]

        def call(method, params=None):
            params = params or {}
            timeline.append(method)
            if method == "command.register":
                result = original(method, params)
                registered.add(params["name"])
                return result
            if method == "events.subscribe":
                self.assertTrue(ext.listeners, "listener installed before subscription")
                self.assertEqual(registered, {"coord." + command for command in COMMANDS})
            if method == block and not blocked[0]:
                blocked[0] = True
                self.assertEqual(registered, {"coord." + command for command in COMMANDS},
                                 "no mutation may precede public command registration")
                self.assertIn("events.subscribe", timeline)
                self.assertFalse(ext.vqro.patient)
                entered.set()
                if not release.wait(5):
                    raise AssertionError("test failed to release its bootstrap gate")
                if error == "durability_unconfirmed":
                    original(method, params)  # real fake-store adoption, lost durability confirmation
                    raise StoreError(error, "adopted but uncertain")
                if error:
                    raise StoreError(error, "test mutation refused")
            return original(method, params)

        ext.vqro.call = call
        service = CoordinationService(ext, defer_bootstrap=True)
        self.assertEqual(timeline, [], "deferred constructor performs no blocking operational I/O")

        def cleanup():
            release.set()
            if service._bootstrap_thread:
                service._bootstrap_thread.join(3)
                self.assertFalse(service._bootstrap_thread.is_alive())
            service.pool.shutdown(wait=True)
        self.addCleanup(cleanup)
        return ext, service, entered, release, timeline, registered

    def handle(self, service, command, args=None):
        return service.handle("command.invoke", {"name": "coord." + command, "args": args or {}})

    def assert_reads(self, service, *, phase):
        outputs, errors = {}, []
        def read():
            try:
                outputs["status"] = self.handle(service, "status")
                outputs["get"] = self.handle(service, "inbox.get", {"delivery": "d:claimed"})
                outputs["list"] = self.handle(service, "inbox.list")
                outputs["peek"] = self.handle(service, "inbox.peek")
            except Exception as error:
                errors.append(error)
        thread = threading.Thread(target=read, daemon=True)
        thread.start()
        thread.join(0.5)
        self.assertFalse(thread.is_alive(), "read-only callbacks wait behind a blocked bootstrap")
        self.assertEqual(errors, [])
        self.assertEqual(outputs["status"]["state"], "initializing")
        self.assertEqual(outputs["status"]["bootstrap"], phase)
        self.assertTrue(outputs["status"]["recovery_readable"])
        self.assertEqual(outputs["get"]["state"], "unknown")
        self.assertTrue(outputs["get"]["read_only"])
        self.assertTrue(outputs["get"]["stale"])
        for name in ("list", "peek"):
            self.assertEqual(len(outputs[name]["items"]), 2)
            self.assertEqual({row["state"] for row in outputs[name]["items"]}, {"unknown", "held"})

    def test_registration_blocks_but_registered_reads_and_recovery_stay_reachable(self):
        ext, service, entered, release, timeline, registered = self.make(block="operational.register")
        service.register()
        service.start_bootstrap()
        self.assertTrue(entered.wait(1))
        self.assert_reads(service, phase="registering")
        self.assertIsNone(service.engine)
        self.assertFalse(service.bootstrap_done.is_set())
        self.assertFalse(any(method == "resource.invoke" for method in timeline))
        self.assertFalse(any(method == "operational.apply" for method in timeline))
        release.set()
        self.assertTrue(service.bootstrap_done.wait(2))
        self.assertEqual(self.handle(service, "status")["state"], "ready")
        self.assertEqual(service.engine.permits, set())

    def test_recovery_write_blocks_after_snapshot_without_exposing_partial_engine(self):
        ext, service, entered, release, timeline, registered = self.make(block="operational.apply")
        service.register()
        service.start_bootstrap()
        self.assertTrue(entered.wait(1))
        self.assert_reads(service, phase="recovering")
        self.assertIsNone(service.engine)
        for command in ("inbox.send", "inbox.take", "scope", "control"):
            with self.assertRaises(VqroError) as caught:
                self.handle(service, command)
            self.assertEqual(caught.exception.kind, "unavailable")
        self.assertFalse(any(method == "resource.invoke" for method in timeline))
        release.set()
        self.assertTrue(service.bootstrap_done.wait(2))
        self.assertEqual(service.engine.get("d:claimed")["state"], "unknown")
        self.assertEqual(service.engine.permits, set())

    def test_failed_registration_keeps_captured_reads_without_workspace_fallback(self):
        ext, service, entered, release, timeline, registered = self.make(block="operational.register", error="io")
        before = ext.vqro.store.snapshot()
        service.register()
        service.start_bootstrap()
        self.assertTrue(entered.wait(1))
        release.set()
        self.assertTrue(service.bootstrap_done.wait(2))
        self.assertIsNone(service.engine)
        status = self.handle(service, "status")
        self.assertEqual((status["state"], status["bootstrap"]), ("unavailable", "failed"))
        self.assertEqual(self.handle(service, "inbox.get", {"delivery": "d:claimed"})["state"], "unknown")
        self.assertEqual(ext.vqro.store.snapshot(), before)
        self.assertFalse(any(method in ("workspace.apply", "resource.invoke") for method in timeline))

    def test_unconfirmed_bootstrap_never_publishes_engine_or_authorizes_input(self):
        ext, service, entered, release, timeline, registered = self.make(block="operational.apply", error="durability_unconfirmed")
        service.register()
        service.start_bootstrap()
        self.assertTrue(entered.wait(1))
        self.assert_reads(service, phase="recovering")
        release.set()
        self.assertTrue(service.bootstrap_done.wait(2))
        self.assertIsNone(service.engine)
        self.assertEqual(self.handle(service, "status")["reason"]["kind"], "durability_unconfirmed")
        self.assertEqual(self.handle(service, "inbox.get", {"delivery": "d:claimed"})["state"], "unknown")
        self.assertNotIn("resource.invoke", timeline)

    def test_public_registration_is_idempotent_and_precedes_bootstrap(self):
        ext, service, entered, release, timeline, registered = self.make()
        with self.assertRaises(VqroError):
            service.start_bootstrap()
        service.register()
        first = list(timeline)
        service.register()
        self.assertEqual(timeline, first)
        self.assertEqual(timeline[-1], "events.subscribe")
        subscribe = next(params for method, params in ext.vqro.calls if method == "events.subscribe")
        self.assertEqual(subscribe, {"from": 17, "epoch": ext.epoch, "partial_resources": True,
                                     "types": ext.event_types, "view_content": ext.view_content})
        thread = service.start_bootstrap()
        self.assertIs(service.start_bootstrap(), thread)
        self.assertTrue(service.bootstrap_done.wait(2))
        self.assertEqual(timeline.count("operational.register"), 1)

    def test_subscribe_failure_cannot_start_mutation_or_input(self):
        ext, service, entered, release, timeline, registered = self.make()
        original = ext.vqro.call
        def refuse(method, params=None):
            if method == "events.subscribe":
                raise VqroError("not subscribed", "unavailable")
            return original(method, params)
        ext.vqro.call = refuse
        with self.assertRaises(VqroError):
            service.register()
        with self.assertRaises(VqroError):
            service.start_bootstrap()
        self.assertFalse(any(method.startswith("operational.") for method in timeline))
        self.assertIsNone(service.engine)

    def test_subscription_conflict_keeps_existing_sdk_recovery_semantics(self):
        ext, service, entered, release, timeline, registered = self.make()
        original = ext.vqro.call
        def subscribed(method, params=None):
            if method == "events.subscribe":
                original(method, params)
                raise VqroError("already subscribed", "conflict")
            return original(method, params)
        ext.vqro.call = subscribed
        service.register()
        service.start_bootstrap()
        self.assertTrue(service.bootstrap_done.wait(2))
        self.assertIsNotNone(service.engine)

    def test_run_registers_before_starting_boot_and_never_reenters_sdk_run(self):
        ext, service, entered, release, timeline, registered = self.make()
        order = []
        class Finished(Exception):
            pass
        class Worker:
            def __init__(self, **kwargs):
                self.target = kwargs["target"]
            def start(self):
                self_test.assertEqual(self.target, service.work)
                order.append("worker")
        class Wait:
            def wait(self):
                order.append("wait")
                raise Finished()
        self_test = self
        def boot():
            self.assertTrue(service._public_registered)
            self.assertIn("events.subscribe", timeline)
            order.append("bootstrap")
        def contribute(_entries):
            self.assertEqual(order, ["bootstrap", "worker"])
            order.append("view")
        ext.contribute = contribute
        ext.run = lambda: self.fail("SDK.run must not duplicate registration/subscription")
        with patch.object(service, "start_bootstrap", side_effect=boot), \
                patch("coordination.threading.Thread", Worker), patch("coordination.threading.Event", Wait):
            with self.assertRaises(Finished):
                service.run()
        self.assertEqual(order, ["bootstrap", "worker", "view", "wait"])
        self.assertFalse(any(method.startswith("operational.") for method in timeline))

    def test_production_main_explicitly_defers_constructor(self):
        ext = object()
        with patch("coordination.Extension", return_value=ext), patch("coordination.CoordinationService") as service:
            main()
        service.assert_called_once_with(ext, defer_bootstrap=True)
        service.return_value.run.assert_called_once_with()

    def test_existing_immediate_constructor_remains_ready_and_preserves_observer_pool(self):
        ext = LifecycleExtension()
        service = CoordinationService(ext)
        self.addCleanup(service.pool.shutdown, wait=True)
        self.assertIsNotNone(service.engine)
        self.assertTrue(service.bootstrap_done.is_set())
        self.assertEqual(service.bootstrap_state, "ready")
        self.assertIsNotNone(service.items)
        self.assertEqual(service.pool._max_workers, 8)
        self.assertIn(service.on_event, ext.listeners)


if __name__ == "__main__":
    unittest.main()
