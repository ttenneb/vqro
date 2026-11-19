"""Protected-domain adapter contract and deterministic durable-claim fault seam."""
import copy
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
from coordination_store import MemoryStore, OperationalStore, StoreError


class CoordinationStoreTests(unittest.TestCase):
    def test_cas_is_atomic_and_delete_does_not_reuse_versions(self):
        store = MemoryStore()
        store.apply([{"op": "set", "key": "message/m", "expect": 0, "value": 1}])
        before = store.snapshot()
        with self.assertRaises(StoreError):
            store.apply([{"op": "set", "key": "message/m", "expect": 1, "value": 2},
                         {"op": "set", "key": "message/n", "expect": 99, "value": 3}])
        self.assertEqual(store.snapshot(), before)
        store.apply([{"op": "delete", "key": "message/m", "expect": 1}])
        store.apply([{"op": "set", "key": "message/m", "expect": 0, "value": 4}])
        self.assertGreater(store.get("message/m")["version"], before["message/m"]["version"])

    def test_fail_before_does_not_publish_and_fail_after_adopts_but_is_not_confirmed(self):
        store = MemoryStore()
        ops = [{"op": "set", "key": "message/m", "expect": 0, "value": {"state": "claimed"}}]
        store.fault = "before"
        with self.assertRaises(StoreError) as caught:
            store.apply(ops)
        self.assertEqual(caught.exception.kind, "io")
        self.assertIsNone(store.get("message/m"))
        store.fault = "after"
        with self.assertRaises(StoreError) as caught:
            store.apply(ops)
        self.assertEqual(caught.exception.kind, "durability_unconfirmed")
        self.assertEqual(store.get("message/m")["value"]["state"], "claimed")
        with self.assertRaises(StoreError) as caught:
            store.apply(ops)
        self.assertEqual(caught.exception.kind, "conflict")

    def test_snapshots_and_reads_are_copied(self):
        store = MemoryStore()
        store.apply([{"op": "set", "key": "m", "expect": 0, "value": {"x": []}}])
        snapshot = store.snapshot()
        snapshot["m"]["value"]["x"].append("not persisted")
        self.assertEqual(store.get("m")["value"]["x"], [])

    def test_control_reserve_survives_normal_record_limit(self):
        store = MemoryStore(record_limit=10)
        store.apply([{"op": "set", "key": str(n), "expect": 0, "value": n} for n in range(9)])
        with self.assertRaises(StoreError) as caught:
            store.apply([{"op": "set", "key": "10", "expect": 0, "value": 10}])
        self.assertEqual(caught.exception.kind, "quota")
        store.apply([{"op": "set", "key": "control", "expect": 0, "value": "pause"}], control=True)
        self.assertEqual(len(store.snapshot()), 10)

    def test_operational_adapter_only_uses_public_owned_domain(self):
        calls = []
        def call(method, params):
            calls.append((method, copy.deepcopy(params)))
            if method == "operational.register":
                return {"id": 0}
            if method == "operational.snapshot":
                return {"entries": {"operational/coord/message/m": {"version": 7, "value": "saved"}},
                        "limits": {"bytes": 2 * 1024 * 1024, "records": 1024}}
            if method == "operational.get":
                return {"entry": {"version": 7, "value": "saved"}}
            return {"id": 8, "changes": [{"key": "operational/coord/message/m", "version": 8}]}
        store = OperationalStore(call, domain="coord")
        store.register()
        self.assertEqual(store.get("message/m")["version"], 7)
        result = store.apply([{"op": "set", "key": "message/m", "expect": 7, "value": "claimed"}], control=True)
        self.assertEqual(result["changes"][0]["key"], "message/m")
        self.assertEqual(calls[-1][1]["ops"][0]["key"], "operational/coord/message/m")
        self.assertTrue(calls[-1][1]["control"])
        self.assertFalse(any(method.startswith("workspace.") for method, _ in calls))
        self.assertEqual(store.byte_limit, 2 * 1024 * 1024)

    def test_missing_core_api_never_falls_back_to_workspace(self):
        calls = []
        def unavailable(method, params):
            calls.append(method)
            raise StoreError("not_found", "not implemented")
        with self.assertRaises(StoreError):
            OperationalStore(unavailable).register()
        self.assertEqual(calls, ["operational.register"])

    def test_bad_domain_reply_fails_closed(self):
        store = OperationalStore(lambda *args: {"entries": {"operational/other/x": {"value": 1}}})
        with self.assertRaises(StoreError) as caught:
            store.snapshot()
        self.assertEqual(caught.exception.kind, "invalid")


if __name__ == "__main__":
    unittest.main()
