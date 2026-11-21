"""C12 handler negatives: a fake public core that breaks one rule at a time must fail the case (no false pass)."""
import asyncio
import json
import unittest

from agent_conformance.cases import HANDLERS
from agent_conformance.results import CheckFailed

STRINGS = ("kind", "id", "provenance", "endpoint", "epoch", "boot")


def depth(value):
    if isinstance(value, dict):
        return 1 + max((depth(v) for v in value.values()), default=0)
    if isinstance(value, list):
        return 1 + max((depth(v) for v in value), default=0)
    return 0


def valid_string(value):
    return isinstance(value, str) and 0 < len(value.encode()) <= 256 and not any(ord(c) < 32 for c in value)


def valid_cause(cause):
    if not isinstance(cause, dict) or depth(cause) > 64:
        return False
    if len(json.dumps(cause, separators=(",", ":"), ensure_ascii=False).encode()) > 4096:
        return False
    if not all(valid_string(cause.get(field)) for field in ("kind", "id", "provenance")):
        return False
    if any(field in cause and not valid_string(cause[field]) for field in ("endpoint", "epoch", "boot")):
        return False
    if cause["kind"] in ("entity", "request"):
        return valid_string(cause.get("endpoint")) and valid_string(cause.get("epoch"))
    if cause["kind"] == "effect":
        return valid_string(cause.get("boot"))
    return True


class Native:
    def __init__(self, fault):
        self.fault, self.rows = fault, []

    async def call(self, verb, **params):
        return {}

    def mark(self):
        return len(self.rows)

    def writes(self, mark):
        return self.rows[mark:]


class Peer:
    def __init__(self, ctx):
        self.ctx, self.core_origin = ctx, {"principal": "owner"}

    async def call(self, method, params=None):
        assert method == "workspace.snapshot"
        return {"epoch": "boot-1"}

    async def wait(self, predicate):
        value = predicate()
        if not value:
            raise asyncio.TimeoutError()
        return value


class Ctx:
    """Implements the core rule unless `fault` names one rule to break."""
    def __init__(self, fault=None):
        self.fault, self.events, self.evidence, self.serial = fault, [], [], 0
        self.native, self.peer = Native(fault), Peer(self)
        self.agents = [{"id": "r1"}]

    async def facts(self):
        return {"epoch": "e1", "state": "idle", "caps": {"rev": 1, "agent.send": {"modes": {"start": {}}}}}

    async def wait_facts(self, predicate):
        return True

    def delivery(self):
        self.serial += 1
        return "d" + str(self.serial)

    async def invoke(self, op, args, **root):
        links, invalid = {}, False
        for field in ("cause", "correlation"):
            nested, explicit = args.get(field), root.get(field)
            for value in (nested, explicit):
                if value is None:
                    continue
                ok = valid_cause(value) if field == "cause" else valid_string(value)
                if not ok and self.fault != "accept_invalid":
                    invalid = True
            if nested is not None and explicit is not None and nested != explicit and self.fault != "accept_conflict":
                invalid = True
            value = explicit if explicit is not None else nested
            if value is not None:
                links[field] = value
        if invalid:
            if self.fault == "write_on_refusal":
                self.native.rows.append({"kind": "command", "effectful": True})
            return {"outcome": "rejected", "rpc_error": True, "error": {"message": "invalid", "data": {"kind": "invalid"}}}
        self.serial += 1
        effect = "boot-1:e" + str(self.serial)
        audit = dict(links)
        if self.fault == "drop_cause" and "cause" in audit:
            audit.pop("cause")
        if self.fault == "rewrite_provenance" and "cause" in audit:
            audit["cause"] = dict(audit["cause"], provenance="asserted" if audit["cause"]["provenance"] != "asserted" else "adapter")
        if self.fault == "mutate_args":
            args.pop("cause", None)
        self.events.append({"type": "effect", "data": {"effect": effect, "op": op, "outcome": "accepted", **audit}})
        self.native.rows.append({"kind": "command", "effectful": True})
        return {"outcome": "accepted", "effect": effect, "data": {"receipt": {"delivery": args.get("delivery")}}}


def run(fault=None):
    asyncio.run(HANDLERS["C12"](Ctx(fault)))


class C12Negatives(unittest.TestCase):
    def test_conforming_fake_core_passes(self):
        run()

    def test_each_broken_rule_fails(self):
        for fault in ("accept_invalid", "accept_conflict", "write_on_refusal", "drop_cause", "rewrite_provenance", "mutate_args"):
            with self.subTest(fault=fault), self.assertRaises(CheckFailed):
                run(fault)


if __name__ == "__main__":
    unittest.main()
