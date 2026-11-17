import json
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
from pi import Pi, DEFAULTS, EXTENSIONS, LEGACY, SETTINGS, runtimes
from prim_ext import PrimError
from agent_fixture import FakeCore


class PiTests(unittest.TestCase):
    def setUp(self):
        self.core = FakeCore("pi")
        self.pi = Pi(self.core)

    def command(self, name, **args):
        return self.core.handlers["pi."+name](args)

    def native(self):
        return [(m, p) for m, p in self.core.calls if m.startswith("resource.")]

    def test_defaults_and_owned_launcher_only(self):
        self.assertEqual(self.core.value(SETTINGS), DEFAULTS)
        self.assertEqual(self.core.value("binding/pi.new")["key"], "a")
        self.assertEqual(self.core.value("menu/pi.new")["owner"], "pi")
        self.assertNotIn("agents.prompt", self.core.handlers)
        self.assertEqual(self.native(), [])

    def test_new_spawns_once_with_literal_argv_then_public_agents_show(self):
        self.core.apply([{"op": "set", "key": SETTINGS, "value": {"runtimes": [{"name": "custom", "argv": ["/node", "literal $(not-a-shell)", "--mode", "rpc"]}]}}], "settings")
        result = self.command("new", cwd="/private/project")
        self.assertEqual((result["outcome"], result["agent"]), ("observed", "r2"))
        spawn = self.native()[0][1]
        self.assertTrue(spawn["bridge"])
        self.assertEqual(spawn["argv"][1:4], ["-m", "pi_bridge", "--"])
        self.assertEqual(spawn["argv"][4:], ["/node", "literal $(not-a-shell)", "--mode", "rpc"])
        self.assertEqual(spawn["env"], {"PYTHONPATH": EXTENSIONS})
        self.assertEqual(spawn["cwd"], "/private/project")
        self.assertEqual(self.core.calls[-1], ("agents.show", {"resource": "r2"}))
        self.assertEqual(len(self.native()), 1)

    def test_multiple_launches_leave_backend_naming_to_backend(self):
        self.command("new")
        self.command("new")
        spawns = [params for method, params in self.native() if method == "resource.spawn"]
        self.assertEqual(len(spawns), 2)
        self.assertTrue(all("name" not in params for params in spawns))

    def test_agents_disabled_falls_back_to_real_terminal_without_respawning(self):
        self.core.fail_agents = True
        result = self.command("new")
        self.assertEqual(result["outcome"], "observed")
        self.assertIn(("tabs.show", {"resource": "r1"}), self.core.calls)
        self.assertEqual(len(self.native()), 1)

    def test_runtime_picker_does_not_spawn_before_selection(self):
        self.core.apply([{"op": "set", "key": SETTINGS, "value": {"runtimes": [{"name": "one", "argv": ["/one"]}, {"name": "two", "argv": ["/two"]}]}}], "settings")
        result = self.command("new")
        self.assertEqual(result["client"]["popup"]["view"], "pi-runtimes")
        self.assertEqual(self.native(), [])
        self.command("new", runtime="two")
        self.assertEqual(self.native()[0][1]["argv"][-1], "/two")

    def test_late_agent_retains_observed_terminal_and_never_repeats_spawn(self):
        self.core.make_agent = False
        with patch("pi.time.monotonic", side_effect=[0, 13]):
            result = self.command("new")
        self.assertFalse(result["ready"])
        self.assertEqual(result["outcome"], "observed")
        self.assertEqual(len(self.native()), 1)

    def test_migration_copies_legacy_runtime_once_and_preserves_source(self):
        core = FakeCore("pi")
        old = {"runtimes": [{"name": "saved", "argv": ["/saved"]}], "other": "preserve"}
        core.apply([{"op": "set", "key": LEGACY, "value": old}], "legacy")
        Pi(core)
        self.assertEqual(core.value(SETTINGS), {"runtimes": old["runtimes"]})
        self.assertEqual(core.value(LEGACY), old)
        core.contributed = False
        core.apply([{"op": "set", "key": LEGACY, "value": DEFAULTS}], "legacy changed")
        Pi(core)
        self.assertEqual(core.value(SETTINGS)["runtimes"][0]["name"], "saved")

    def test_existing_destination_is_never_overwritten(self):
        core = FakeCore("pi")
        core.apply([{"op": "set", "key": SETTINGS, "value": {"runtimes": [{"name": "new", "argv": ["/new"]}]}},
                    {"op": "set", "key": LEGACY, "value": DEFAULTS}], "existing")
        Pi(core)
        self.assertEqual(core.value(SETTINGS)["runtimes"][0]["name"], "new")

    def test_configuration_validates_before_write_or_launch(self):
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
