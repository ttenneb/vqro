import copy
import copy
import json
from pathlib import Path
import sys
import threading
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
from agents import Agents, EFFECTS
from vqro_ext import Extension, VqroError
from agent_fixture import FakeCore


class AgentsTests(unittest.TestCase):
    def setUp(self):
        self.core = FakeCore()
        self.agents = Agents(self.core)

    def command(self, name, **args):
        return self.core.handlers["agents."+name](args)

    def native(self):
        return [(m, p) for m, p in self.core.calls if m.startswith("resource.")]

    def test_orphan_owned_panels_are_pruned_only_after_layout_references_disappear(self):
        self.core.add_agent()
        self.agents.event("resource", {"id": "r2"})
        self.core.apply([{"op": "set", "key": "layout/main", "value": {"tabs": [{"node": {"view": "agents-r2"}}]}}], "fixture")
        del self.core.resources["r2"]
        self.agents.event("resource.removed", {"id": "r2"})
        self.assertIsNotNone(self.core.value("view/agents-r2"))
        self.assertFalse(any(key.startswith("menu/agents.r2.") for key in self.core.entries))
        self.core.apply([{"op": "delete", "key": "layout/main"}], "fixture")
        self.agents.event("entries", {})
        self.assertIsNone(self.core.value("view/agents-r2"))
        self.core.apply([{"op": "set", "key": "view/agents-r99", "value": {"kind": "panel", "owner": "user"}}], "fixture")
        self.agents.event("entries", {})
        self.assertEqual(self.core.value("view/agents-r99")["owner"], "user")

    def test_cleanup_ignores_scalar_and_array_workspace_documents(self):
        self.core.apply([{"op": "set", "key": "doc/theme", "value": "fixture"},
                         {"op": "set", "key": "doc/list", "value": [1, 2]}], "fixture")
        self.agents.event("entries", {})
        Agents(self.core)
        self.assertEqual(self.core.value("doc/theme"), "fixture")
        self.assertEqual(self.core.value("doc/list"), [1, 2])

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
        with self.assertRaises(VqroError) as failure:
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
        self.assertNotIn("other native transcript", json.dumps(panel), "native output must not be copied")
        reference = next(row for row in panel["items"] if row.get("type") == "view")
        self.assertEqual(reference["view"], "agent-r2")
        self.assertTrue(reference["readonly"])
        self.assertIn("embedded_views", panel["requires"])
        before = json.dumps(panel)
        self.core.live["agent-r2"] = {"items": [{"text": "x"*1024} for _ in range(100)]}
        self.agents.event("view.content", {"view": "agent-r2"})
        self.assertEqual(json.dumps(self.core.live["agents-r2"]), before, "transcript changes must not republish a mirror")
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
            with self.assertRaises(VqroError):
                self.command("prompt", **args)
        self.core.add_agent()
        self.core.resources["r2"]["available"] = False
        with self.assertRaises(VqroError):
            self.command("abort", resource="r2")
        self.assertEqual(self.native(), [])

    def test_uncertain_effect_is_not_retried(self):
        self.core.add_agent()
        self.core.fail_effect = "indeterminate"
        with self.assertRaises(VqroError):
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
            with self.assertRaises(VqroError):
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
        with self.assertRaises(VqroError):
            self.command("prompt", resource="r2", text="not sent")

    def test_prefix_p_prompts_the_focused_agent_pane_only(self):
        binding = self.core.value("binding/agents.prompt")
        self.assertEqual((binding["key"], binding["command"], binding["when"]), ("p", "agents.prompt", {"resource_kind": "agent"}))
        self.assertTrue(binding["input"], "asks for the text first")
        self.core.add_agent()
        self.agents.event("resource", {"id": "r2"})
        self.assertEqual(self.core.value("view/agents-r2")["resource"], "r2", "the pane names its agent, so resource_kind is agent")
        # What the client sends for the binding: the pane context plus the typed text.
        self.command("prompt", context={"view": "agents-r2", "resource": "r2", "resource_kind": "agent"}, text="exactly this")
        self.assertEqual(self.native()[-1][1], {"resource": "r2", "op": "agent.prompt", "args": {"message": "exactly this"}})

    def declare_settings(self):
        self.core.add_agent()
        self.core.providers["other"]["capabilities"].update({"agent.command": {}, "agent.configure": {}})
        self.core.resources["r2"]["data"]["agent"] = {"v": 0,
            "config": [{"id": "model", "label": "Model", "category": "model", "type": "select", "value": "p/a", "rev": 3,
                        "choices": [{"value": "p/a", "label": "A"}, {"value": "p/b", "label": "B"}]},
                       {"id": "auto", "label": "Auto", "type": "boolean", "value": True, "rev": 1}],
            "commands": [{"name": "compact", "description": "Compact it", "input": {"hint": "Instructions"}}, {"name": "new_session"}]}
        self.agents.event("resource", {"id": "r2"})

    def test_agent_settings_popup_configure_and_command_use_only_declared_rows(self):
        self.declare_settings()
        menu = self.core.value("menu/agents.r2.settings")
        self.assertEqual((menu["command"], menu["when"]), ("agents.config", {"view": "agents-r2"}))
        self.assertEqual(self.core.live["agents-r2"]["items"][0]["command"], "agents.config", "panel row opens it too")
        self.command("list")
        row = self.core.live["agents-list"]["items"][0]
        self.assertIn({"label": "Agent settings…", "command": "agents.config", "args": {"resource": "r2"}}, row["menu"])
        popup = self.command("config", context={"view": "agents-r2"})
        panel = self.core.live[popup["client"]["popup"]["view"]]
        model = next(r for r in panel["items"] if r.get("type") == "select" and r["text"] == "Model")
        self.assertEqual((model["value"], [o["value"] for o in model["options"]]), ("p/a", ["p/a", "p/b"]))
        self.assertEqual(model["args"], {"resource": "r2", "id": "model", "rev": 3}, "a stale popup choice is rejected, not applied")
        compact = next(r for r in panel["items"] if r.get("text") == "/compact")
        self.assertEqual((compact["command"], compact["input"]), ("agents.command", "Instructions"))

        self.command("configure", **model["args"], value="p/b")
        self.command("configure", resource="r2", id="auto", value=False)
        run = self.core.handlers["agents.command"]
        run({"resource": "r2", "name": "compact", "text": "keep tests"})
        run({"resource": "r2", "name": "new_session", "text": "ignored: no input declared"})
        self.assertEqual([p["args"] for _, p in self.native()], [
            {"v": 0, "id": "model", "value": "p/b", "rev": 3}, {"v": 0, "id": "auto", "value": False},
            {"v": 0, "name": "compact", "args": "keep tests"}, {"v": 0, "name": "new_session"}])
        self.assertIn("has not confirmed", self.core.notices[-3][0], "accepted is not reported as applied")

        for args in ({"id": "model", "value": "p/zzz"}, {"id": "temperature", "value": 1}, {"id": "auto", "value": "yes"}):
            with self.assertRaises(VqroError):
                self.command("configure", resource="r2", **args)
        with self.assertRaises(VqroError):
            run({"resource": "r2", "name": "rm"})
        self.assertEqual(len(self.native()), 4, "undeclared requests send nothing")

    def test_prefix_shift_p_opens_agent_settings_on_agent_panes(self):
        binding = self.core.value("binding/agents.config")
        self.assertEqual((binding["key"], binding["command"], binding["when"]), ("P", "agents.config", {"resource_kind": "agent"}))
        self.declare_settings()
        # What the client sends for the binding on an agent pane: the pane context.
        popup = self.core.handlers["agents.config"]({"context": {"view": "agents-r2", "resource": "r2", "resource_kind": "agent"}})
        self.assertEqual(popup["client"]["popup"]["view"], "agents-settings-r2")

    def test_agents_without_declarations_show_no_settings(self):
        self.core.add_agent()
        self.agents.event("resource", {"id": "r2"})
        self.assertIsNone(self.core.value("menu/agents.r2.settings"))
        self.assertNotIn("agents.config", json.dumps(self.core.live["agents-r2"]))
        with self.assertRaises(VqroError):
            self.command("config", resource="r2")
        # Declared rows without the provider capability are not offered either.
        self.core.resources["r2"]["data"]["agent"] = {"v": 0, "commands": [{"name": "x"}]}
        self.agents.event("resource", {"id": "r2"})
        self.assertIsNone(self.core.value("menu/agents.r2.settings"))

    def test_a_stale_copy_never_overwrites_the_embedded_native_view(self):
        # e2e-agents-ui flake: agents.show published the panel from its older snapshot (no views yet)
        # after the event path had published the embedded view; nothing republished it afterwards.
        self.core.add_agent()
        self.core.resources["r2"]["data"]["views"] = []  # the bridge's first facts: no view declared yet
        stale = copy.deepcopy(self.core.resources["r2"])
        self.core.resources["r2"]["data"]["views"] = ["agent-r2"]
        self.core.live["agent-r2"] = {"items": [{"text": "Ready for a prompt"}]}
        self.agents.publish_panel(self.core.resources["r2"])
        self.agents.publish_panel(stale)
        rows = self.core.live["agents-r2"]["items"]
        self.assertTrue(any(row.get("type") == "view" and row.get("view") == "agent-r2" for row in rows), rows)

    def test_activation_names_the_agent_of_kept_panels(self):
        core = FakeCore()
        core.entries["view/agents-r2"] = {"version": 1, "value": {"owner": "agents", "kind": "panel", "items": []}}
        core.entries["layout/main"] = {"version": 1, "value": {"view": "agents-r2"}}
        Agents(core)
        self.assertEqual(core.value("view/agents-r2")["resource"], "r2")

    def stoppable(self):
        self.core.add_agent()
        self.core.resources["r1"] = {"id": "r1", "kind": "terminal", "name": "Other bridge", "state": "live",
                                     "argv": ["python3", "launch.py"], "provider": "herdr"}
        self.core.providers["herdr"] = {"name": "herdr", "capabilities": {"terminal.kill": {}}}
        self.agents.event("resource", {"id": "r2"})

    def test_stop_asks_first_then_kills_only_the_host_terminal_and_reports_the_outcome(self):
        self.stoppable()
        self.assertEqual(self.core.value("menu/agents.r2.stop")["when"], {"view": "agents-r2"})
        self.assertIsNotNone(self.core.value("view/agents-stop-r2"), "popup declared with the agent, not on click")
        before = len(self.core.transactions)
        popup = self.command("stop", context={"view": "agents-r2"})
        self.assertEqual(popup["client"]["popup"]["view"], "agents-stop-r2")
        self.assertEqual(self.native(), [], "nothing killed before confirmation")
        self.assertEqual(len(self.core.transactions), before, "asking writes no workspace state")
        content = json.dumps(self.core.live["agents-stop-r2"])
        self.assertIn("terminal r1", content)
        self.assertIn("runtime process", content)
        confirm = next(row for row in self.core.live["agents-stop-r2"]["items"] if row.get("command") == "agents.stop")
        result = self.command("stop", **confirm["args"])
        self.assertEqual(self.native(), [("resource.kill", {"resource": "r1"})])
        self.assertEqual(result["client"], {"close": True})
        self.assertIn("kill accepted", self.core.notices[-1][0])

    def test_stop_refuses_a_changed_or_ended_host_and_explains_hostless_agents(self):
        self.stoppable()
        with self.assertRaises(VqroError) as caught:
            self.command("stop", resource="r2", how="host_kill", host="r9", serves=["r2"], confirm=True)
        self.assertEqual(caught.exception.kind, "conflict")
        self.core.resources["r1"]["state"] = "finished"
        with self.assertRaises(VqroError) as caught:
            self.command("stop", resource="r2", how="host_kill", host="r1", serves=["r2"], confirm=True)
        self.assertIn("already ended", str(caught.exception))
        del self.core.resources["r2"]["host"]
        with self.assertRaises(VqroError) as caught:
            self.command("stop", resource="r2")
        self.assertEqual(caught.exception.kind, "unsupported")
        self.assertIn("can't be stopped from vqro", str(caught.exception))
        self.assertIn("(", self.core.notices[-1][0])
        self.assertEqual(self.native(), [])

    def test_agents_list_rows_offer_stop_and_forget_only_when_unavailable(self):
        self.stoppable()
        self.command("list")
        row = next(row for row in self.core.live["agents-list"]["items"] if row.get("args") == {"resource": "r2"})
        self.assertIn({"label": "Stop agent…", "command": "agents.stop", "args": {"resource": "r2"}}, row["menu"])
        self.assertNotIn("agents.forget", json.dumps(row["menu"]))
        self.core.resources["r2"]["available"] = False
        self.command("list")
        row = next(row for row in self.core.live["agents-list"]["items"] if row.get("args") == {"resource": "r2"})
        self.assertIn({"label": "Forget agent", "command": "agents.forget", "args": {"resource": "r2"}}, row["menu"])
        self.assertEqual(row["badges"], [{"text": "unknown", "style": "warn"}])
        del self.core.resources["r2"]["host"]  # no bridge terminal: Stop is not offered, Forget still is
        self.command("list")
        row = next(row for row in self.core.live["agents-list"]["items"] if row.get("args") == {"resource": "r2"})
        self.assertEqual([item["label"] for item in row["menu"]], ["Show", "Forget agent"])

    def test_disconnected_agents_say_unknown_since_or_that_their_bridge_is_dead(self):
        self.stoppable()
        self.core.resources["r2"]["available"] = False
        self.core.providers["other"].update(connected=False, since=1_800_000_000_000)
        self.agents.event("provider", {"name": "other"})
        text = self.core.live["agents-r2"]["items"][0]["text"]
        self.assertIn("state unknown since ", text)
        self.assertIn("its bridge (terminal r1, still running) disconnected", text)
        self.assertIn("keeps it until it reconnects", text)
        self.assertEqual(self.core.value("menu/agents.r2.forget")["command"], "agents.forget")
        self.core.resources["r1"]["state"] = "finished"
        self.agents.event("resource", {"id": "r1"})
        self.assertIn("can't reconnect", self.core.live["agents-r2"]["items"][0]["text"])

    def test_forget_is_explicit_and_only_for_unavailable_agents(self):
        self.stoppable()
        with self.assertRaises(VqroError) as caught:
            self.command("forget", resource="r2")
        self.assertIn("is connected", str(caught.exception))
        self.assertEqual(self.native(), [])
        self.core.resources["r2"]["available"] = False
        result = self.command("forget", context={"view": "agents-r2"})
        self.assertEqual(self.native(), [("resource.forget_cached", {"resource": "r2"})])
        self.assertEqual(result["client"], {"close": True})
        self.assertIn("nothing was sent", self.core.notices[-1][0])

    def test_palette_template_targets_focused_agent(self):
        self.core.add_agent()
        self.core.resources["r4"] = dict(self.core.resources["r2"], id="r4")
        self.agents.event("resource", {})
        template = self.core.value("menu/agents.r2.prompt")
        self.command("prompt", **dict(template.get("args", {}), context={"view": "agents-r4"}, text="focused"))
        self.assertEqual(self.native()[-1][1]["resource"], "r4")


class AgentV0Tests(unittest.TestCase):
    """A runtime declaring agent standard v0 ops is driven with them; legacy runtimes keep legacy ops."""

    def setUp(self):
        self.core = FakeCore()
        self.core.providers["v0"] = {"name": "v0", "capabilities": {
            "agent": {"v": 0}, "agent.send": {"modes": {"start": {}, "settle": {}, "admit": {}}},
            "agent.interrupt": {}, "agent.queue": {}, "agent.respond": {}, "agent.stop": {}}}
        self.agents = Agents(self.core)
        self.core.add_agent()
        self.core.resources["r2"].update({"provider": "v0", "host": None, "data": {"agent": {
            "v": 0, "state": "idle", "lifecycle": "live", "epoch": "e7", "caps": {
                "rev": 3, "agent.send": {"modes": {"start": {"busy": "reject"}, "settle": {"point": "would_stop"},
                                                     "admit": {}}},
                "agent.queue": {"actions": ["list", "clear"]}, "agent.stop": {"covers": ["foreground", "queued"]},
                "agent.interrupt": {}, "agent.respond": {"methods": ["confirm", "permission", "text", "form"]}},
            "requests": [{"request": "q1", "revision": 2, "method": "confirm", "title": "Run it?",
                          "params": {"message": "rm -rf build/ and rebuild"}},
                         {"request": "q2", "revision": 1, "method": "permission", "title": "Push?", "params": {},
                          "choices": [{"id": "allow_once", "label": "Allow"}, {"id": "reject_once", "label": "Reject"}]},
                         {"request": "q3", "revision": 1, "method": "form", "title": "Details", "incomplete": ["params"]},
                         {"request": "q4", "revision": 1, "method": "text", "title": "Old", "status": "answered"}]}}})
        self.agents.event("resource", {"id": "r2"})

    def command(self, name, **args):
        return self.core.handlers["agents."+name](args)

    def native(self):
        return [(m, p) for m, p in self.core.calls if m.startswith("resource.")]

    def test_idle_prompt_sends_start_with_typed_content_and_steer_is_absent_when_undeclared(self):
        self.command("prompt", context={"view": "agents-r2"}, text="go")
        (method, params), = self.native()
        self.assertEqual((method, params["op"]), ("resource.invoke", "agent.send"))
        args = params["args"]
        self.assertEqual((args["v"], args["caps_rev"], args["mode"]), (0, 3, "start"))
        self.assertEqual(args["content"], [{"type": "text", "text": "go"}], "always an array of typed blocks")
        self.assertTrue(args["delivery"].startswith("d:"))
        self.assertEqual(args["origin"]["kind"], "human")
        self.assertIsNone(self.core.value("menu/agents.r2.steer"), "boundary is not declared")
        self.assertIsNotNone(self.core.value("menu/agents.r2.send"))
        self.command("follow_up", resource="r2", text="later")
        self.assertEqual(self.native()[-1][1]["args"]["mode"], "settle")

    def test_busy_prompt_offers_only_declared_modes_and_sends_nothing_until_chosen(self):
        self.core.resources["r2"]["data"]["agent"]["state"] = "running"
        popup = self.command("prompt", context={"view": "agents-r2"}, text="careful")
        self.assertEqual(popup["client"]["popup"]["view"], "agents-send-r2")
        self.assertEqual(self.native(), [], "no silent fallback to another mode")
        rows = [row for row in self.core.live["agents-send-r2"]["items"] if row.get("command") == "agents.send"]
        self.assertEqual([row["args"]["mode"] for row in rows], ["start", "settle", "admit"])
        self.assertIn("weaker", rows[2]["description"])
        self.command("send", **rows[1]["args"])
        sent = self.native()[-1][1]["args"]
        self.assertEqual((sent["mode"], sent["content"]), ("settle", [{"type": "text", "text": "careful"}]))
        with self.assertRaises(VqroError) as caught:
            self.command("send", resource="r2", mode="boundary", text="x")
        self.assertEqual(caught.exception.kind, "unsupported")
        self.assertEqual(len(self.native()), 1)

    def test_interrupt_and_queue_clear_use_v0_ops(self):
        self.command("abort", resource="r2")
        self.command("clear_queue", resource="r2")
        self.assertEqual([(p["op"], p["args"]) for _, p in self.native()],
                         [("agent.interrupt", {"v": 0, "caps_rev": 3}),
                          ("agent.queue", {"v": 0, "caps_rev": 3, "action": "clear", "epoch": "e7"})])

    def test_respond_carries_epoch_and_revision_and_incomplete_requests_only_cancel(self):
        self.command("answer", resource="r2")
        items = self.core.live["agents-requests-r2"]["items"]
        self.assertTrue(any("can't be approved" in row.get("text", "") for row in items))
        shown = {(row["args"]["request"], bool(row["args"].get("cancelled"))): row["args"] for row in items if row.get("command")}
        self.assertEqual(shown[("q1", False)], {"resource": "r2", "request": "q1", "epoch": "e7", "revision": 2})
        self.command("answer", **shown[("q1", False)], value=True)
        self.command("answer", **shown[("q2", False)], value="allow_once")
        self.command("answer", **shown[("q3", True)])
        with self.assertRaises(VqroError) as caught:
            self.command("answer", resource="r2", request="q1", value=True)
        self.assertEqual(caught.exception.kind, "stale", "an answer without the shown epoch/revision is never sent")
        self.assertEqual([p["args"] for _, p in self.native()], [
            {"v": 0, "request": "q1", "response": {"value": True}, "revision": 2, "epoch": "e7"},
            {"v": 0, "request": "q2", "response": {"choice": "allow_once"}, "revision": 1, "epoch": "e7"},
            {"v": 0, "request": "q3", "response": {"action": "cancel"}, "revision": 1, "epoch": "e7"}])
        self.assertNotIn("Old", json.dumps(items), "only pending requests are offered")
        for bad in ({"request": "q4", "text": "late"}, {"request": "q3", "value": True}, {"request": "q2", "value": "always"}, {"request": "gone", "value": True}):
            with self.assertRaises(VqroError):
                self.command("answer", resource="r2", epoch="e7", revision=1, **bad)
        self.assertEqual(len(self.native()), 3)

    def test_graceful_stop_is_offered_without_a_host_and_asks_first(self):
        popup = self.command("stop", resource="r2")
        self.assertEqual(self.native(), [])
        rows = [row for row in self.core.live[popup["client"]["popup"]["view"]]["items"] if row.get("command")]
        self.assertEqual([row["args"].get("how") for row in rows], ["graceful"], "no host terminal: no kill offered")
        self.assertIn("foreground, queued", json.dumps(self.core.live["agents-stop-r2"]))
        self.command("stop", **rows[0]["args"])
        self.assertEqual(self.native(), [("resource.invoke", {"resource": "r2", "op": "agent.stop",
                                                           "args": {"v": 0, "caps_rev": 3, "covers": ["foreground", "queued"]}})])
        self.assertIn("ended only when the runtime reports", self.core.notices[-1][0])

    def test_list_status_uses_v0_lifecycle_and_state(self):
        self.command("list")
        row = self.core.live["agents-list"]["items"][0]
        self.assertEqual(row["description"], "idle")
        self.core.resources["r2"]["data"]["agent"]["lifecycle"] = "stopping"
        self.agents.event("resource", {"id": "r2"})
        self.assertEqual(self.core.live["agents-list"]["items"][0]["description"], "stopping")


class AgentV0ConsentTests(unittest.TestCase):
    """Consent is to what was shown (K3a review probes, spike-panel-2): a popup row never rebinds to newer facts,
    and nothing is inferred beyond the agent's own evidence. FakeCore records intent only; nothing native runs."""

    setUp, command, native = AgentV0Tests.setUp, AgentV0Tests.command, AgentV0Tests.native

    def facts(self):
        return self.core.resources["r2"]["data"]["agent"]

    def refused(self, kind, name, **args):
        with self.assertRaises(VqroError) as caught:
            self.command(name, **args)
        self.assertEqual(caught.exception.kind, kind)
        self.assertEqual(self.native(), [], "nothing sent")

    def row(self, view, **match):
        return copy.deepcopy(next(row for row in self.core.live[view]["items"] if row.get("command")
                                  and all(row["args"].get(k) == v for k, v in match.items())))

    def test_an_old_answer_is_not_rebound_to_a_revised_request(self):
        self.command("answer", resource="r2")
        row = self.row("agents-requests-r2", request="q1", cancelled=None)
        self.facts()["requests"][0].update(revision=3, title="Destroy a different resource?")
        self.refused("stale", "answer", **row["args"], value=True)
        self.assertIn("Destroy", json.dumps(self.core.live["agents-requests-r2"]), "the open popup shows the new question")

    def test_an_old_answer_is_not_rebound_to_a_new_epoch(self):
        self.command("answer", resource="r2")
        row = self.row("agents-requests-r2", request="q1", cancelled=None)
        self.facts()["epoch"] = "e8"
        self.refused("stale", "answer", **row["args"], value=True)
        cancel = self.row("agents-requests-r2", request="q2", cancelled=True)
        self.assertEqual(cancel["args"]["epoch"], "e8")

    def test_a_confirmed_graceful_stop_never_widens_its_coverage(self):
        self.command("stop", resource="r2")
        row = self.row("agents-stop-r2", how="graceful")
        self.facts()["caps"]["rev"] = 4
        self.facts()["caps"]["agent.stop"]["covers"].append("children")
        self.refused("stale", "stop", **row["args"])
        self.assertIn("children", json.dumps(self.core.live["agents-stop-r2"]), "re-shown with the new coverage")

    def test_a_mode_choice_keeps_the_caps_revision_it_was_shown(self):
        self.facts()["state"] = "running"
        self.command("prompt", resource="r2", text="go")
        row = self.row("agents-send-r2", mode="settle")
        self.assertEqual(row["args"]["caps_rev"], 3)
        self.facts()["caps"]["rev"] = 4
        self.facts()["caps"]["agent.send"]["modes"]["settle"]["point"] = "settled"
        self.refused("stale", "send", **row["args"])

    def host(self, owns=True):
        self.core.providers["term"] = {"capabilities": {"terminal.kill": {}}}
        self.core.resources["r1"] = {"id": "r1", "provider": "term", "kind": "terminal", "state": "live",
                                     "available": True, "argv": ["server"]}
        self.core.resources["r2"]["host"] = "r1"
        self.facts()["host"] = {"id": "h1", "provider": "term", "instance": "x", "incarnation": "i1", "kind": "terminal",
                                "resource": "r1", "owns_lifecycle": owns}

    def test_host_kill_is_reconfirmed_when_the_host_serves_a_new_agent(self):
        self.host()
        self.command("stop", resource="r2")
        row = self.row("agents-stop-r2", how="host_kill")
        self.assertEqual(row["args"]["serves"], ["r2"])
        self.core.resources["r3"] = dict(self.core.resources["r2"], id="r3", name="Not shown when consented")
        self.refused("conflict", "stop", **row["args"])
        self.assertIn("r3", json.dumps(self.core.live["agents-stop-r2"]), "the new agent is disclosed before any kill")

    def test_an_unknown_stop_action_never_falls_through_to_a_kill(self):
        self.host()
        self.refused("invalid", "stop", resource="r2", how="future_graceful", host="r1", serves=["r2"], confirm=True)
        self.refused("invalid", "stop", resource="r2", host="r1", serves=["r2"], confirm=True)

    def test_a_non_owning_host_kill_does_not_promise_the_runtime_ends(self):
        self.host(owns=False)
        self.command("stop", resource="r2")
        text = json.dumps(self.core.live["agents-stop-r2"])
        self.assertNotIn("ends the agent's runtime process", text)
        self.assertIn("may continue", text)
        self.command("stop", **self.row("agents-stop-r2", how="host_kill")["args"])
        self.assertIn("unknown until its runtime reports", self.core.notices[-1][0])

    def test_missing_or_cut_state_is_not_idle_even_if_the_coarse_status_is_done(self):
        del self.facts()["state"]
        self.facts()["incomplete"] = ["state"]
        self.core.resources["r2"]["status"] = {"state": "done"}
        result = self.command("prompt", resource="r2", text="go")
        self.assertEqual((result["client"]["popup"]["view"], self.native()), ("agents-send-r2", []))

    def test_a_stopping_agent_is_not_idle(self):
        self.facts()["lifecycle"] = "stopping"
        result = self.command("prompt", resource="r2", text="go")
        self.assertEqual((result["client"]["popup"]["view"], self.native()), ("agents-send-r2", []))

    def test_the_provider_bound_does_not_enable_an_op_this_agent_lacks(self):
        del self.facts()["caps"]["agent.interrupt"]
        self.agents.event("resource", {"id": "r2"})
        self.assertIsNone(self.core.value("menu/agents.r2.abort"))
        self.refused("unsupported", "abort", resource="r2")


class AgentV0RequestDetailTests(unittest.TestCase):
    """A request's native details are the question: shown before any approval, or no approval (A1)."""
    setUp, command, native = AgentV0Tests.setUp, AgentV0Tests.command, AgentV0Tests.native

    def panel(self):
        self.command("answer", resource="r2")
        return self.core.live["agents-requests-r2"]["items"]

    def test_a_confirmation_shows_its_message_before_yes(self):
        items = self.panel()
        texts = [row.get("text") for row in items]
        message, yes = texts.index("rm -rf build/ and rebuild"), texts.index("Answer: Run it?")
        self.assertLess(texts.index("Run it?"), message, "the title, then the native message")
        self.assertLess(message, yes, "the native message comes before the row that approves")
        self.assertEqual(items[yes]["options"][0], {"label": "Yes", "value": True})

    def test_without_details_only_cancel_is_offered_and_approval_is_refused(self):
        del self.core.resources["r2"]["data"]["agent"]["requests"][0]["params"]
        items = self.panel()
        self.assertFalse(any(row.get("options") for row in items if row.get("args", {}).get("request") == "q1"))
        self.assertTrue(any("details not available" in row.get("text", "") for row in items))
        with self.assertRaises(VqroError):
            self.command("answer", resource="r2", request="q1", epoch="e7", revision=2, value=True)
        self.command("answer", resource="r2", request="q1", epoch="e7", revision=2, cancelled=True)
        self.assertEqual(self.native()[-1][1]["args"]["response"], {"action": "cancel"})

    def test_text_requests_carry_prefill_and_multiline_and_choices_show_their_effect(self):
        self.core.resources["r2"]["data"]["agent"]["requests"] = [
            {"request": "q5", "revision": 1, "method": "text", "title": "Commit message",
             "params": {"prefill": "fix: lint", "multiline": True, "placeholder": "Message"}},
            {"request": "q6", "revision": 1, "method": "permission", "title": "Push?", "params": {"command": "git push"},
             "choices": [{"id": "a", "label": "Allow", "effect": "allow_always"}]}]
        items = self.panel()
        text = next(row for row in items if row.get("input"))
        self.assertEqual((text["input"], text["prefill"], text["multiline"]), ("Message", "fix: lint", True))
        self.assertIn("command: git push", [row.get("text") for row in items])
        push = next(row for row in items if row.get("options"))
        self.assertEqual(push["options"], [{"label": "1. Allow always: Allow", "value": "a"}])
        self.assertIn("Choice 1 (allow always, id a): Allow", [row.get("text") for row in items])

    def check_whole_question(self, params):
        """K3a P1 (spike-panel-2): a legal summary whose presentation grows (escaped controls, JSON) is never
        clipped while Yes is offered: every byte of the question is shown, across rows, or only Cancel is."""
        q = self.core.resources["r2"]["data"]["agent"]["requests"][0]
        q["params"] = params
        self.core.resources["r2"]["data"]["agent"]["requests"] = [q]
        self.assertLessEqual(len(json.dumps(q, ensure_ascii=False, separators=(",", ":")).encode()), 2048)
        items = self.panel()
        shown = "".join(row.get("text", "") for row in items if row.get("type") == "text")
        approvable = any(row.get("options") for row in items)
        self.assertTrue(approvable)
        self.assertIn("DELETE /sensitive-workspace !", shown)
        self.assertTrue(all(len(row.get("text", "").encode()) <= 512 for row in items if row.get("type") == "text"))
        self.assertIn("wrap", self.core.live["agents-requests-r2"]["requires"], "a client that can't wrap can't show it")
        return shown

    def test_escaped_controls_never_push_the_question_out_of_view(self):
        shown = self.check_whole_question({"message": "DELETE /sensitive-workspace !" + "\x85" * 800})
        self.assertTrue(shown.endswith("\\x85" * 10), "every escaped byte is shown, none dropped")

    def test_json_params_are_shown_whole(self):
        shown = self.check_whole_question({"tool": {"command": "DELETE /sensitive-workspace !", "arguments": [0] * 700}})
        self.assertIn(",0]}", shown)

    def test_a_question_too_long_to_show_whole_is_cancel_only(self):
        q = self.core.resources["r2"]["data"]["agent"]["requests"][0]
        q["params"] = {"message": "\x01" * 5000}
        items = self.panel()
        self.assertFalse(any(row.get("options") for row in items if row.get("args", {}).get("request") == "q1"))
        self.assertTrue(any("too long to show" in row.get("text", "") for row in items))
        with self.assertRaises(VqroError):
            self.command("answer", resource="r2", request="q1", epoch="e7", revision=2, value=True)

    def k3a_permission(self, choices, params=None):
        q = self.core.resources["r2"]["data"]["agent"]["requests"][0]
        q.update(method="permission", params=params or {"message": "Authorize this operation?"}, choices=choices)
        self.core.resources["r2"]["data"]["agent"]["requests"] = [q]
        return self.panel()

    def test_long_permission_labels_never_hide_which_choice_is_which(self):
        """K3a delta (spike-panel-2): head-cut labels must not make allow_once and allow_always look the same."""
        long = "Allow " + "this operation " * 16
        items = self.k3a_permission([{"id": "a", "label": long, "effect": "allow_once"},
                                     {"id": "b", "label": long, "effect": "allow_always"}])
        options = next(row for row in items if row.get("options"))["options"]
        self.assertTrue(options[0]["label"].startswith("1. Allow once: ") and options[1]["label"].startswith("2. Allow always: "))
        texts = [row.get("text", "") for row in items]
        self.assertTrue(any(t.startswith("Choice 2 (allow always, id b): Allow this") for t in texts), "each choice whole")
        self.command("answer", resource="r2", request="q1", epoch="e7", revision=2, value="b")
        self.assertEqual(self.native()[-1][1]["args"]["response"], {"choice": "b"})

    def test_choices_count_in_the_same_disclosure_budget(self):
        items = self.k3a_permission([{"id": "b", "label": "X" * 17000 + "SENSITIVE_SUFFIX", "effect": "allow_always"}])
        self.assertFalse(any(row.get("options") for row in items))
        with self.assertRaises(VqroError):
            self.command("answer", resource="r2", request="q1", epoch="e7", revision=2, value="b")

    def test_an_explicit_null_param_is_shown(self):
        q = self.core.resources["r2"]["data"]["agent"]["requests"][0]
        q["params"] = {"message": "Authorize access?", "expiry": None}
        self.assertIn("expiry: null", [row.get("text") for row in self.panel()])

    def test_a_cut_or_missing_lifecycle_is_not_idle(self):
        facts = self.core.resources["r2"]["data"]["agent"]
        del facts["lifecycle"]
        facts["incomplete"] = ["lifecycle"]
        result = self.command("prompt", resource="r2", text="go")
        self.assertEqual((result["client"]["popup"]["view"], self.native()), ("agents-send-r2", []))
        facts["incomplete"] = []
        result = self.command("prompt", resource="r2", text="go")
        self.assertEqual((result["client"]["popup"]["view"], self.native()), ("agents-send-r2", []))


class Tier1StatusTests(unittest.TestCase):
    def setUp(self):
        self.core = FakeCore()
        self.agents = Agents(self.core)
        self.core.resources["r1"] = {"id": "r1", "kind": "terminal", "state": "live", "argv": ["claude"]}
        self.report = self.core.handlers["agents.status"]

    def test_reports_publish_osc_shaped_status_last_writer_by_seq(self):
        self.assertEqual(self.report({"terminal": "r1", "app": "claude", "state": "running", "seq": 10})["applied"], True)
        entry = self.core.live["agents.terminal_status"]["data"]["r1"]
        self.assertEqual((entry["state"], entry["status"]), ("running", {"state": "working", "app": "claude"}))
        self.assertEqual(self.report({"terminal": "r1", "app": "claude", "state": "idle", "seq": 10})["reason"], "stale")
        self.report({"terminal": "r1", "app": "claude", "state": "sparkling", "seq": 11})
        self.assertEqual(self.core.live["agents.terminal_status"]["data"]["r1"]["state"], "unknown", "no state without evidence")
        self.assertEqual([m for m, _ in self.core.calls if m.startswith("resource.")], [], "advisory: nothing native")

    def test_bad_reports_are_refused_to_the_caller_without_notices(self):
        for bad in ({"terminal": "r9", "app": "c", "state": "idle", "seq": 1}, {"terminal": "r1", "app": "", "seq": 1},
                    {"terminal": "r1", "app": "c", "state": "idle", "seq": -1}, {"terminal": "r1", "app": "c", "seq": "1"}):
            with self.assertRaises(VqroError):
                self.report(bad)
        self.assertEqual(self.core.notices, [])
        self.assertNotIn("agents.terminal_status", self.core.live)

    def test_a_late_publication_never_overwrites_a_newer_one(self):
        """K3a delta N1: reports run concurrently; the visible table always ends at the newest state."""
        entered, release, original, errors = threading.Event(), threading.Event(), self.core.publish, []

        def publish(view, content):
            if view == "agents.terminal_status" and content["data"]["r1"]["seq"] == 1:
                entered.set()
                self.assertTrue(release.wait(5))
            return original(view, content)
        self.core.publish = publish

        def older():
            try:
                self.report({"terminal": "r1", "app": "hook", "state": "running", "seq": 1})
            except BaseException as error:
                errors.append(error)
        thread = threading.Thread(target=older)
        thread.start()
        try:
            self.assertTrue(entered.wait(5))
            self.report({"terminal": "r1", "app": "hook", "state": "idle", "seq": 2})
        finally:
            release.set()
            thread.join(5)
        self.assertEqual(errors, [])
        self.assertEqual(self.core.live["agents.terminal_status"]["data"]["r1"]["state"], "idle")
        self.report({"terminal": "r1", "app": "hook", "state": "idle", "seq": 3})
        self.assertEqual(self.core.live["agents.terminal_status"]["data"]["r1"]["state"], "idle")

    def test_a_failed_publication_is_retried_by_the_next_report_even_if_unchanged(self):
        """K3a P2 (spike-panel-2): one failed publication never leaves the table unpublished."""
        original = self.core.publish

        def fail(view, content):
            raise VqroError("injected", "overloaded")
        self.core.publish = fail
        with self.assertRaises(VqroError):
            self.report({"terminal": "r1", "app": "hook", "state": "idle", "seq": 1})
        self.core.publish = original
        self.report({"terminal": "r1", "app": "hook", "state": "idle", "seq": 2})
        self.assertEqual(self.core.live["agents.terminal_status"]["data"]["r1"]["seq"], 2)

    def test_a_same_seq_retry_after_a_failed_publication_flushes_the_table(self):
        original = self.core.publish

        def fail(view, content):
            raise VqroError("injected", "overloaded")
        self.core.publish = fail
        report = {"terminal": "r1", "app": "hook", "state": "idle", "seq": 1}
        with self.assertRaises(VqroError):
            self.report(report)
        self.core.publish = original
        self.assertEqual(self.report(report)["reason"], "stale", "not recorded twice")
        self.assertEqual(self.core.live["agents.terminal_status"]["data"]["r1"]["seq"], 1)

    def test_the_published_table_stays_under_the_live_value_cap(self):
        """K3a delta N2: admission counts encoded bytes, so a full table still publishes (1 MiB core cap)."""
        from agents import TIER1_BYTES
        app = "\U0001F600" * 64
        for n in range(1, 5000):
            rid = f"r{n}"
            self.core.resources[rid] = {"id": rid, "kind": "terminal", "state": "live", "argv": ["sh"]}
            try:
                self.report({"terminal": rid, "app": app, "state": "requires_action", "seq": 2_000_000_000_000})
            except VqroError as error:
                self.assertEqual(error.kind, "overloaded")
                break
        else:
            self.fail("admission never refused")
        self.assertGreater(n, 1000, "refused by a bound, not early")
        size = len(json.dumps(self.core.live["agents.terminal_status"], ensure_ascii=False, separators=(",", ":")).encode())
        self.assertLessEqual(size, 1024 * 1024)
        self.assertNotIn(rid, self.agents.tier1, "a refused report leaves nothing behind")
        self.assertLessEqual(sum(self.agents.tier1_size.values()), TIER1_BYTES)

    def test_the_declared_count_of_worst_case_entries_fits_the_cap(self):
        from agents import TIER1_MAX, TIER1_SEQ_MAX
        app, rid = "\U0001F600" * 64, "r" + "9" * 63
        entry = {"app": app, "state": "requires_action", "seq": TIER1_SEQ_MAX, "at_ms": 10 ** 13 - 1,
                 "status": {"state": "blocked", "app": app}}
        table = {"data": {rid[:-4] + f"{n:04d}": entry for n in range(TIER1_MAX)}}
        self.assertLessEqual(len(json.dumps(table, ensure_ascii=False, separators=(",", ":")).encode()), 1024 * 1024)
        for bad in ({"seq": TIER1_SEQ_MAX + 1}, {"app": "a\x1b[31m"}, {"terminal": "r" * 65}):
            with self.assertRaises(VqroError):
                self.report({"terminal": "r1", "app": "hook", "state": "idle", "seq": 1, **bad})

    def test_an_ended_or_removed_terminal_drops_its_report(self):
        self.report({"terminal": "r1", "app": "claude", "state": "idle", "seq": 1})
        del self.core.resources["r1"]
        self.agents.event("resource.removed", {"id": "r1"})
        self.assertEqual(self.core.live["agents.terminal_status"]["data"], {})


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
