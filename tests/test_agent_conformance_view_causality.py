"""C12 handler negatives: a fake public core that breaks one rule at a time must fail the case (no false pass)."""
import asyncio
import json
import unittest

# Load the shared registry first, then this slice explicitly (K1 adds the module import at the end of cases.py).
from agent_conformance.cases import HANDLERS
import agent_conformance.view_causality_cases  # noqa: F401  (registers C11/C12)
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


class Witness:
    """In-process stand-in for the public dispatch-boundary provider: records what the fake core dispatches."""
    rid = "w1"

    def __init__(self):
        self.received = []

    async def start(self):
        return self

    async def close(self):
        pass


class Ctx:
    """Implements the core rule unless `fault` names one rule to break."""
    def __init__(self, fault=None):
        self.fault, self.events, self.evidence, self.serial = fault, [], [], 0
        self.native, self.peer = Native(fault), Peer(self)
        self.agents = [{"id": "r1"}]
        self.witness = Witness()
        self.dispatch_witness_factory = lambda: self.witness

    async def facts(self):
        return {"epoch": "e1", "state": "idle", "caps": {"rev": 1, "agent.send": {"modes": {"start": {}}}}}

    async def wait_facts(self, predicate):
        return True

    def delivery(self):
        self.serial += 1
        return "d" + str(self.serial)

    async def invoke(self, op, args, rid=None, defaults=True, **root):
        args = json.loads(json.dumps(args))  # a real RPC never shares caller memory with the core
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
                if rid == self.witness.rid:
                    self.witness.received.append({"op": op, "args": args})
            return {"outcome": "rejected", "rpc_error": True, "error": {"message": "invalid", "data": {"kind": "invalid"}}}
        self.serial += 1
        effect = "boot-1:e" + str(self.serial)
        audit = dict(links)
        if self.fault == "drop_cause" and "cause" in audit:
            audit.pop("cause")
        if self.fault == "rewrite_provenance" and "cause" in audit:
            audit["cause"] = dict(audit["cause"], provenance="asserted" if audit["cause"]["provenance"] != "asserted" else "adapter")
        dispatched = json.loads(json.dumps(args))
        if self.fault == "drop_dispatched_cause":
            dispatched.pop("cause", None)  # audit stays right, the adapter-bound args lose the link
        if self.fault == "inject_root_link" and "cause" in root and "cause" not in dispatched:
            dispatched["cause"] = root["cause"]  # the core must not rewrite args with root metadata
        if rid == self.witness.rid:
            self.witness.received.append({"op": op, "args": dispatched})
        self.events.append({"type": "effect", "data": {"effect": effect, "op": op, "outcome": "accepted", **audit}})
        self.native.rows.append({"kind": "command", "effectful": True})
        return {"outcome": "accepted", "effect": effect, "data": {"receipt": {"delivery": args.get("delivery")}}}


def run(fault=None):
    asyncio.run(HANDLERS["C12"](Ctx(fault)))


class C12Negatives(unittest.TestCase):
    def test_conforming_fake_core_passes(self):
        run()

    def test_each_broken_rule_fails(self):
        for fault in ("accept_invalid", "accept_conflict", "write_on_refusal", "drop_cause", "rewrite_provenance",
                      "drop_dispatched_cause", "inject_root_link"):
            with self.subTest(fault=fault), self.assertRaises(CheckFailed):
                run(fault)


if __name__ == "__main__":
    unittest.main()


class C11Fake:
    """A fake public endpoint whose facts follow the Pi-wire native fixture reports, unless `fault` breaks one rule."""
    def __init__(self, fault=None):
        self.fault, self.evidence, self.serial = fault, [], 0
        self.manifest = {"wire": "pi"}
        self.report, self.native_cwd, self.model = None, None, "fixture-small"
        self.native = self
        self.rows = []
        self.agents = [{"id": "r1"}]

    def mark(self):
        return len(self.rows)

    def writes(self, mark):
        return self.rows[mark:]

    # native fixture control
    async def call(self, verb, **params):
        if verb == "state":
            return {"cwd": self.native_cwd or "/launch", "model": self.model, "effort": "off"}
        if verb == "native_cwd":
            self.native_cwd = params["cwd"]
        if verb == "usage":
            self.report = params["report"]
        return {}

    def _usage(self):
        report = self.report
        if report is None:
            return None
        usage = {}
        context = report.get("contextUsage")
        if context:
            usage["context"] = {"window": context["contextWindow"]}
            if context.get("tokens") is not None:
                usage["context"]["used"] = context["tokens"]
            elif self.fault == "used_from_last_turn" and "tokens" in report:
                usage["context"]["used"] = report["tokens"]["total"]
        if "tokens" in report:
            t = report["tokens"]
            usage["tokens"] = {"input": t["input"], "output": t["output"], "cache_read": t["cacheRead"], "cache_write": t["cacheWrite"]}
        elif self.fault == "zero_tokens":
            usage["tokens"] = {"input": 0, "output": 0}
        if self.fault == "priced_cost" and "cost" in report:
            usage["cost"] = {"amount": report["cost"], "currency": "USD", "basis": "reported"}
        if self.fault == "late_priced_cost" and "cost" not in report:
            usage["cost"] = {"amount": 77.0, "currency": "USD", "basis": "reported", "plan": "fabricated-with-no-native-report"}
        return usage

    async def facts(self):
        cwd = "/bridge-process" if self.fault == "bridge_cwd" else (self.native_cwd or "/launch")
        model = {"id": "guessed-model" if self.fault == "guessed_model" else self.model, "effort": "off"}
        return {"state": "idle", "session": {"id": "s1", "cwd": cwd, "model": model}, "usage": self._usage(),
                "caps": {"rev": 1, "agent.send": {"modes": {"start": {}}}, "agent.configure": {}},
                "config": [{"id": "thinking_level", "category": "thought_level", "type": "select", "rev": 1, "value": "a",
                            "choices": [{"value": "a"}, {"value": "b"}]}] if self.fault == "effort_only_option" else
                          [{"id": "model", "category": "model", "type": "select", "rev": 1, "value": "a",
                            "choices": [{"value": "a"}, {"value": "b"}]}]}

    async def ensure_session(self):
        return await self.facts()

    async def wait_facts(self, predicate, timeout=2):
        facts = await self.facts()
        if not predicate(facts):
            raise CheckFailed("public agent facts did not reach the required state")
        return facts

    async def invoke(self, op, args, **root):
        if op == "agent.configure" and self.fault == "effort_only_option":
            self.rows.append({"kind": "command", "effectful": True, "command": "set_thinking_level"})  # model unchanged
        elif op == "agent.configure" and self.fault != "configure_without_native":
            self.rows.append({"kind": "command", "effectful": True, "command": "set_model"})
            self.model = "fixture-large"
        return {"outcome": "observed"}

    async def send(self, text, **kwargs):
        return {"outcome": "accepted"}

    async def attach(self):
        class Reducer:
            snapshots = 1
            entities = {("message", "m1"): {"value": {"role": "assistant", **({"usage": {"tokens": {"input": 1, "output": 1}}} if self.fault == "apportioned_message" else {})}}}
        class View:
            reducer = Reducer()
        return View()

    async def wait_view(self, view, predicate):
        return predicate(view.reducer)


def run11(fault=None):
    asyncio.run(HANDLERS["C11"](C11Fake(fault)))


class C11Negatives(unittest.TestCase):
    def test_conforming_fake_passes(self):
        run11()

    def test_each_broken_rule_fails(self):
        for fault in ("used_from_last_turn", "zero_tokens", "priced_cost", "late_priced_cost", "bridge_cwd", "guessed_model",
                      "apportioned_message", "configure_without_native", "effort_only_option"):
            with self.subTest(fault=fault), self.assertRaises(CheckFailed):
                run11(fault)
