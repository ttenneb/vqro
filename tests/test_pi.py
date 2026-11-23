import json
import os
import subprocess
import tempfile
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

EXT = Path(os.environ.get("VQRO_TEST_EXTENSIONS_DIR", Path(__file__).resolve().parents[1] / "extensions"))
sys.path.insert(0, str(EXT))
from pi import Pi, DEFAULTS, EXTENSIONS, LEGACY, SETTINGS, default_bridge, runtimes
from vqro_ext import VqroError
from agent_fixture import FakeCore


class PiTests(unittest.TestCase):
    def setUp(self):
        self.core = FakeCore("pi")
        self.pi = Pi(self.core)

    def command(self, name, **args):
        return self.core.handlers["pi."+name](args)

    def native(self):
        return [(m, p) for m, p in self.core.calls if m == "resource.spawn"]

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
        self.assertEqual(spawn["argv"][:3], default_bridge())
        self.assertEqual(spawn["argv"][3:], ["--", "/node", "literal $(not-a-shell)", "--mode", "rpc"])
        self.assertRegex(spawn["env"]["VQRO_PRINCIPAL"], r"^p:[0-9a-f]{32}$")
        self.assertEqual(set(spawn["env"]), {"VQRO_PRINCIPAL"})
        self.assertEqual(spawn["cwd"], "/private/project")
        self.assertEqual(self.core.calls[-1], ("agents.show", {"resource": "r2"}))
        self.assertEqual(len(self.native()), 1)

    def test_new_uses_only_scoped_reads_even_for_source_cwd(self):
        self.core.resources["r88"]={"id":"r88","cwd":"/private/source"}
        with patch.object(self.core,"fresh",side_effect=AssertionError("full snapshot is not a scoped launch read")):
            self.command("new",context={"resource":"r88"})
        self.assertIn(("workspace.get",{"key":SETTINGS}),self.core.calls)
        self.assertIn(("resource.list",{"id":"r88"}),self.core.calls)
        self.assertIn(("resource.list",{"host":"r1"}),self.core.calls)
        self.assertEqual(self.native()[0][1]["cwd"],"/private/source")

    def test_transient_agent_lookup_retries_only_the_read_not_spawn(self):
        original=self.core.call;reads=[]
        def call(method,params):
            if method=="resource.list" and "host" in params:
                reads.append(params)
                if len(reads)==1:raise VqroError("busy","overloaded")
            return original(method,params)
        with patch.object(self.core,"call",side_effect=call),patch("pi.time.sleep"):
            result=self.command("new")
        self.assertEqual(result["agent"],"r2");self.assertEqual(len(reads),2);self.assertEqual(len(self.native()),1)

    def test_agent_lookup_failure_reports_already_observed_host_without_retrying_spawn(self):
        original=self.core.call
        def call(method,params):
            if method=="resource.list" and "host" in params:raise VqroError("busy","overloaded")
            return original(method,params)
        with patch.object(self.core,"call",side_effect=call),patch("pi.time.monotonic",side_effect=[0,13]):
            result=self.command("new")
        self.assertEqual((result["outcome"],result["host"],result["ready"]),("observed","r1",False))
        self.assertEqual(len(self.native()),1)
        self.assertTrue(any("Do not blindly retry" in text and "spawned" in text for text,_ in self.core.notices))

    def test_post_spawn_ui_failure_preserves_host_and_agent_result(self):
        original=self.core.invoke
        def invoke(name,args=None):
            if name=="agents.show":raise VqroError("busy","overloaded")
            return original(name,args)
        with patch.object(self.core,"invoke",side_effect=invoke):
            result=self.command("new")
        self.assertEqual((result["outcome"],result["host"],result["agent"]),("observed","r1","r2"))
        self.assertIn(("tabs.show",{"resource":"r1"}),self.core.calls);self.assertEqual(len(self.native()),1)

    def test_default_does_not_infer_bridge_host_from_core_platform(self):
        with patch("pi.sys.platform", "win32"):
            self.assertEqual(default_bridge(), ["python3", "-I", str(Path(EXTENSIONS)/"pi_bridge"/"launch.py")])
        value = {"runtimes": [{"name": "windows host", "argv": ["pi1"], "bridge_argv": ["py", "-3", "-m", "pi_bridge"]}]}
        self.assertEqual(runtimes(value)[0]["bridge_argv"], ["py", "-3", "-m", "pi_bridge"])

    def test_default_bridge_never_imports_project_or_pythonpath_shadow_modules(self):
        for shadow in ("uuid.py", "queue.py", "pi_bridge/__init__.py"):
            with self.subTest(shadow=shadow), tempfile.TemporaryDirectory(prefix="bridge-import-") as project:
                source=Path(project)/shadow;source.parent.mkdir(parents=True,exist_ok=True)
                source.write_text("open('shadow-executed','w').write('bad')\nraise SystemExit('project module executed')\n")
                binary=Path(project)/"bin"/"vqro-bridge-pi";binary.parent.mkdir();binary.write_text("#!/bin/sh\nexit 0\n");binary.chmod(0o755)
                env={"HOME":project,"PATH":str(binary.parent),"PYTHONPATH":os.pathsep.join([project,EXTENSIONS])}
                result=subprocess.run([sys.executable,*default_bridge()[1:],"--help"],cwd=project,env=env,capture_output=True,text=True,timeout=5)
                self.assertEqual(result.returncode,0,result.stderr)
                self.assertFalse((Path(project)/"shadow-executed").exists())

    def test_invalid_host_launcher_is_rejected_before_spawn(self):
        for extra in ({"bridge_argv": "python -m pi_bridge"}, {"bridge_env": {"PYTHONPATH": None}}):
            with self.assertRaises(VqroError):
                runtimes({"runtimes": [{"name": "bad", "argv": ["pi1"], **extra}]})
        self.assertEqual(self.native(), [])

    def test_bridge_interpreter_is_resolved_on_terminal_host(self):
        self.command("new")
        self.assertEqual(self.native()[0][1]["argv"][0], "python3")

    def test_configured_host_launcher_and_environment_are_literal(self):
        self.core.apply([{"op": "set", "key": SETTINGS, "value": {"runtimes": [{"name": "remote", "argv": ["pi-host", "--mode", "rpc"],
            "bridge_argv": ["/Host Tools/python", "-m", "pi_bridge"], "bridge_env": {"PYTHONPATH": "/Host Modules"}}]}}], "host")
        self.command("new")
        spawn = self.native()[0][1]
        self.assertEqual(spawn["argv"], ["/Host Tools/python", "-m", "pi_bridge", "--", "pi-host", "--mode", "rpc"])
        self.assertEqual(spawn["env"]["PYTHONPATH"], "/Host Modules")
        self.assertRegex(spawn["env"]["VQRO_PRINCIPAL"], r"^p:[0-9a-f]{32}$")

    def test_custom_host_launcher_does_not_inject_core_module_path(self):
        self.core.apply([{"op": "set", "key": SETTINGS, "value": {"runtimes": [{"name": "host", "argv": ["pi-host"],
            "bridge_argv": ["vqro-bridge-pi"]}]}}], "host")
        self.command("new")
        self.assertEqual(set(self.native()[0][1]["env"]), {"VQRO_PRINCIPAL"})

    def test_multiple_launches_name_the_bridge_not_the_interpreter(self):
        self.command("new")
        self.command("new")
        spawns = [params for method, params in self.native() if method == "resource.spawn"]
        self.assertEqual(len(spawns), 2)
        self.assertNotEqual(spawns[0]["env"]["VQRO_PRINCIPAL"],spawns[1]["env"]["VQRO_PRINCIPAL"])
        self.assertTrue(all(params["name"].startswith("pi-bridge-") for params in spawns))
        self.assertNotEqual(spawns[0]["name"], spawns[1]["name"])

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
            with self.assertRaises(VqroError):
                runtimes(value)
        before = self.core.value(SETTINGS)
        with self.assertRaises(VqroError):
            self.command("configure", text="not JSON")
        self.assertEqual(self.core.value(SETTINGS), before)
        self.command("configure", text='[{"name":"fake","argv":["/python","fixture"]}]')
        self.assertEqual(self.core.value(SETTINGS)["runtimes"][0]["name"], "fake")
        self.assertEqual(self.native(), [])


if __name__ == "__main__":
    unittest.main()
