"""Default extension glue over fake public RPC, not a real-core conformance claim."""
import copy
from pathlib import Path
import sys
import threading
import types
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
from coordination import CoordinationService, callback_context, COMMANDS
from coordination_store import MemoryStore, StoreError
from coordination_policy import PolicyError
from vqro_ext import VqroError


class FakeRPC:
    def __init__(self, ext, *, available=True):
        self.ext, self.available = ext, available
        self.store = MemoryStore()
        self.calls = []
        self.handler = lambda method, params: {"forwarded": method}
        self._command_context = threading.local()

    def call(self, method, params=None):
        params = params or {}
        self.calls.append((method, copy.deepcopy(params)))
        if method.startswith("operational.") and not self.available:
            raise StoreError("not_found", "operational API unavailable")
        if method == "operational.register":
            return {"txn": 0, "undoable": False}
        if method == "operational.snapshot":
            return {"entries": {"operational/coord/" + key: row for key, row in self.store.snapshot().items()},
                    "limits": {"bytes": self.store.byte_limit, "records": self.store.record_limit}}
        if method == "operational.get":
            return {"entry": self.store.get(params["key"].removeprefix("operational/coord/"))}
        if method == "operational.apply":
            ops = [dict(op, key=op["key"].removeprefix("operational/coord/")) for op in params["ops"]]
            result = self.store.apply(ops, control=params.get("control", False))
            for change in result["changes"]:
                change["key"] = "operational/coord/" + change["key"]
            return result
        if method == "resource.invoke":
            return {"outcome": "accepted", "data": {"receipt": {"stage": "admitted"}}}
        if method == "workspace.apply":
            for op in params["ops"]:
                current = self.ext.entries.get(op["key"], {}).get("version", 0)
                if current != op.get("expect"):
                    raise StoreError("conflict", "ordinary CAS conflict")
            for op in params["ops"]:
                if op["op"] == "delete":
                    self.ext.entries.pop(op["key"], None)
                else:
                    self.ext.entries[op["key"]] = {"version": 2, "value": op["value"]}
            return {"txn": 2, "changes": []}
        return {}


class FakeExtension:
    def __init__(self, *, available=True):
        self.entries = {"doc/principals/owner": {"version": 1, "value": "p:owner"}}
        self.resources = {"r1": {"id": "r1", "kind": "agent", "available": True, "incarnation": "i1",
                                 "data": {"agent": {"v": 0, "state": "idle", "session": {"id": "s1"}, "lifecycle": "live",
                                                     "caps": {"agent.send": {"modes": {"start": {}}, "origin": [{"as": "attributed_user"}]}}}}}}
        self.providers = {}
        self.epoch = "core-boot"
        self.cache_lock = threading.RLock()
        self.vqro = FakeRPC(self, available=available)
        self.handlers = {}
        self.listeners = []
        self.logs = []

    def command(self, name, title):
        def register(function):
            self.handlers[name] = function
            return function
        return register

    def on_event(self, function):
        self.listeners.append(function)

    def fresh(self):
        pass

    def log(self, *args):
        self.logs.append(args)


class ServiceTests(unittest.TestCase):
    def setUp(self):
        self.ext = FakeExtension()
        self.service = CoordinationService(self.ext)

    def callback(self, command, args, *, principal="p:owner", actor=None, label=None):
        return self.service.handle("command.invoke", {"name": "coord." + command, "args": args,
                                                       "caller": "client:boot:1", "author": "client:boot:1",
                                                       "actor": actor or principal, "principal": principal,
                                                       "initiator": actor or principal, "label": label, "invocation": "proof"})

    def test_registers_only_owned_coord_commands_and_one_protected_domain(self):
        self.assertEqual(set(self.ext.handlers), {"coord." + name for name in COMMANDS})
        registers = [params for method, params in self.ext.vqro.calls if method == "operational.register"]
        self.assertEqual(registers, [{"name": "coord", "bytes": 4 * 1024 * 1024}])
        self.assertFalse(any(method == "workspace.apply" for method, _ in self.ext.vqro.calls))

    def test_core_callback_principal_not_body_or_label_is_authority(self):
        window = self.callback("inbox.window", {})
        op = window["epoch"] + ":send"
        args = {"op": op, "submission": op, "to": "r1", "mode": "start", "content": [{"type": "text", "text": "hello"}], "principal": "p:owner"}
        with self.assertRaises(PolicyError) as caught:
            self.callback("inbox.send", args, principal="p:worker", label="p:owner")
        self.assertEqual(caught.exception.kind, "no_grant")
        sent = self.callback("inbox.send", args)
        saved = self.callback("inbox.get", {"delivery": sent["delivery"]})
        self.assertEqual(saved["from"]["principal"], "p:owner")
        self.assertNotIn("principal", {key: value for key, value in saved.items() if key != "from"})

    def test_missing_old_core_fields_fail_closed_instead_of_defaulting_to_owner(self):
        context = callback_context({"args": {"principal": "p:owner"}, "caller": "client:old:1"})
        self.assertEqual(context["principal"], "undeclared")
        self.assertEqual(context["kind"], "tool")

    def test_flat_delegation_context_keeps_actor_initiator_and_effective_principal(self):
        context = callback_context({"author": "client:boot:7", "caller": "client:boot:7", "actor": "A", "principal": "B", "initiator": "A", "label": "not-authority"})
        self.assertEqual((context["actor"], context["principal"], context["initiator"]), ("A", "B", "A"))

    def test_callback_proof_uses_invocation_never_on_behalf_of(self):
        context = self.ext.vqro._command_context
        context.invocation = "opaque-proof"
        self.service.rpc("workspace.apply", {"ops": []})
        method, params = self.ext.vqro.calls[-1]
        self.assertEqual(method, "workspace.apply")
        self.assertEqual(params["invocation"], "opaque-proof")
        self.assertNotIn("on_behalf_of", params)
        self.assertEqual(context.invocation, "opaque-proof")

    def test_read_only_recovery_survives_failed_registration_without_mutation(self):
        ext = FakeExtension()
        record = {"delivery": "d:old", "state": "claimed", "order": 1, "to": {"agent": "r1"}, "from": {"principal": "p:owner"}}
        ext.vqro.store.apply([{"op": "set", "key": "message/d:old", "expect": 0, "value": record}], control=True)
        before = ext.vqro.store.snapshot()
        original_call = ext.vqro.call
        def refuse(method, params=None):
            if method == "operational.register":
                raise StoreError("io", "registration not confirmed")
            return original_call(method, params)
        ext.vqro.call = refuse
        service = CoordinationService(ext)
        response = service.handle("command.invoke", {"name": "coord.inbox.get", "args": {"delivery": "d:old"}})
        self.assertEqual(response["state"], "unknown")
        self.assertTrue(response["read_only"])
        self.assertTrue(response["stale"])
        self.assertEqual(ext.vqro.store.snapshot(), before)
        self.assertFalse(any(method == "resource.invoke" for method, _ in ext.vqro.calls))

    def test_unavailable_domain_stays_visible_and_never_uses_workspace_fallback(self):
        ext = FakeExtension(available=False)
        service = CoordinationService(ext)
        self.assertEqual(service.handle("command.invoke", {"name": "coord.status", "args": {}})["state"], "unavailable")
        with self.assertRaises(VqroError):
            service.handle("command.invoke", {"name": "coord.inbox.send", "args": {}})
        self.assertFalse(any(method in ("resource.invoke", "workspace.apply") for method, _ in ext.vqro.calls))
        self.assertEqual(service.handle("recovery.command", {}), {"forwarded": "recovery.command"})


if __name__ == "__main__":
    unittest.main()
