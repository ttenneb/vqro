import copy
import json
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
from agents import Agents, DEFAULTS, EFFECTS, EXTENSIONS, SETTINGS, runtimes
from prim_ext import PrimError


class FakeCore:
    def __init__(self):
        self.entries, self.resources, self.handlers, self.live = {}, {}, {}, {}
        self.calls, self.notices, self.transactions = [], [], []
        self.prim = self
        self.fail_tabs, self.fail_effect, self.make_agent = False, None, True

    def fresh(self):
        pass

    def version(self, key):
        return self.entries.get(key, {}).get("version", 0)

    def value(self, key, default=None):
        return copy.deepcopy(self.entries.get(key, {}).get("value", default))

    def apply(self, ops, note):
        self.transactions.append(copy.deepcopy(ops))
        for op in ops:
            if "expect" in op and op["expect"] != self.version(op["key"]):
                raise PrimError("changed", "conflict")
            if op["op"] == "delete":
                self.entries.pop(op["key"], None)
            else:
                self.entries[op["key"]] = {"value": copy.deepcopy(op["value"]), "version": self.version(op["key"])+1}
        return {}

    def contribute(self, entries):
        ops = [{"op": "set", "key": k, "value": dict(v, owner="agents")} for k, v in entries.items()
               if self.value(k) != dict(v, owner="agents")]
        if ops:
            self.apply(ops, "contribute")

    def retrying(self, fn):
        return fn()

    def command(self, name, title):
        def register(fn):
            self.handlers[name] = fn
            return fn
        return register

    def on_event(self, fn):
        self.listener = fn

    def notify(self, text, style=None):
        self.notices.append((text, style))

    def publish(self, view, content):
        self.live[view] = copy.deepcopy(content)
        return {}

    def invoke(self, name, args=None):
        self.calls.append((name, copy.deepcopy(args)))
        if self.fail_tabs:
            raise PrimError("tabs stopped", "unavailable")
        return {"ok": True}

    def add_agent(self):
        self.resources["r2"] = {"id": "r2", "kind": "agent", "state": "live", "available": True,
                                "host": "r1", "data": {"requests": []}, "status": {"state": "idle"}}
        self.entries["view/agent-r2"] = {"value": {"kind": "panel"}, "version": 1}

    def call(self, method, params):
        self.calls.append((method, copy.deepcopy(params)))
        if method == "resource.spawn":
            if self.make_agent:
                self.add_agent()
            return {"outcome": "observed", "resource": {"id": "r1"}}
        if self.fail_effect:
            raise PrimError("native outcome unknown", self.fail_effect)
        return {"outcome": "dispatched" if params["op"] == "agent.answer" else "accepted", "effect": "e1"}


class AgentsTests(unittest.TestCase):
    def setUp(self):
        self.core = FakeCore()
        self.agents = Agents(self.core)

    def command(self, name, **args):
        return self.core.handlers["agents."+name](args)

    def native(self):
        return [(m, p) for m, p in self.core.calls if m.startswith("resource.")]

    def test_defaults_and_contributions_without_execution(self):
        self.assertEqual(self.core.value(SETTINGS), DEFAULTS)
        self.assertEqual(self.core.value("binding/agents.new")["key"], "a")
        self.assertEqual(self.core.value("menu/agents.new")["where"], "tabbar")
        self.assertEqual(self.native(), [])
        self.assertTrue(all("agents."+op in self.core.handlers for op in EFFECTS))

    def test_new_spawns_once_with_literal_runtime_argv_then_public_tab_command(self):
        self.core.apply([{"op": "set", "key": SETTINGS, "value": {"runtimes": [{"name": "custom", "argv": ["/node", "literal $(not-a-shell)", "--mode", "rpc"]}]}}], "settings")
        result = self.command("new", cwd="/private/project")
        self.assertEqual((result["outcome"], result["agent"]), ("observed", "r2"))
        spawn = self.native()[0][1]
        self.assertTrue(spawn["bridge"])
        self.assertEqual(spawn["argv"][1:4], ["-m", "pi_bridge", "--"])
        self.assertEqual(spawn["argv"][4:], ["/node", "literal $(not-a-shell)", "--mode", "rpc"])
        self.assertEqual(spawn["env"], {"PYTHONPATH": EXTENSIONS})
        self.assertEqual(spawn["cwd"], "/private/project")
        self.assertEqual(self.core.calls[-1], ("tabs.show", {"view": "agent-r2", "title": "custom r2"}))
        self.assertEqual(len(self.native()), 1)

    def test_runtime_picker_does_not_spawn_until_explicit_selection(self):
        self.core.apply([{"op": "set", "key": SETTINGS, "value": {"runtimes": [{"name": "one", "argv": ["/one"]}, {"name": "two", "argv": ["/two"]}]}}], "settings")
        result = self.command("new")
        self.assertEqual(result["client"]["popup"]["view"], "agents-runtimes")
        self.assertEqual(self.native(), [])
        self.command("new", runtime="two")
        self.assertEqual(self.native()[0][1]["argv"][-1], "/two")

    def test_tab_failure_does_not_respawn_or_discard_observed_agent(self):
        self.core.fail_tabs = True
        result = self.command("new")
        self.assertEqual(result["outcome"], "observed")
        self.assertEqual(result["client"]["popup"]["view"], "agent-r2")
        self.assertEqual(len(self.native()), 1)

    def test_late_agent_does_not_cause_a_second_spawn(self):
        self.core.make_agent = False
        with patch("agents.time.monotonic", side_effect=[0, 13]):
            result = self.command("new")
        self.assertFalse(result["ready"])
        self.assertEqual(result["outcome"], "observed")
        self.assertEqual(len(self.native()), 1)

    def test_all_native_effects_and_truthful_notifications(self):
        self.core.add_agent()
        for op in ("prompt", "steer", "follow_up"):
            result = self.command(op, context={"view": "agent-r2"}, text="explicit text")
            self.assertEqual(result["outcome"], "accepted")
            self.assertIn("not settled", self.core.notices[-1][0])
        for op in ("abort", "clear_queue"):
            self.command(op, resource="r2")
        self.assertEqual([p["op"] for _, p in self.native()], ["agent."+op for op in ("prompt", "steer", "follow_up", "abort", "clear_queue")])
        self.assertEqual(self.native()[-2][1]["args"], {})

    def test_invalid_context_empty_prompt_and_unavailable_agent_send_nothing(self):
        for args in ({}, {"context": {"view": "settings"}}, {"resource": "r2", "text": ""}):
            with self.assertRaises(PrimError):
                self.command("prompt", **args)
        self.core.add_agent()
        self.core.resources["r2"]["available"] = False
        with self.assertRaises(PrimError):
            self.command("abort", resource="r2")
        self.assertEqual(self.native(), [])

    def test_uncertain_effect_is_not_retried(self):
        self.core.add_agent()
        self.core.fail_effect = "indeterminate"
        with self.assertRaises(PrimError):
            self.command("prompt", resource="r2", text="one attempt")
        self.assertEqual(len(self.native()), 1)
        self.assertIn("no automatic retry", self.core.notices[-1][0])

    def test_request_picker_answers_all_native_kinds_and_cancellation(self):
        self.core.add_agent()
        for kind, answer in [("confirm", {"value": False}), ("select", {"value": "B"}),
                             ("input", {"text": ""}), ("editor", {"text": "line1\nline2"})]:
            self.core.resources["r2"]["data"]["requests"] = [{"id": "q", "kind": kind,
                "prompt": "Question\x1b]evil\x07", "data": {"message": "Full body", "options": ["A", "B"]}}]
            popup = self.command("answer", resource="r2")
            self.assertEqual(popup["client"]["popup"]["view"], "agents-requests-r2")
            content = self.core.live["agents-requests-r2"]
            self.assertNotIn("\x1b", json.dumps(content, ensure_ascii=False))
            result = self.command("answer", resource="r2", request="q", **answer)
            self.assertEqual(result["outcome"], "dispatched")
            self.assertIn("no runtime acknowledgement", self.core.notices[-1][0])
            native = self.native()[-1][1]["args"]
            self.assertEqual(native["id"], "q")
            self.assertEqual(native.get("confirmed", native.get("value")), answer.get("value", answer.get("text")))
        result = self.command("answer", resource="r2", request="q", cancelled=True)
        self.assertEqual(result["outcome"], "dispatched")
        self.assertTrue(self.native()[-1][1]["args"]["cancelled"])

    def test_stale_or_invalid_answer_is_rejected_without_native_send(self):
        self.core.add_agent()
        self.core.resources["r2"]["data"]["requests"] = [{"id": "q", "kind": "confirm", "prompt": "?", "data": {}}]
        for args in ({"request": "old", "value": True}, {"request": "q", "value": "true"}):
            with self.assertRaises(PrimError):
                self.command("answer", resource="r2", **args)
        self.assertEqual(self.native(), [])

    def test_resource_status_updates_do_not_churn_pane_contributions(self):
        self.core.add_agent()
        self.agents.event("resource", {"id": "r2"})
        count = len(self.core.transactions)
        self.assertEqual(self.core.value("menu/agents.r2.prompt")["when"], {"view": "agent-r2"})
        for _ in range(10):
            self.agents.event("resource", {"id": "r2"})
        self.assertEqual(len(self.core.transactions), count)
        self.core.resources.clear()
        self.agents.event("resource.removed", {"id": "r2"})
        self.assertIsNone(self.core.value("menu/agents.r2.prompt"))

    def test_configuration_validates_before_write_and_launch(self):
        for value in ({}, {"runtimes": []}, {"runtimes": [{"name": "x", "argv": "shell string"}]},
                      {"runtimes": [{"name": "x", "argv": ["ok"]}, {"name": "x", "argv": ["again"]}]}):
            with self.assertRaises(PrimError):
                runtimes(value)
        before = self.core.value(SETTINGS)
        with self.assertRaises(PrimError):
            self.command("configure", text="not JSON")
        self.assertEqual(self.core.value(SETTINGS), before)
        self.command("configure", text='[{"name":"fake","argv":["/python","fixture"]}]')
        self.assertEqual(self.core.value(SETTINGS)["runtimes"][0]["name"], "fake")
        self.assertEqual(self.native(), [])


if __name__ == "__main__":
    unittest.main()
