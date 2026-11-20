"""Private fake-RPC control blast radius and threaded dispatcher checks."""
import copy
import threading
import unittest
import test_coordination_engine as fixtures
import test_coordination_service as service_fixtures

from coordination import CoordinationService
from coordination_policy import PolicyError


class SharedHostTests(unittest.TestCase):
    def setUp(self):
        self.f = fixtures.EngineTests()
        self.f.setUp()
        f = self.f
        f.resources["r2"] = fixtures.resource("r2")
        f.resources["r100"] = {"id": "r100", "kind": "terminal", "provider": "fake", "available": True, "incarnation": "host1"}
        for rid in ("r1", "r2"):
            f.resources[rid]["data"]["agent"]["host"] = {"resource": "r100", "incarnation": "host1", "owns_lifecycle": False}
        f.engine.providers = lambda: {"fake": {"capabilities": {"terminal.kill": {}}}}
        f.resume()

    def preview(self, context=fixtures.OWNER):
        return self.f.engine.control({"target": "r1", "action": "inspect"}, context)["confirm_host"]

    def stop(self, confirmation, context=fixtures.OWNER):
        return self.f.engine.control({"op": self.f.op(), "target": "r1", "action": "stop",
                                     "args": {"how": "host_kill"}, "confirm_host": confirmation}, context)

    def test_preview_is_exact_and_host_string_alone_is_not_confirmation(self):
        f = self.f
        preview = self.preview()
        self.assertEqual(preview, {"resource": "r100", "incarnation": "host1", "endpoints": ["r1", "r2"]})
        with self.assertRaises(PolicyError) as failure:
            self.stop("r100")
        self.assertEqual(failure.exception.kind, "conflict")
        self.assertEqual(f.engine.permits, {"r1", "r2"})
        self.assertEqual(f.ledger, [])

    def test_stop_right_on_one_endpoint_cannot_kill_another_endpoints_host(self):
        f = self.f
        f.config["doc/agents/grants/stop-one"] = {"version": 1, "value": {"who": "p:worker", "can": ["stop"], "on": "agent:r1"}}
        with self.assertRaises(PolicyError) as failure:
            self.stop(self.preview(fixtures.WORKER), fixtures.WORKER)
        self.assertEqual(failure.exception.kind, "no_grant")
        self.assertEqual(f.engine.permits, {"r1", "r2"})
        self.assertEqual(f.ledger, [])

    def test_confirmed_host_kill_closes_all_gates_but_never_invents_endpoint_end(self):
        f = self.f
        result = self.stop(self.preview())
        self.assertEqual(result["state"], "accepted")
        self.assertEqual(f.engine.permits, set())
        self.assertEqual([(rid, op) for rid, op, _ in f.ledger], [("r100", "terminal.kill")])
        self.assertEqual(result["native"], {"outcome": "accepted", "data": {"receipt": {"stage": "admitted"}}})
        self.assertTrue(all(stone["lifecycle"] != "ended" for stone in f.engine.tombstones().values()))

    def test_new_reported_endpoint_after_confirmation_rejects_before_gate_changes(self):
        f = self.f
        preview = self.preview()
        f.resources["r3"] = copy.deepcopy(f.resources["r2"])
        f.resources["r3"]["id"] = "r3"
        with self.assertRaises(PolicyError) as failure:
            self.stop(preview)
        self.assertEqual(failure.exception.kind, "conflict")
        self.assertEqual(f.engine.permits, {"r1", "r2"})
        self.assertEqual(f.ledger, [])

    def test_host_snapshot_rechecked_after_durable_intent_before_invoke(self):
        f = self.f
        preview = self.preview()
        original = f.store.apply
        def apply(ops, **kwargs):
            reply = original(ops, **kwargs)
            if any(op.get("value", {}).get("command") == "coord.control" and op["value"].get("state") == "claimed" for op in ops):
                f.resources["r3"] = copy.deepcopy(f.resources["r2"])
                f.resources["r3"]["id"] = "r3"
            return reply
        f.store.apply = apply
        result = self.stop(preview)
        self.assertEqual(result["state"], "partial")
        self.assertIsNone(result["native"])
        self.assertEqual(result["coordination"]["error"]["kind"], "stale")
        self.assertEqual(f.ledger, [])
        self.assertEqual(f.engine.permits, set())

    def test_grant_rechecked_after_control_intent_and_local_refusal_is_not_native(self):
        f = self.f
        grant = "doc/agents/grants/interrupt"
        f.config[grant] = {"version": 1, "value": {"who": "p:worker", "can": ["interrupt"], "on": "agent:r1"}}
        original = f.store.apply
        def apply(ops, **kwargs):
            reply = original(ops, **kwargs)
            if any(op.get("value", {}).get("command") == "coord.control" and op["value"].get("state") == "claimed" for op in ops):
                f.config.pop(grant, None)
            return reply
        f.store.apply = apply
        result = f.engine.control({"op": f.op(), "target": "r1", "action": "interrupt"}, fixtures.WORKER)
        self.assertEqual(result["state"], "rejected")
        self.assertIsNone(result["native"])
        self.assertEqual(result["coordination"]["error"]["kind"], "no_grant")
        self.assertEqual(f.ledger, [])

    def test_unknown_stop_strategy_never_becomes_graceful_stop(self):
        f = self.f
        f.resources["r1"]["data"]["agent"]["caps"]["agent.stop"] = {}
        with self.assertRaises(PolicyError) as failure:
            f.engine.control({"op": f.op(), "target": "r1", "action": "stop", "args": {"how": "future-stop"}}, fixtures.OWNER)
        self.assertEqual(failure.exception.kind, "unsupported")
        self.assertEqual(f.engine.permits, {"r1", "r2"})
        self.assertEqual(f.ledger, [])


class DispatcherPoolTests(unittest.TestCase):
    def setUp(self):
        self.ext = service_fixtures.FakeExtension()
        self.service = CoordinationService(self.ext)
        self.releases = []
        self.number = 0

    def tearDown(self):
        for event in self.releases:
            event.set()
        self.service.pool.shutdown(wait=True, cancel_futures=True)

    def op(self):
        self.number += 1
        return self.service.engine.window()["epoch"] + ":" + str(self.number)

    def prepare(self, count):
        template = self.ext.resources["r1"]
        self.ext.resources = {"r" + str(n): dict(copy.deepcopy(template), id="r" + str(n)) for n in range(1, count + 1)}
        for rid in self.ext.resources:
            op = self.op()
            self.service.engine.send({"op": op, "submission": op, "to": rid, "mode": "start", "content": [{"type": "text", "text": "probe"}]}, fixtures.OWNER)
        engine = self.service.engine
        engine.scope({"op": self.op(), "action": "resume", "scope": "workspace", "expect": engine.scope_preview("workspace")["expect"]}, fixtures.OWNER)

    def test_slow_endpoint_does_not_block_independent_native_dispatch(self):
        self.prepare(2)
        release, first, second = threading.Event(), threading.Event(), threading.Event()
        self.releases.append(release)
        original = self.ext.vqro.call
        def call(method, params=None):
            if method == "resource.invoke":
                if params["resource"] == "r1":
                    first.set()
                    release.wait(2)
                else:
                    second.set()
            return original(method, params)
        self.ext.vqro.call = call
        self.service.schedule()
        self.assertTrue(first.wait(1))
        self.assertTrue(second.wait(1))
        self.assertFalse(release.is_set())

    def test_at_most_eight_independent_native_attempts_are_admitted(self):
        self.prepare(12)
        release = threading.Event()
        self.releases.append(release)
        original = self.ext.vqro.call
        def call(method, params=None):
            if method == "resource.invoke":
                release.wait(2)
            return original(method, params)
        self.ext.vqro.call = call
        self.service.schedule()
        with self.service.job_lock:
            self.assertEqual(len(self.service.jobs), 8)
        self.assertEqual(len(self.service.engine.native_inflight), 8)


if __name__ == "__main__":
    unittest.main()
