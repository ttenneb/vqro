import json
from pathlib import Path
import sys
import threading
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
from agents import Agents, EFFECTS
from prim_ext import Extension, PrimError
from agent_fixture import FakeCore


class AgentsTests(unittest.TestCase):
    def setUp(self):
        self.core = FakeCore()
        self.agents = Agents(self.core)

    def command(self, name, **args):
        return self.core.handlers["agents."+name](args)

    def native(self):
        return [(m, p) for m, p in self.core.calls if m.startswith("resource.")]

    def test_neutral_extension_has_no_launcher_or_runtime_settings(self):
        self.assertNotIn("agents.new", self.core.handlers)
        self.assertNotIn("agents.settings", self.core.handlers)
        self.assertNotIn("binding/agents.new", self.core.entries)
        self.assertEqual(self.native(), [])
        source = (Path(__file__).resolve().parents[1]/"extensions/agents.py").read_text()
        self.assertNotIn("pi_bridge", source)
        self.assertNotIn("pi1", source)
        self.assertNotIn("resource.spawn", source)

    def test_runtime_neutral_failure_and_retry_facts_remain_visible(self):
        self.core.add_agent()
        self.core.resources["r2"]["status"] = {"state": "error"}
        self.core.resources["r2"]["data"].update(failure={"message": "Other runtime failed"},
            retry={"active": False, "attempt": 3, "max_attempts": 3, "success": False})
        self.agents.event("resource", {"id": "r2"})
        rows = self.core.live["agents-r2"]["items"]
        self.assertTrue(any(row.get("style") == "error" and "Other runtime failed" in row.get("text", "") for row in rows))
        self.assertTrue(any("3/3" in row.get("text", "") for row in rows))

    def test_other_runtime_panel_and_only_declared_capabilities(self):
        self.core.add_agent()
        self.core.providers["other"]["capabilities"] = {"agent.abort": {}}
        self.core.resources["r2"]["data"] = {"custom": "native other runtime"}
        self.agents.event("resource", {"id": "r2"})
        self.assertIsNotNone(self.core.value("menu/agents.r2.abort"))
        self.assertIsNone(self.core.value("menu/agents.r2.prompt"))
        self.assertIsNone(self.core.value("menu/agents.r2.answer"))
        result = self.command("show", resource="r2")
        self.assertEqual(result["view"], "agents-r2")
        self.assertIn("native other runtime", json.dumps(self.core.live["agents-r2"]))
        with self.assertRaises(PrimError) as failure:
            self.command("prompt", resource="r2", text="must not send")
        self.assertEqual(failure.exception.kind, "unsupported")
        self.assertEqual(self.native(), [])
        self.command("abort", resource="r2")
        self.assertEqual(self.native()[-1][1]["op"], "agent.abort")

    def test_provider_capability_change_prunes_actions_without_resource_change(self):
        self.core.add_agent()
        self.agents.event("resource", {})
        self.core.providers["other"]["capabilities"] = {"agent.abort": {}}
        self.agents.event("provider", {"name": "other"})
        self.assertIsNone(self.core.value("menu/agents.r2.prompt"))
        self.assertIsNotNone(self.core.value("menu/agents.r2.abort"))

    def test_missing_provider_declarations_offer_no_native_actions(self):
        self.core.add_agent()
        self.core.providers.clear()
        self.agents.event("resource", {})
        self.assertTrue(all(self.core.value("menu/agents.r2."+op) is None for op in EFFECTS))
        self.assertIsNotNone(self.core.value("view/agents-r2"))

    def test_optional_provider_output_is_readonly_and_bounded(self):
        self.core.add_agent()
        self.core.live["agent-r2"] = {"items": [{"text": "other native transcript", "command": "not.allowed"}]}
        self.agents.event("resource", {})
        panel = self.core.live["agents-r2"]
        self.assertIn("other native transcript", json.dumps(panel))
        self.assertNotIn("command", panel["items"][0])
        self.core.live["agent-r2"] = {"items": [{"text": "x"*1024} for _ in range(100)]}
        self.agents.event("view.content", {"view": "agent-r2"})
        self.assertLessEqual(len(json.dumps(self.core.live["agents-r2"]).encode()), 32768)

    def test_all_native_effects_and_truthful_notifications(self):
        self.core.add_agent()
        for op in ("prompt", "steer", "follow_up"):
            result = self.command(op, context={"view": "agents-r2"}, text="explicit text")
            self.assertEqual(result["outcome"], "accepted")
            self.assertIn("acceptance is not settlement", self.core.notices[-1][0])
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
            self.assertNotIn("\x1b", self.core.live["agents-requests-r2"]["items"][0]["text"])
            result = self.command("answer", resource="r2", request="q", **answer)
            self.assertEqual(result["outcome"], "dispatched")
            self.assertIn("no runtime acknowledgement", self.core.notices[-1][0])
            native = self.native()[-1][1]["args"]
            self.assertEqual(native.get("confirmed", native.get("value")), answer.get("value", answer.get("text")))
        self.command("answer", resource="r2", request="q", cancelled=True)
        self.assertTrue(self.native()[-1][1]["args"]["cancelled"])

    def test_stale_or_invalid_answer_is_rejected_without_native_send(self):
        self.core.add_agent()
        self.core.resources["r2"]["data"]["requests"] = [{"id": "q", "kind": "confirm", "prompt": "?", "data": {}}]
        for args in ({"request": "old", "value": True}, {"request": "q", "value": "true"}):
            with self.assertRaises(PrimError):
                self.command("answer", resource="r2", **args)
        self.assertEqual(self.native(), [])

    def test_resource_updates_publish_only_changed_agent_panels(self):
        self.core.add_agent()
        self.core.resources["r1"] = {"id": "r1", "kind": "terminal", "state": "live", "name": "Bridge"}
        self.core.resources["r4"] = dict(self.core.resources["r2"], id="r4", host="r3")
        self.agents.event("resource", {})
        with mock.patch.object(self.core, "publish", wraps=self.core.publish) as published:
            self.core.resources["r2"]["status"] = {"state": "working"}
            self.agents.event("resource", {"id": "r2"})
            self.assertEqual([call.args[0] for call in published.call_args_list], ["agents-r2"])
            published.reset_mock()
            for _ in range(20):
                self.agents.event("resource", {"id": "r1"})
            self.assertFalse(published.called, "unchanged terminal status cannot republish every agent")

    def test_resource_status_updates_do_not_churn_contributions(self):
        self.core.add_agent()
        self.agents.event("resource", {"id": "r2"})
        count = len(self.core.transactions)
        for _ in range(10):
            self.agents.event("resource", {"id": "r2"})
        self.assertEqual(len(self.core.transactions), count)
        self.core.resources.clear()
        self.agents.event("resource.removed", {"id": "r2"})
        self.assertIsNone(self.core.value("menu/agents.r2.prompt"))

    def test_activation_removes_moved_launcher_contributions_not_foreign_entries(self):
        core = FakeCore()
        core.entries = {"binding/agents.new": {"version": 1, "value": {"owner": "agents", "key": "a"}},
                        "menu/pi.new": {"version": 1, "value": {"owner": "pi", "command": "pi.new"}}}
        core.add_agent()
        Agents(core)
        self.assertIsNone(core.value("binding/agents.new"))
        self.assertEqual(core.value("menu/pi.new")["owner"], "pi")
        self.assertIsNotNone(core.value("menu/agents.r2.prompt"))

    def test_activation_keeps_referenced_panels_before_inventory_returns(self):
        core = FakeCore()
        core.entries["view/agents-r2"] = {"version": 1, "value": {"owner": "agents", "kind": "panel", "items": []}}
        core.entries["layout/main"] = {"version": 1, "value": {"view": "agents-r2"}}
        controller = Agents(core)
        self.assertIsNotNone(core.value("view/agents-r2"))
        self.assertIn("Agent gone", json.dumps(core.live["agents-r2"]))
        controller.event("resource.removed", {"id": "r2"})
        self.assertIsNotNone(core.value("view/agents-r2"))

    def test_bridge_terminal_is_an_explicit_action_and_unavailable_agents_can_be_inspected(self):
        self.core.add_agent()
        self.core.resources["r1"] = {"id": "r1", "kind": "terminal", "name": "Other bridge", "state": "live"}
        self.core.resources["r2"]["available"] = False
        self.command("show", resource="r2")
        panel = self.core.live["agents-r2"]
        bridge = next(row for row in panel["items"] if row.get("text") == "Bridge terminal")
        self.assertEqual((bridge["command"], bridge["args"]), ("tabs.show", {"resource": "r1"}))
        self.assertIn("unavailable", json.dumps(panel))
        with self.assertRaises(PrimError):
            self.command("prompt", resource="r2", text="not sent")

    def test_palette_template_targets_focused_agent(self):
        self.core.add_agent()
        self.core.resources["r4"] = dict(self.core.resources["r2"], id="r4")
        self.agents.event("resource", {})
        template = self.core.value("menu/agents.r2.prompt")
        self.command("prompt", **dict(template.get("args", {}), context={"view": "agents-r4"}, text="focused"))
        self.assertEqual(self.native()[-1][1]["resource"], "r4")


class ProviderCache(unittest.TestCase):
    def test_snapshot_events_and_snapshot_without_providers(self):
        ext = Extension.__new__(Extension)
        ext.cache_lock = threading.RLock()
        ext._load({"entries": {}, "resources": [], "live": {}, "providers": {"a": {"capabilities": {"agent.abort": {}}}}, "seq": 0, "epoch": "e"})
        self.assertIn("agent.abort", ext.providers["a"]["capabilities"])
        ext._notification("event", {"epoch": "e", "seq": 1, "type": "provider", "data": {"name": "a", "state": {"name": "a", "capabilities": {}}}})
        self.assertEqual(ext.providers["a"]["capabilities"], {})
        ext._load({"entries": {}, "resources": [], "seq": 2, "epoch": "e"})
        self.assertEqual(ext.providers, {})


if __name__ == "__main__":
    unittest.main()
