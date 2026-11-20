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

from agent_conformance.part_b.catalog import CASES, BY_ID, select_cases, cases_from_spec
from agent_conformance.part_b.native import Native
from agent_conformance.part_b.runner import run_case, validate_manifest, inventory_complete
from agent_conformance.results import FixtureFailed, HarnessGap, NotApplicable, report
from unittest.mock import patch


class Catalog(unittest.TestCase):
    def test_known_case_titles_and_no_silent_selection(self):
        expected = [*(f"I{i}" for i in range(1,24)), *(f"H{i}" for i in range(1,17)),
                    *(f"G{i}" for i in range(1,14)), *(f"T{i}" for i in range(1,15)),
                    *(f"L{i}" for i in range(5,12)), "S1"]
        self.assertEqual([case.id for case in CASES], expected)
        self.assertEqual(len(BY_ID), 74)
        self.assertEqual([case.id for case in select_cases(["T1", "I1"])], ["I1", "T1"])
        with self.assertRaises(ValueError): select_cases(["I0"])

    def test_spec_discovers_new_groups_ranges_and_does_not_count_prototype_or_session_ids(self):
        text="# Part B:\n**I. Inbox.** I1 session S2; prototype P2.\n**Q. Requests.** Q1–Q4.\n**S. Scale.** combined.\n## Running\n"
        rows=cases_from_spec(text)
        self.assertEqual([row.id for row in rows],["I1","Q1","Q2","Q3","Q4","S1"])
        self.assertEqual([row.id for row in select_cases(["Q4"],rows)],["Q4"])
        with self.assertRaises(ValueError): cases_from_spec("# Part B:\n## Running")

    def test_inventory_needs_captured_normative_ids_not_a_literal_catalog_count(self):
        rows=[SimpleNamespace(case=case.id) for case in CASES]
        normative=[case.id for case in CASES]
        self.assertTrue(inventory_complete(rows,normative))
        self.assertFalse(inventory_complete(rows,[]))
        self.assertFalse(inventory_complete(rows,normative+["I999"]))
        self.assertFalse(inventory_complete(rows+[rows[0]],normative))

    def test_assertions_always_retain_observed_result_before_failure(self):
        from agent_conformance.part_b.context import PublicCoordination
        from agent_conformance.results import CheckFailed
        context=object.__new__(PublicCoordination);context.evidence=[];context.assertions=0
        context.check(True,"observed actual witness")
        with self.assertRaises(CheckFailed): context.check(False,"missing semantic witness")
        self.assertEqual([item["passed"] for item in context.evidence],[True,False])

    def test_manifest_requires_explicit_target_and_sufficient_baseline(self):
        for value in ({}, {"name":"coord", "extensions":"ambient"},
                      {"name":"coord", "extensions":"/private/ext", "agents":1},
                      {"name":"coord", "extensions":"/private/ext", "core_driver":["ambient"]}):
            with self.assertRaises(ValueError): validate_manifest(value)
        self.assertEqual(validate_manifest({"name":"coord", "extensions":"/private/ext"})["name"], "coord")


class SaturationOrder(unittest.IsolatedAsyncioTestCase):
    async def test_probe_grant_precedes_fill_but_first_input_follows_it(self):
        from agent_conformance.part_b.scale_cases import saturate_domain
        from agent_conformance.wire import RpcError
        test = self

        class Sub:
            filled = False
            inputs = []

            async def grant(self, *args):
                test.assertFalse(self.filled, "configuration cannot be added after saturation")

            async def connect(self, name):
                return name

            async def send(self, *, peer, **fields):
                self.filled = True
                self.inputs.append(peer)
                raise RpcError({"message": "domain quota", "data": {"kind": "quota"}})

        sub = Sub()
        ctx = SimpleNamespace(evidence=[])
        accepted, probe, refusal = await saturate_domain(ctx, sub, [{"id": "r1"}], senders=1, size=16)
        self.assertEqual(sub.inputs, ["sat-s0"] * 5 + ["sat-probe"])
        self.assertEqual((accepted, probe, refusal["kind"]), ({"r1": []}, "sat-probe", "quota"))


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
        self.assertEqual(len(document["not_selected"]),73)
        self.assertEqual(document["summary"]["unsupported-declared"],0)

    async def test_stalled_cleanup_is_bounded_and_never_passes(self):
        class Stalled(Dummy):
            async def close(self): await asyncio.Future()
        async def probe(ctx): ctx.check(True,"observed")
        with tempfile.TemporaryDirectory() as root, patch("agent_conformance.part_b.runner.CLEANUP_TIMEOUT",.02,create=True):
            row=await asyncio.wait_for(run_case(BY_ID["I1"],Path(root)/"case",{},"/not/run",factory=Stalled,handlers={"I1":probe}),.3)
        self.assertEqual((row.status,row.component),("fail","cleanup"))

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

    async def test_publish_receipt_replays_only_the_stored_native_receipt(self):
        with tempfile.TemporaryDirectory() as root:
            provider=Provider();native=Native(provider,Path(root));await native.add("a0")
            effect=SimpleNamespace(id="e1",ref="a0",incarnation="i:a0")
            await native.effect("agent.send",effect,{"v":0,"delivery":"d:late","mode":"start",
                "content":[{"type":"text","text":"literal"}],"origin":{"principal":"worker","actor":"worker","kind":"agent"}})
            ledger=lambda:[json.loads(line) for line in native.ledger.read_text().splitlines()]
            inserted=next(row["seq"] for row in ledger() if row["kind"]=="model_request" and row["delivery"]=="d:late")
            journal=native.journal("a0")
            first=await native.control({"verb":"publish-receipt","ref":"a0","delivery":"d:late"})
            self.assertEqual({k:first[k] for k in ("receipt","model_request_seq")},{"receipt":{"delivery":"d:late","stage":"inserted"},"model_request_seq":inserted})
            row=json.loads(journal["rows"][-1][1])
            self.assertEqual((row["op"],row["kind"],row["id"],row["rev"],row["value"]),
                             ("put","receipt","d:late",first["rev"],{"delivery":"d:late","stage":"inserted"}))
            second=await native.control({"verb":"publish-receipt","ref":"a0","delivery":"d:late"})
            self.assertGreater(second["rev"],first["rev"])
            encoded=lambda entity:len(json.dumps(entity,ensure_ascii=False,separators=(",",":")).encode())+1
            self.assertEqual(journal["entity_bytes"],sum(map(encoded,journal["entities"].values())),"republish keeps byte accounting exact")
            self.assertEqual(sum(1 for entity in journal["entities"].values() if entity["kind"]=="receipt"),1)
            seq=journal["seq"]
            with self.assertRaises(ValueError):
                await native.control({"verb":"publish-receipt","ref":"a0","delivery":"d:never-sent"})
            self.assertEqual(journal["seq"],seq,"an unknown delivery publishes nothing")
            self.assertEqual([row["delivery"] for row in ledger() if row["kind"]=="receipt_published"],["d:late","d:late"])
            self.assertEqual(sum(row["kind"]=="model_request" for row in ledger()),1,"publishing never sends or inserts again")

    async def test_evicted_entity_remove_revision_exceeds_its_last_revision(self):
        with tempfile.TemporaryDirectory() as root:
            provider=Provider();native=Native(provider,Path(root));await native.add("a0")
            native.publish_entity("a0",{"op":"put","kind":"receipt","id":"d:x","rev":3,"value":{"delivery":"d:x","stage":"read"}})
            for index in range(600): native.item("a0",{"role":"user","content":[],"final":True})
            removes=[json.loads(data) for _,data in native.journal("a0")["rows"] if json.loads(data)["op"]=="remove"]
            receipt=[row for row in removes if row["id"]=="d:x"]
            self.assertEqual(len(receipt),1);self.assertGreater(receipt[0]["rev"],3)

    async def test_receipt_and_message_with_the_same_id_are_distinct_entities(self):
        from agent_conformance.wire import ItemReducer
        with tempfile.TemporaryDirectory() as root:
            native=Native(Provider(),Path(root));await native.add("a0")
            native.insert("a0",{"delivery":"m1","content":[{"type":"text","text":"real inserted payload"}]})
            await native.control({"verb":"publish-receipt","ref":"a0","delivery":"m1"})
            journal=native.journal("a0")
            live=ItemReducer();live.accept({"op":"snapshot","phase":"begin","epoch":journal["epoch"],"coverage":"tail"})
            live.accept({"op":"snapshot","phase":"end","seq":0})
            for _,data in journal["rows"]: live.accept(json.loads(data))
            fresh=ItemReducer();fresh.accept({"op":"snapshot","phase":"begin","epoch":journal["epoch"],"coverage":"tail"})
            for entity in journal["entities"].values(): fresh.accept(entity)
            fresh.accept({"op":"snapshot","phase":"end","seq":journal["seq"]})
            self.assertEqual(sorted(live.entities),sorted(fresh.entities))
            self.assertEqual(len(journal["entities"]),2,"message m1 and receipt m1 are both retained")

    async def test_republish_after_eviction_exceeds_the_remove_revision(self):
        with tempfile.TemporaryDirectory() as root:
            native=Native(Provider(),Path(root));await native.add("a0")
            native.insert("a0",{"delivery":"d:repeat","content":[]})
            for _ in range(2): await native.control({"verb":"publish-receipt","ref":"a0","delivery":"d:repeat"})
            for _ in range(600): native.item("a0",{"role":"user","content":[],"final":True})
            rows=[json.loads(data) for _,data in native.journal("a0")["rows"]]
            removed=next(row for row in rows if row["op"]=="remove" and row["kind"]=="receipt" and row["id"]=="d:repeat")
            again=await native.control({"verb":"publish-receipt","ref":"a0","delivery":"d:repeat"})
            self.assertGreater(again["rev"],removed["rev"],"revisions strictly increase across eviction and re-creation")

    async def test_reused_delivery_after_native_receipt_turnover_keeps_revisions_increasing(self):
        # Review R2: the native keeps 256 stored receipts; a delivery inserted again after that turnover must not
        # restart its still-retained typed receipt at a lower revision.
        with tempfile.TemporaryDirectory() as root:
            native=Native(Provider(),Path(root));await native.add("a0")
            native.insert("a0",{"delivery":"d:repeat","content":[]})
            for _ in range(2): await native.control({"verb":"publish-receipt","ref":"a0","delivery":"d:repeat"})
            retained=native.journal("a0")["entities"][("receipt","d:repeat")]["rev"]
            for index in range(257): native.insert("a0",{"delivery":f"d:{index}","content":[]})
            await native.control({"verb":"publish-receipt","ref":"a0","delivery":"d:256"})
            self.assertNotIn("d:repeat",native.receipts["a0"],"the native receipt store turned over")
            self.assertIn(("receipt","d:repeat"),native.journal("a0")["entities"],"the typed receipt is still retained")
            native.insert("a0",{"delivery":"d:repeat","content":[]})
            again=await native.control({"verb":"publish-receipt","ref":"a0","delivery":"d:repeat"})
            self.assertGreater(again["rev"],retained)

    async def test_control_error_reply_keeps_the_request_id(self):
        from agent_conformance.core import Native as Client
        from agent_conformance.results import FixtureFailed
        with tempfile.TemporaryDirectory() as root:
            provider=Provider();native=Native(provider,Path(root))
            server=await asyncio.start_unix_server(native.connection,str(Path(root)/"n.sock"))
            try:
                with self.assertRaises(FixtureFailed) as caught:
                    await Client(Path(root)/"n.sock",Path(root)/"native.jsonl").call("publish-receipt",ref="a0",delivery="d:none")
                self.assertIn("no stored native receipt",str(caught.exception))
            finally:
                server.close();await server.wait_closed()

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


class OrderingCheckerRegressions(unittest.TestCase):
    """prim-agent-ux's review counterexamples (review-part-b-ordering.md R2-R5) must fail the ordering checkers."""

    def setUp(self):
        from agent_conformance.part_b import ordering_status_cases as module
        self.m = module

    def test_i17_crossed_valid_receipt_envelopes_fail(self):
        alice = {"result": {"receipt": {"delivery": "d:alice", "stage": "inserted"}}}
        bob = {"result": {"receipt": {"delivery": "d:bob", "stage": "inserted"}}}
        self.assertTrue(self.m.receipts_isolated([("d:alice", alice), ("d:bob", bob)]))
        self.assertFalse(self.m.receipts_isolated([("d:alice", bob), ("d:bob", alice)]))
        self.assertFalse(self.m.receipts_isolated([("d:alice", {"result": {"native": {"outcome": "accepted"}}}), ("d:bob", bob)]))
        acked = {"result": {"receipt": {"delivery": "d:alice", "stage": "admitted"}}}
        self.assertFalse(self.m.receipts_isolated([("d:alice", acked), ("d:bob", bob)]), "the admitted ACK is not the insertion witness")

    def test_i22_missing_operator_view_fails(self):
        self.assertFalse(self.m.operator_view_names([{}, None], "d:succ", "d:pred"))
        self.assertFalse(self.m.operator_view_names([{"items": [{"delivery": "d:pred", "state": "unknown"}]}], "d:succ", "d:pred"))
        named = {"items": [{"delivery": "d:succ", "reason": "ordering_unknown", "predecessor": "d:pred"}]}
        self.assertTrue(self.m.operator_view_names([{}, named], "d:succ", "d:pred"))
        legend = {"legend": ["ordering_unknown"], "items": [{"delivery": "d:succ", "reason": "paused", "predecessor": "d:pred"}]}
        self.assertFalse(self.m.operator_view_names([legend], "d:succ", "d:pred"))

    def test_l10_extra_frozen_target_fails(self):
        frozen = {"targets": ["r-mover"], "receipt": "x"}
        self.assertTrue(self.m.frozen_set_kept(frozen, {"targets": ["r-mover"], "receipt": "x"}))
        self.assertFalse(self.m.frozen_set_kept(frozen, {"targets": ["r-mover", "r-unselected"], "receipt": "x"}))
        self.assertFalse(self.m.frozen_set_kept(frozen, {"targets": [], "receipt": "x"}))
        self.assertFalse(self.m.frozen_set_kept(frozen, {"targets": ["r-mover"], "receipt": "y"}))

    def test_t13_public_effect_events_are_seen(self):
        effect = {"type": "effect", "data": {"op": "agent.send", "outcome": "accepted"}}
        self.assertEqual(self.m.status_effects([effect]), ["effect"])
        self.assertEqual(self.m.status_effects([{"type": "view.content", "data": {"view": "sidebar"}}]), [])
        self.assertEqual(self.m.status_effects([{"type": "view.content", "data": {"view": "agents.terminal_status"}}]),
                         ["view.content:agents.terminal_status"])


class HandoverCheckerRegressions(unittest.TestCase):
    """prim-agent-ux's T9 review (review-part-b-t9.md R2): the recorded plan and op lookup are graded exactly."""

    def setUp(self):
        from agent_conformance.part_b import endpoint_handover_cases as module
        self.m = module
        self.src, self.dst = {"agent": "r1", "incarnation": "i:a"}, {"agent": "r2", "incarnation": "i:b"}
        self.plan = {"state": "pending", "from": self.src, "to": dict(self.dst), "deliveries": ["d:1", "d:2"],
                     "stages": [{"kind": "names", "state": "pending"}]}

    def test_lookup_unknown_or_wrong_plan_fails(self):
        self.assertTrue(self.m.lookup_names_plan({"plan": "handover:x", "historical": True, "state": "held"}, "handover:x"))
        self.assertFalse(self.m.lookup_names_plan({"state": "unknown", "plan": "wrong-plan"}, "handover:x"))
        self.assertFalse(self.m.lookup_names_plan({"plan": "handover:x", "state": "unknown"}, "handover:x"))

    def test_plan_identity_and_delivery_set_are_exact(self):
        ok = lambda plan: self.m.plan_matches(plan, self.src, self.dst, ["d:2", "d:1"], state="pending", stage="pending")
        self.assertTrue(ok(self.plan))
        self.assertFalse(ok(dict(self.plan, **{"from": {"agent": "r1", "incarnation": "i:other"}})))
        self.assertFalse(ok(dict(self.plan, deliveries=["d:1"])))
        self.assertFalse(ok(dict(self.plan, deliveries=["d:1", "d:2", "d:3"])))
        self.assertFalse(ok(dict(self.plan, stages=[{"kind": "names", "state": "accepted"}])))
