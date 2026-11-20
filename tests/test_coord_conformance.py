# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Part B harness self-tests, never a coordination qualification claim."""
import asyncio
from copy import deepcopy
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest

from agent_conformance.part_b.catalog import CASES, BY_ID, select_cases
from agent_conformance.part_b.native import Native
from agent_conformance.part_b.runner import run_case, validate_manifest
from agent_conformance.results import FixtureFailed, HarnessGap, NotApplicable, report


class Catalog(unittest.TestCase):
    def test_complete_part_b_inventory_and_no_silent_selection(self):
        expected = [*(f"I{i}" for i in range(1,22)), *(f"H{i}" for i in range(1,17)),
                    *(f"G{i}" for i in range(1,14)), *(f"T{i}" for i in range(1,15)),
                    *(f"L{i}" for i in range(5,12)), "S1"]
        self.assertEqual([case.id for case in CASES], expected)
        self.assertEqual(len(BY_ID), 72)
        self.assertEqual([case.id for case in select_cases(["T1", "I1"])], ["I1", "T1"])
        with self.assertRaises(ValueError): select_cases(["I0"])

    def test_manifest_requires_explicit_target_and_sufficient_baseline(self):
        for value in ({}, {"name":"coord", "extensions":"ambient"},
                      {"name":"coord", "extensions":"/private/ext", "agents":1},
                      {"name":"coord", "extensions":"/private/ext", "core_driver":["ambient"]}):
            with self.assertRaises(ValueError): validate_manifest(value)
        self.assertEqual(validate_manifest({"name":"coord", "extensions":"/private/ext"})["name"], "coord")


class Dummy:
    instances = []
    def __init__(self, root, manifest, vqro, hc):
        self.root, self.manifest = root, manifest
        self.status, self.metadata, self.evidence = {"state":"ready"}, {"self_test":True}, []
        self.assertions = 0; self.closed = False
        self.__class__.instances.append(self)
    async def start(self): pass
    async def close(self):
        self.closed = True
        if self.manifest.get("cleanup_error"): raise RuntimeError("owned fixture cleanup refused")
    def check(self, value, message):
        self.assertions += 1
        if not value: raise AssertionError(message)


class Runner(unittest.IsolatedAsyncioTestCase):
    async def test_missing_handler_is_harness_fail_before_any_service_starts(self):
        before = len(Dummy.instances)
        with tempfile.TemporaryDirectory() as root:
            row = await run_case(BY_ID["I2"], Path(root)/"case", {}, "/not/run", factory=Dummy, handlers={})
        self.assertEqual((row.status, row.component), ("fail", "harness"))
        self.assertEqual(len(Dummy.instances), before)

    async def test_declaration_excludes_whole_case_without_counting_as_pass(self):
        async def absent(ctx): raise NotApplicable("doc/agents/terminals/*/pull",None)
        with tempfile.TemporaryDirectory() as root:
            row=await run_case(BY_ID["T12"],Path(root)/"case",{},"/not/run",factory=Dummy,handlers={"T12":absent})
        self.assertEqual(row.status,"not-applicable")
        self.assertEqual(row.declarations["exclusion"],{"path":"doc/agents/terminals/*/pull","actual":None})
        document=report({},[row],[BY_ID["T12"]],part="B",inventory=CASES)
        self.assertEqual(document["summary"]["pass"],0)
        self.assertEqual(document["summary"]["not-applicable"],1)

    async def test_gap_gets_explicit_reason_not_exclusion(self):
        async def gap(ctx): raise HarnessGap("native framing fixture absent for declared profile")
        with tempfile.TemporaryDirectory() as root:
            row=await run_case(BY_ID["T12"],Path(root)/"case",{},"/not/run",factory=Dummy,handlers={"T12":gap})
        self.assertEqual((row.status,row.reason),("fail","harness_gap"))
        self.assertNotIn("exclusion",row.declarations)

    async def test_setup_or_no_assertion_cannot_certify_case(self):
        async def empty(ctx): pass
        with tempfile.TemporaryDirectory() as root:
            row = await run_case(BY_ID["I1"], Path(root)/"case", {}, "/not/run", factory=Dummy, handlers={"I1":empty})
        self.assertEqual((row.status, row.component), ("fail", "harness"))
        self.assertTrue(Dummy.instances[-1].closed)

    async def test_fixture_gap_is_never_unsupported_or_pass(self):
        async def gap(ctx): raise FixtureFailed("real durability seam absent")
        with tempfile.TemporaryDirectory() as root:
            row = await run_case(BY_ID["I3"], Path(root)/"case", {}, "/not/run", factory=Dummy, handlers={"I3":gap})
        self.assertEqual((row.status, row.component), ("fail", "fixture"))

    async def test_not_applicable_is_explicit_subcase_evidence_not_unsupported(self):
        async def probe(ctx):
            ctx.check(True, "applicable negative branch")
            ctx.evidence.append({"subcase":"entry-evidence", "applicability":"not-applicable",
                                 "declaration":{"insert":"context", "evidence":"none"}})
        with tempfile.TemporaryDirectory() as root:
            row = await run_case(BY_ID["T10"], Path(root)/"case", {}, "/not/run", factory=Dummy, handlers={"T10":probe})
        self.assertEqual(row.status, "pass")
        self.assertEqual(row.evidence[0]["applicability"], "not-applicable")
        document = report({}, [row], [BY_ID["T10"]], part="B", inventory=CASES)
        self.assertFalse(document["complete_inventory"])
        self.assertEqual(len(document["not_selected"]),71)
        self.assertEqual(document["summary"]["unsupported-declared"],0)

    async def test_cleanup_failure_overrides_pass(self):
        async def probe(ctx): ctx.check(True,"observed")
        with tempfile.TemporaryDirectory() as root:
            row = await run_case(BY_ID["I1"], Path(root)/"case", {"cleanup_error":True}, "/not/run", factory=Dummy, handlers={"I1":probe})
        self.assertEqual((row.status,row.component),("fail","cleanup"))


class Provider:
    def __init__(self):
        from agent_conformance.part_b.native import send_caps
        self.capabilities = {"agent.send":send_caps(), "agent.interrupt":{}, "agent.receipt":{},
                             "agent.respond":{"methods":["confirm"],"scopes":["session"],"background_requests":False}}
        self.registration = {}; self.handlers = {}; self.published = []
    def effect(self, op):
        def register(handler): self.handlers[op] = handler; return handler
        return register
    async def upsert(self, row): self.published.append(deepcopy(row))
    async def call(self, method, args):
        return {"entry":{"value":{"state":"claimed", "delivery":args["key"].rsplit("/",1)[-1],
                                     "attempt":{"id":"attempt", "control_epoch":"control"}}}}


class NativeOracle(unittest.IsolatedAsyncioTestCase):
    async def test_duplicate_delivery_is_not_suppressed_by_fake_oracle(self):
        with tempfile.TemporaryDirectory() as root:
            provider = Provider(); native = Native(provider,Path(root)); await native.add("a0")
            effect = SimpleNamespace(id="e1",ref="a0",incarnation="i:a0")
            args = {"v":0,"delivery":"d:one","mode":"start","content":[{"type":"text","text":"literal"}],
                    "origin":{"principal":"worker","actor":"worker","kind":"agent"}}
            await native.effect("agent.send",effect,args)
            await native.control({"verb":"point","ref":"a0","point":"idle"})
            effect.id="e2"; await native.effect("agent.send",effect,args)
            rows=[json.loads(line) for line in native.ledger.read_text().splitlines()]
            self.assertEqual(sum(row["kind"]=="invoke" for row in rows),2)
            self.assertEqual(sum(row["kind"]=="model_request" for row in rows),2)
            self.assertTrue(all(row["witness"]["entry"]["value"]["state"]=="claimed" for row in rows if row["kind"]=="invoke"))

    async def test_request_validation_precedes_native_response_and_false_is_not_cancel(self):
        with tempfile.TemporaryDirectory() as root:
            provider=Provider();native=Native(provider,Path(root));await native.add("a0")
            native.agents["a0"]["data"]["agent"]["requests"]=[{"request":"q","revision":1,"method":"confirm",
                "scope":{"session":"s1"},"status":"pending","params":{},"incomplete":False}]
            effect=SimpleNamespace(id="e",ref="a0",incarnation="i:a0")
            args={"v":0,"epoch":"e1","request":"q","revision":1,"response":{"value":"not a boolean"}}
            invalid=await native.effect("agent.respond",effect,args)
            self.assertEqual(invalid["error"]["kind"],"invalid")
            self.assertFalse(any(row["kind"]=="native_response" for row in map(json.loads,native.ledger.read_text().splitlines())))
            args["response"]={"value":False};valid=await native.effect("agent.respond",effect,args)
            self.assertEqual(valid["outcome"],"accepted")
            self.assertEqual(native.agents["a0"]["data"]["agent"]["requests"][0]["status"],"answered")
            repeated=await native.effect("agent.respond",effect,args);self.assertEqual(repeated["error"]["kind"],"stale")

    async def test_boundary_joins_only_at_fake_safe_point_and_quote_stays_claim(self):
        with tempfile.TemporaryDirectory() as root:
            provider=Provider(); native=Native(provider,Path(root));await native.add("a0",state="running")
            args={"v":0,"delivery":"d:boundary","mode":"boundary","content":[{"type":"text","text":"quoted"}],
                  "origin":{"principal":"worker","actor":"worker","kind":"agent"},
                  "quoted":{"principal":"owner","kind":"human"}}
            await native.effect("agent.send",SimpleNamespace(id="e",ref="a0",incarnation="i:a0"),args)
            self.assertFalse(any(row["kind"]=="model_request" for row in map(json.loads,native.ledger.read_text().splitlines())))
            await native.control({"verb":"point","ref":"a0","point":"tool"})
            rows=list(map(json.loads,native.ledger.read_text().splitlines())); model=next(row for row in rows if row["kind"]=="model_request")
            self.assertEqual(model["sender"]["principal"],"worker");self.assertEqual(model["role"],"user")
            self.assertEqual(model["quoted_claim"]["principal"],"owner")


if __name__ == "__main__":
    unittest.main()
