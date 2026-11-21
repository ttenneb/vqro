"""C12 handler negatives: a fake public core that breaks one rule at a time must fail the case (no false pass)."""
import asyncio
import json
import unittest

# Load the shared registry first, then this slice explicitly (K1 adds the module import at the end of cases.py).
from agent_conformance.cases import HANDLERS
import agent_conformance.view_causality_cases as m  # registers C11/C12
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
        if self.fault == "bool_dispatched_args" and isinstance((dispatched.get("cause") or {}).get("extra"), dict):
            dispatched["cause"]["extra"]["bounded"][0] = True
        if self.fault == "bool_audit" and isinstance((audit.get("cause") or {}).get("extra"), dict):
            audit["cause"] = json.loads(json.dumps(audit["cause"]))
            audit["cause"]["extra"]["bounded"][0] = True
        if self.fault == "drop_dispatched_cause":
            dispatched.pop("cause", None)  # audit stays right, the adapter-bound args lose the link
        if self.fault == "inject_root_link" and "cause" in root and "cause" not in dispatched:
            dispatched["cause"] = root["cause"]  # the core must not rewrite args with root metadata
        if rid == self.witness.rid:
            # The core lifts the links into the provider-bound effect metadata (root params), args left unchanged.
            metadata = json.loads(json.dumps(links))
            if self.fault == "drop_dispatch_metadata":
                metadata = {}
            if self.fault == "rewrite_dispatch_metadata":
                if "cause" in metadata:
                    metadata["cause"] = dict(metadata["cause"], provenance="wrong-provider-provenance")
                if "correlation" in metadata:
                    metadata["correlation"] = "wrong-provider-correlation"
            if self.fault == "bool_dispatch_metadata" and isinstance(metadata.get("cause", {}).get("extra"), dict) \
                    and isinstance(metadata["cause"]["extra"].get("bounded"), list):
                metadata["cause"]["extra"]["bounded"][0] = True  # JSON number 1 -> true; Python == still says equal
            if self.fault == "invent_dispatch_metadata" and "correlation" not in metadata:
                metadata["correlation"] = "invented"
            self.witness.received.append({"op": op, "args": dispatched, **metadata})
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
                      "drop_dispatched_cause", "inject_root_link", "drop_dispatch_metadata", "rewrite_dispatch_metadata",
                      "invent_dispatch_metadata", "bool_dispatch_metadata", "bool_dispatched_args", "bool_audit"):
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


class C11FutureFake(C11Fake):
    """Mock transport/control with the codex_control native_response/model_readback seam; no runtime/model."""
    def __init__(self, fault=None):
        super().__init__(fault)
        self.manifest = {'wire': 'codex'}
        self.model, self.selected, self.stage, self.active = 'old', None, 'initial', False
        self.sends, self.controls, self.releases = [], [], 0

    def record(self, kind, **value):
        self.rows.append({'seq': len(self.rows) + 1, 'kind': kind, **value})

    def records(self, kind=None, *, after=0):
        return [row for row in self.rows if row['seq'] > after and (kind is None or row['kind'] == kind)]

    def writes(self, mark=0):
        return [row for row in self.records('command', after=mark) if row.get('effectful')]

    async def call(self, verb, **params):
        self.controls.append((verb, params))
        if verb == 'state': return {'session': 'native-s', 'model': self.model, 'cwd': '/launch', 'effort': None}
        if verb == 'pause_before':
            self.assert_phase(params); self.release_event = asyncio.Event()
        if verb == 'release':
            self.assert_phase(params); self.releases += 1; self.release_event.set()
        if verb == 'usage': self.report = params['report']
        if verb == 'turn': self.active = params['state'] == 'start'
        return {}

    def assert_phase(self, params):
        assert params == {'phase': 'model_readback'}, params

    async def wait(self, predicate, *, timeout=2):
        await asyncio.sleep(0)  # Yield to the ONE public send task, no timed scheduling oracle.
        result = predicate(self.rows)
        if not result: raise CheckFailed('native ledger condition missing')
        return result

    async def invoke(self, op, args, **root):
        assert op == 'agent.configure'
        self.selected, self.stage = args['value'], 'configured'
        if self.fault in ('early_mutation', 'early_query'):
            self.record('command', command='thread/resume' if self.fault == 'early_mutation' else 'thread/read',
                        effectful=self.fault == 'early_mutation', value={'id': 99})
        return {'outcome': 'accepted'}

    def _usage(self):
        if self.report is None: return None
        usage = {}
        if 'modelContextWindow' in self.report: usage['context'] = {'window': self.report['modelContextWindow']}
        if 'total' in self.report:
            total = self.report['total']
            usage['tokens'] = {'input': total['inputTokens'], 'output': total['outputTokens'], 'cache_read': total['cachedInputTokens']}
        return usage

    async def facts(self):
        actual, pending, status = 'old', self.selected is not None, 'pending'
        if self.stage == 'final':
            actual = self.reported_model
            pending, status = actual != self.selected, 'confirmed' if actual == self.selected else 'unknown' if actual is None else 'mismatch'
        if (self.stage == 'configured' and self.fault == 'premature') or (self.stage == 'held' and self.fault == 'clear_ack'):
            actual, pending, status = self.selected, False, 'confirmed'
        if self.stage == 'final' and self.fault in ('request_infer', 'ack_infer', 'drop_read_infer'):
            actual, pending, status = self.selected, False, 'confirmed'
        session = {'id': 'public-s', 'cwd': '/launch'}
        if actual is not None: session['model'] = {'id': actual}
        return {'epoch': 'e', 'state': 'busy' if self.active else 'idle', 'session': session, 'usage': self._usage(),
                'caps': {'rev': 1, 'agent.configure': {}, 'agent.send': {'modes': {'start': {}}}},
                'config': [{'id': 'model', 'category': 'model', 'type': 'select', 'rev': 1,
                            'value': self.selected or 'old', 'choices': [{'value': 'old'}, {'value': 'choice'}],
                            'pending': pending, 'applies_from': 'next_turn', 'status': status}]}

    async def send(self, text, **params):
        self.sends.append(text); self.active = True
        command = {'id': 100, 'method': 'turn/start', 'params': {'threadId': 'native-s', 'model': self.selected, 'input': [{'type': 'text', 'text': text}]}}
        if self.fault == 'wrong_model': command['params']['model'] = 'other'
        self.record('command', command='turn/start', effectful=True, value=command)
        if self.fault == 'replay': self.record('command', command='turn/start', effectful=True, value=dict(command, id=101))
        ack = {'id': 100, 'result': {'turn': {'id': 'turn-1', 'status': 'inProgress', 'items': []}}}
        if self.fault == 'ack_infer': ack['result']['turn']['model'] = self.selected
        self.record('native_response', command='turn/start', request=100, success=True, value=ack)
        query = {'id': 102, 'method': 'thread/read', 'params': {'threadId': 'native-s', 'includeTurns': False}}
        if self.fault == 'foreign_read': query['params']['threadId'] = 'foreign'
        self.record('command', command='thread/read', effectful=False, value=query)
        if self.fault == 'duplicate_read': self.record('command', command='thread/read', effectful=False, value=dict(query, id=103))
        self.stage = 'held'
        self.record('paused', phase='model_readback', when='before')
        async def finish_read():
            await self.release_event.wait()
            self.reported_model = ('other' if self.fault == 'mismatch' else None if self.fault in ('unknown', 'reject', 'request_infer', 'ack_infer', 'drop_read', 'drop_read_infer') else self.selected)
            self.model = self.reported_model or self.selected
            response = {'id': 102, 'result': {'thread': {'id': 'native-s', 'model': self.reported_model,
                                                        'status': {'type': 'active'}, 'reasoningEffort': None}, 'readState': None}}
            if self.fault == 'wrong_response_id': response['id'] = 103
            if self.fault == 'foreign_response': response['result']['thread']['id'] = 'foreign'
            if self.fault == 'reject': response = {'id': 102, 'error': {'code': -32603, 'message': 'native read rejected'}}
            if self.fault not in ('drop_read', 'drop_read_infer'):
                self.record('native_response', command='thread/read', request=102, success=self.fault != 'reject', value=response)
            self.stage = 'final'
            return {'outcome': 'accepted'}
        if self.fault == 'late_public_ack': return await finish_read()
        self.readback_task = asyncio.create_task(finish_read())
        return {'outcome': 'accepted'}


class C11FutureModelHandler(unittest.TestCase):
    def test_future_route_and_whole_c11_reuse_existing_turn(self):
        context = C11FutureFake()
        gap, active = asyncio.run(m.future_model_route_witness(context))
        self.assertIsNone(gap); self.assertTrue(active)
        self.assertEqual(len(context.sends), 1)
        self.assertEqual(context.releases, 1)
        self.assertEqual(context.evidence[-1]['scope'], 'current_configured_after_turn_dispatch')
        late_ack = C11FutureFake('late_public_ack')
        self.assertEqual(asyncio.run(m.future_model_route_witness(late_ack)), (None, True))
        self.assertEqual(len(late_ack.sends), 1)
        full = C11FutureFake()
        asyncio.run(HANDLERS['C11'](full))
        self.assertEqual(len(full.sends), 1, 'C11 must reuse the future-model witness turn, never replay it')

    def test_premature_application_inference_and_replay_fail(self):
        for fault in ('premature', 'clear_ack', 'request_infer', 'ack_infer', 'replay', 'early_mutation', 'early_query',
                      'wrong_model', 'foreign_read', 'foreign_response', 'wrong_response_id', 'duplicate_read', 'drop_read_infer'):
            context = C11FutureFake(fault)
            with self.subTest(fault=fault), self.assertRaises(CheckFailed):
                asyncio.run(m.future_model_route_witness(context))
            self.assertLessEqual(len(context.sends), 1)
            if context.sends: self.assertEqual(context.releases, 1)

    def test_truthful_native_mismatch_unknown_are_missing_positive_witness_not_adapter_fault(self):
        from agent_conformance.results import HarnessGap
        for fault in ('mismatch', 'unknown', 'reject', 'drop_read'):
            context = C11FutureFake(fault)
            gap, active = asyncio.run(m.future_model_route_witness(context))
            self.assertIsInstance(gap, str); self.assertTrue(active)
            self.assertFalse(context.evidence[-1]['matched'])
            full = C11FutureFake(fault)
            with self.subTest(fault=fault), self.assertRaises(HarnessGap):
                asyncio.run(HANDLERS['C11'](full))
            self.assertEqual(len(full.sends), 1)


class C11FutureModelChecks(unittest.TestCase):
    def facts(self, *, pending=True, actual='old', status='pending'):
        return {'epoch': 'e', 'session': {'id': 'public-s', **({'model': {'id': actual}} if actual is not None else {})},
                'config': [{'id': 'model', 'category': 'model', 'value': 'choice', 'pending': pending,
                            'applies_from': 'next_turn', 'status': status}]}

    def test_pending_does_not_claim_native_application(self):
        m.c11_pending(self.facts(), 'model', 'choice', 'old', 'public-s', 'e', before_turn=True)
        m.c11_pending(self.facts(actual=None), 'model', 'choice', 'old', 'public-s', 'e', before_turn=False)
        for facts in (self.facts(pending=False), self.facts(actual='choice'), self.facts(status='confirmed')):
            with self.subTest(facts=facts), self.assertRaises(CheckFailed):
                m.c11_pending(facts, 'model', 'choice', 'old', 'public-s', 'e', before_turn=True)

    def test_public_facts_follow_only_matching_native_readback(self):
        self.assertTrue(m.c11_model_readback_facts(self.facts(pending=False, actual='choice', status='confirmed'),
                                                  'model', 'choice', 'choice', 'public-s', 'e'))
        self.assertFalse(m.c11_model_readback_facts(self.facts(actual='other', status='mismatch'),
                                                   'model', 'choice', 'other', 'public-s', 'e'))
        self.assertFalse(m.c11_model_readback_facts(self.facts(actual=None, status='unknown'),
                                                   'model', 'choice', None, 'public-s', 'e'))
        for actual in ('other', None):
            with self.subTest(actual=actual), self.assertRaises(CheckFailed):
                m.c11_model_readback_facts(self.facts(pending=False, actual='choice', status='confirmed'),
                                          'model', 'choice', actual, 'public-s', 'e')

    def envelopes(self):
        return [{'seq': 1, 'value': {'id': 10, 'method': 'turn/start', 'params': {'threadId': 'native-s', 'model': 'choice'}}},
                {'seq': 2, 'value': {'id': 10, 'result': {'turn': {'id': 'turn-1'}}}},
                {'seq': 3, 'value': {'id': 11, 'method': 'thread/read', 'params': {'threadId': 'native-s', 'includeTurns': False}}},
                {'seq': 4, 'value': {'id': 11, 'result': {'thread': {'id': 'native-s', 'model': 'choice', 'status': {'type': 'active'}}}}}]

    def test_native_read_is_correlated_ordered_and_not_turn_telemetry(self):
        rows = self.envelopes()
        self.assertEqual(m.c11_dispatch_readback(*rows, 'native-s', 'choice'), 'choice')
        rows[1]['value']['result']['turn']['model'] = 'invented-ack-model'
        rows[3]['value']['result']['thread']['model'] = 'other'
        self.assertEqual(m.c11_dispatch_readback(*rows, 'native-s', 'choice'), 'other')
        del rows[3]['value']['result']['thread']['model']
        self.assertIsNone(m.c11_dispatch_readback(*rows, 'native-s', 'choice'))
        for fault in ('order', 'turn_thread', 'turn_model', 'ack_id', 'read_thread', 'include_turns', 'response_id'):
            rows = self.envelopes()
            if fault == 'order': rows[2]['seq'] = 1
            if fault == 'turn_thread': rows[0]['value']['params']['threadId'] = 'foreign'
            if fault == 'turn_model': rows[0]['value']['params']['model'] = 'other'
            if fault == 'ack_id': rows[1]['value']['id'] = True
            if fault == 'read_thread': rows[2]['value']['params']['threadId'] = 'foreign'
            if fault == 'include_turns': rows[2]['value']['params']['includeTurns'] = 0
            if fault == 'response_id': rows[3]['value']['id'] = 12
            with self.subTest(fault=fault), self.assertRaises(CheckFailed):
                m.c11_dispatch_readback(*rows, 'native-s', 'choice')


class C11Negatives(unittest.TestCase):
    def test_conforming_fake_passes(self):
        run11()

    def test_each_broken_rule_fails(self):
        for fault in ("used_from_last_turn", "zero_tokens", "priced_cost", "late_priced_cost", "bridge_cwd", "guessed_model",
                      "apportioned_message", "configure_without_native", "effort_only_option"):
            with self.subTest(fault=fault), self.assertRaises(CheckFailed):
                run11(fault)
