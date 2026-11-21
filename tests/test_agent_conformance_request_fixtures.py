"""Whole-branch negatives for the C6/R2/R6 request-fixture branches: a fake adapter+runtime that breaks one rule
at a time must fail the branch (no false pass); the conforming fake passes."""
import asyncio
import json
import unittest
from unittest import mock

import agent_conformance.cases as cases  # load the shared registry first
from agent_conformance import request_fixture_cases as branches
from agent_conformance.results import CheckFailed

DECLARATION = {"ack": "dispatched", "background_requests": True, "methods": ["permission", "form"], "scopes": ["session", "run"]}


class Native:
    def __init__(self, owner):
        self.owner, self.rows = owner, []

    def add(self, kind, **fields):
        self.rows.append({"seq": len(self.rows) + 1, "kind": kind, **fields})

    def mark(self):
        return len(self.rows)

    def records(self, kind=None, *, after=0):
        return [row for row in self.rows if row["seq"] > after and (kind is None or row["kind"] == kind)]

    def writes(self, after=0):
        return [row for row in self.records("command", after=after) if row.get("effectful")]

    async def wait(self, predicate, timeout=2):
        if not predicate(self.rows):
            raise CheckFailed("native ledger condition did not occur")

    async def call(self, verb, **params):
        return await self.owner.native_verb(verb, **params)


class Fake:
    """Models the scripted Codex fixture plus a conforming adapter, unless `fault` names one rule to break."""
    BUDGET = 8

    def __init__(self, fault=None):
        self.fault, self.evidence, self.serial = fault, [], 0
        self.native = Native(self)
        self.requests = {}  # adapter key -> entity
        self.state = "idle"

    async def ensure_session(self):
        return await self.facts()

    async def facts(self):
        summaries = [{"request": key, "revision": 1, "method": entity["method"], "title": entity["title"],
                      "status": entity["status"]} for key, entity in self.requests.items()]
        if self.fault == "oversized_facts":
            summaries.append({"padding": "p" * (70 << 10)})
        if self.fault == "silent_title_cut" and summaries:
            summaries[0]["title"] = summaries[0]["title"][:3]
        if self.fault == "summary_omits_title" and summaries:
            summaries[0].pop("title")
        facts = {"state": self.state, "requests": summaries}
        if self.fault == "oversized_after_refusal" and self.serial > self.BUDGET:
            facts["overflow_padding"] = "p" * (70 << 10)  # summaries stay valid; only the post-refusal facts exceed 64 KiB
        return facts

    async def wait_facts(self, predicate, timeout=2):
        facts = await self.facts()
        if not predicate(facts):
            raise CheckFailed("public agent facts did not reach the required state")
        return facts

    async def make_request(self, ctx, method=None, **options):
        method = method or "permission"
        self.serial += 1
        native_id, key = "native-%d" % self.serial, "q%d" % self.serial
        if self.fault == "overflow_missing_native_id" and len([e for e in self.requests.values() if e["status"] == "pending"]) >= self.BUDGET:
            self.native.add("request")  # the fixture oracle lost the identity of the failed attempt
        else:
            self.native.add("request", request=native_id)
        if len([e for e in self.requests.values() if e["status"] == "pending"]) >= self.BUDGET:
            if self.fault == "overflow_other_error":
                raise CheckFailed("request method changed during normalization")
            if self.fault == "overflow_harness_gap":
                from agent_conformance.results import HarnessGap
                raise HarnessGap("public agent facts did not reach the required state (missing fixture mechanism)")
            if self.fault not in ("silent_overflow", "overflow_missing_native_id"):
                target = "native-elsewhere" if self.fault == "overflow_wrong_cancel" else native_id
                self.native.add("answer", request=target, normalized_response={"action": "cancel"})
            raise CheckFailed("public agent facts did not reach the required state")
        detail = options.get("command") or options.get("message") or ""
        if self.fault == "detail_lost":
            detail = detail[:16]
        params = {"command": detail} if method == "permission" else {"questions": [{"question": detail}]}
        run = options.get("run") or ("run-1" if self.state == "running" else "background")
        if self.fault == "background_bound_to_foreground":
            run = "run-1"
        entity = {"request": key, "revision": 1, "method": method, "title": (options.get("title") or detail)[:256],
                  "status": "pending", "params": params, "scope": {"session": "s1", "run": run},
                  "_native": native_id, "_blocking": options.get("is_blocking", True),
                  "choices": [{"id": "accept", "effect": "allow_once"}] if method == "permission" else None,
                  "schema": {"type": "object", "properties": {}, "required": []} if method == "form" else None}
        self.requests[key] = entity
        return native_id, entity

    async def native_verb(self, verb, **params):
        if verb == "turn":
            self.state = "running" if params["state"] == "start" else "idle"
        elif verb == "resolve_request":
            for entity in self.requests.values():
                if entity["_native"] == params["request"] and self.fault != "ignore_native_resolution":
                    entity["status"] = "cancelled"
        return {}

    async def invoke(self, op, args, **kwargs):
        if op == "agent.interrupt":
            for entity in self.requests.values():
                withdraw = entity["_blocking"] and entity["scope"]["run"] == "run-1"
                if withdraw or self.fault == "interrupt_withdraws_all":
                    entity["status"] = "cancelled"
                    self.native.add("request_cancelled", request=entity["_native"])
            self.state = "idle"
            return {"outcome": "accepted"}
        entity = self.requests.get(args["request"])
        if entity is not None and entity["status"] == "cancelled" and self.fault == "stale_indeterminate":
            return {"outcome": "indeterminate", "error": {"data": {"kind": "timeout"}}}
        if entity is not None and entity["status"] == "cancelled" and self.fault == "stale_writes":
            self.native.add("command", effectful=True)
            return {"outcome": "rejected", "error": {"data": {"kind": "stale"}}}
        if entity is None or (entity["status"] != "pending" and self.fault != "answer_withdrawn"):
            return {"outcome": "rejected", "error": {"data": {"kind": "stale"}}}
        self.native.add("command", effectful=True)
        target = entity["_native"]
        if self.fault == "answer_wrong_native":
            target = "native-elsewhere"
        self.native.add("answer", request=target, normalized_response=args["response"])
        entity["status"] = "answered"
        return {"outcome": "dispatched"}


def run(branch, fault=None):
    fake = Fake(fault)
    async def answer(ctx, request, response_value, **overrides):
        return await ctx.invoke("agent.respond", {"request": request["request"], "revision": request["revision"],
                                                  "response": response_value, **overrides})
    with mock.patch.object(cases, "make_request", fake.make_request), mock.patch.object(cases, "answer", answer):
        return asyncio.run(branch(fake, DECLARATION))


class ExpiryExclusion(unittest.TestCase):
    def test_only_explicit_declarations_exclude_expiry(self):
        self.assertEqual(branches.expiry_exclusion({"native_timeout": "none"}), "none")
        self.assertEqual(branches.expiry_exclusion({"native_timeout": "unrepresentable_positive"}), "unrepresentable_positive")
        for declaration in ({}, {"native_timeout": None}, {"native_timeout": "None"}, {"native_timeout": 0}, {"native_timeout": "unknown"}):
            self.assertIsNone(branches.expiry_exclusion(declaration))


class RequestFixtureNegatives(unittest.TestCase):
    def test_conforming_fakes_pass(self):
        run(branches.c6_non_confirm)
        self.assertIsNotNone(run(branches.r2_native_withdrawal))  # expiry gap reason is returned, not swallowed
        run(branches.r6_background_foreground)

    def test_each_broken_rule_fails(self):
        for branch, faults in ((branches.c6_non_confirm, ("oversized_facts", "silent_title_cut", "summary_omits_title", "detail_lost",
                                                          "silent_overflow", "overflow_other_error", "overflow_wrong_cancel",
                                                          "overflow_harness_gap", "overflow_missing_native_id",
                                                          "oversized_after_refusal")),
                               (branches.r2_native_withdrawal, ("ignore_native_resolution", "answer_withdrawn")),
                               (branches.r6_background_foreground, ("interrupt_withdraws_all", "background_bound_to_foreground",
                                                                    "answer_wrong_native", "stale_indeterminate", "stale_writes"))):
            for fault in faults:
                with self.subTest(branch=branch.__name__, fault=fault), self.assertRaises(CheckFailed):
                    run(branch, fault)


if __name__ == "__main__":
    unittest.main()
