"""tier1 launcher extension (extensions/tier1.py) and its terminal wrapper (extensions/tier1_run.py), with a fake core."""
import json
import os
from pathlib import Path
import stat
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

EXT = Path(os.environ.get("VQRO_TEST_EXTENSIONS_DIR", Path(__file__).resolve().parents[1] / "extensions"))
sys.path.insert(0, str(EXT))
sys.path.insert(0, str(Path(__file__).resolve().parent))
import tier1  # noqa: E402
import tier1_run  # noqa: E402
from agent_fixture import FakeCore  # noqa: E402
from vqro_ext import VqroError  # noqa: E402

PULL = {"points": {"turn": {"precision": "exact"}, "idle": {"precision": "approximate"}},
        "insert": "context", "evidence": "none"}


class Tier1Tests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="vq-tier1-")
        root = Path(self.temp.name)
        self.home, self.state = root / "home", root / "state"
        self.home.mkdir()
        self.env = patch.dict(os.environ, {"HOME": str(self.home), "VQRO_STATE_DIR": str(self.state)})
        self.env.start()
        self.hook = root / "vqro-claude-hook"
        self.hook.write_text("import json, sys\n"
            f"pull = {PULL!r}\n"
            "value = {'pull': pull} if sys.argv[1] == 'declaration' else "
            "{'hooks': {'Stop': [{'hooks': [{'type': 'command', 'command': sys.argv[2], 'args': ['hook']}]}]}}\n"
            "print(json.dumps(value))\n", encoding="utf-8")
        run_hook = tier1.Tier1.run
        def hook_output(argv):
            self.assertEqual(argv[0], str(self.hook))
            return run_hook([sys.executable, *argv])
        self.core = FakeCore("tier1")
        self.core.make_agent = False
        self.coord_calls, self.refuse = [], set()
        original = self.core.invoke

        def invoke(name, args=None):
            if name.startswith("coord."):
                self.coord_calls.append((name, dict(args or {})))
                if name in self.refuse:
                    raise VqroError("no_grant: resume needs a grant", "forbidden")
                if name == "coord.inbox.window":
                    return {"epoch": "w:1"}
                if name == "coord.scope" and args.get("action") == "inspect":
                    return {"expect": {"control_revision": 7}}
                return {"state": "accepted"}
            return original(name, args)
        self.core.invoke = invoke
        self.ext = tier1.Tier1(self.core)
        self.ext.run = hook_output  # Instance-local: other suites also reuse this fixture directly.

    def tearDown(self):
        self.env.stop()
        self.temp.cleanup()

    def launch(self, runtime, **args):
        return self.core.handlers["tier1." + runtime](args)

    def spawns(self):
        return [params for method, params in self.core.calls if method == "resource.spawn"]

    def configure(self, **value):
        self.core.apply([{"op": "set", "key": tier1.SETTINGS, "value": value}], "settings")

    def test_pi_testing_selection_is_per_launch_and_no_profile_is_modified(self):
        self.core.info = {"socket": "/private/core.sock", "state_dir": "/private/core-state", "extension_token": "never-forward"}
        self.launch("pi")
        env = self.spawns()[0]["env"]
        self.assertEqual(env["VQRO_TESTING_SOCKET"], "/private/core.sock")
        self.assertEqual(env["VQRO_TESTING_STATE_DIR"], "/private/core-state")
        self.assertNotIn("VQRO_EXTENSION_TOKEN", env)
        self.assertFalse((self.home / ".pi").exists())

    def test_pi_missing_testing_asset_refuses_before_spawn(self):
        with patch("tier1.testing_args", side_effect=VqroError("plugin missing", "unavailable")):
            with self.assertRaisesRegex(VqroError, "missing"):
                self.launch("pi")
        self.assertEqual(self.spawns(), [])

    def test_claude_launch_wires_hooks_for_that_launch_only(self):
        self.configure(claude=["/opt/claude"], hook=str(self.hook))
        result = self.launch("claude", cwd="/work")
        spawn = self.spawns()[0]
        argv = spawn["argv"]
        self.assertEqual(argv[:4], ["python3", str(tier1.RUN), "--", "/opt/claude"])
        self.assertEqual(argv[4], "--settings")
        settings = Path(argv[5])
        self.assertEqual(settings.parent, self.state / "tier1")
        if sys.platform != "win32":  # Windows ACLs are not POSIX mode bits.
            self.assertEqual(stat.S_IMODE(settings.stat().st_mode), 0o600)
            self.assertEqual(stat.S_IMODE(settings.parent.stat().st_mode), 0o700)
        self.assertEqual(json.loads(settings.read_text(encoding="utf-8"))["hooks"]["Stop"][0]["hooks"][0]["command"], str(self.hook))
        nonce = spawn["env"]["VQRO_TIER1_LAUNCH"]
        self.assertRegex(spawn["env"]["VQRO_PRINCIPAL"], r"^p:[0-9a-f]{32}$")
        self.assertEqual(set(spawn["env"]), {"VQRO_PRINCIPAL", "VQRO_TIER1_LAUNCH"})
        self.assertEqual((spawn["name"], spawn["cwd"]), ("claude-" + nonce, "/work"))
        declared = self.core.value("doc/agents/terminals/r1")
        self.assertEqual((declared["launch"], declared["app"]), (nonce, "claude"))
        self.assertEqual(declared["pull"]["points"]["idle"], {"precision": "approximate"}, "the plugin's own declaration")
        self.assertEqual(self.core.value("doc/agents/request-routes/r1"), {"route": "human"})
        initial = next(ops for ops in self.core.transactions if any(op["key"] == "doc/agents/terminals/r1" for op in ops))
        self.assertEqual([(op["key"], op["expect"]) for op in initial],
                         [("doc/agents/terminals/r1", 0), ("doc/agents/request-routes/r1", 0)],
                         "declaration and route are one creation transaction")
        self.assertEqual([name for name, _ in self.coord_calls],
                         ["coord.inbox.window", "coord.grants", "coord.scope", "coord.inbox.window", "coord.scope"])
        grant = self.coord_calls[1][1]
        self.assertEqual((grant["id"], grant["expect"], grant["value"]),
                         ("tier1-r1", 0, {"who": spawn["env"]["VQRO_PRINCIPAL"], "on": "agent:r1", "can": ["receive"]}))
        resume = self.coord_calls[4][1]
        self.assertEqual((resume["action"], resume["scope"], resume["expect"]), ("resume", "agent:r1", {"control_revision": 7}))
        self.assertTrue(resume["op"].startswith("w:1:tier1:"))
        self.assertIn(("tabs.show", {"resource": "r1"}), self.core.calls)
        self.assertEqual((result["resource"], result["principal"]), ("r1", spawn["env"]["VQRO_PRINCIPAL"]))
        self.assertEqual(self.core.notices[-1][1], "ok")
        self.assertEqual(len(self.spawns()), 1)
        self.assertFalse((self.home / ".claude").exists() or (self.home / ".pi").exists(), "global config untouched")

    def test_pi_launch_loads_the_plugin_with_its_own_declaration(self):
        self.launch("pi")
        argv = self.spawns()[0]["argv"]
        self.assertEqual(argv[3:], ["pi", "-e", str(tier1.PI_PLUGIN), *tier1.testing_args(tier1.HERE)])
        self.assertTrue(tier1.PI_PLUGIN.is_file())
        declared = self.core.value("doc/agents/terminals/r1")
        self.assertEqual(declared["pull"], tier1.pi_declaration())
        self.assertEqual(declared["pull"]["points"]["idle"], {"precision": "exact"})
        self.assertEqual(declared["app"], "pi")
        self.assertFalse(list((self.state / "tier1").glob("*.json")), "no per-launch file for Pi")

    def test_each_launch_has_a_fresh_principal_and_nonce(self):
        self.core.shells = []
        self.launch("pi")
        self.core.entries.pop("doc/agents/terminals/r1")
        self.launch("pi")
        first, second = self.spawns()
        self.assertNotEqual(first["env"]["VQRO_PRINCIPAL"], second["env"]["VQRO_PRINCIPAL"])
        self.assertNotEqual(first["env"]["VQRO_TIER1_LAUNCH"], second["env"]["VQRO_TIER1_LAUNCH"])

    def test_refused_resume_leaves_the_inbox_paused_and_says_why(self):
        self.refuse.add("coord.scope")
        result = self.launch("pi")
        self.assertEqual(result["resource"], "r1")
        text, style = self.core.notices[-1]
        self.assertEqual(style, "warn")
        self.assertIn("inbox left paused", text)
        self.assertIn("forbidden", text)
        self.assertIsNotNone(self.core.value("doc/agents/terminals/r1"), "the declaration is still recorded")

    def test_a_launch_by_someone_other_than_the_owner_records_no_route(self):
        self.core.caller_record = {"author": "client:2", "actor": "lead", "principal": "p:lead", "initiator": "p:lead"}
        self.launch("pi")
        self.assertIsNotNone(self.core.value("doc/agents/terminals/r1"))
        self.assertIsNone(self.core.value("doc/agents/request-routes/r1"), "missing means human; no coordinator guess")
        self.assertIn("request route not recorded", self.core.notices[-1][0])

    def test_an_existing_route_is_never_rewritten_and_the_failure_is_named(self):
        self.core.entries["doc/agents/request-routes/r1"] = {"value": {"route": "coordinator"}, "version": 3}
        result = self.launch("pi")
        self.assertEqual(result["resource"], "r1")
        self.assertEqual(self.core.value("doc/agents/request-routes/r1"), {"route": "coordinator"})
        self.assertIn("declaration and request route not recorded (conflict)", self.core.notices[-1][0])
        self.assertEqual(len(self.spawns()), 1, "no spawn replay")

    def test_claude_without_its_hook_refuses_before_spawning(self):
        with patch.object(tier1, "find_hook", return_value=None), self.assertRaises(VqroError) as caught:
            self.launch("claude")
        self.assertEqual(caught.exception.kind, "unavailable")
        self.assertEqual(self.spawns(), [])

    def test_sweep_removes_settings_files_of_ended_launches_only(self):
        folder = tier1.state_dir()
        live, ended, fresh = (folder / f"claude-{n}.json" for n in ("live", "ended", "fresh"))
        for path in (live, ended, fresh):
            path.write_text("{}")
        old = time.time() - 120
        os.utime(live, (old, old))
        os.utime(ended, (old, old))
        self.core.resources["r5"] = {"id": "r5", "state": "live"}
        self.core.entries["doc/agents/terminals/r5"] = {"value": {"launch": "live"}, "version": 1}
        self.ext.sweep()
        self.assertTrue(live.exists(), "a live terminal's settings stay")
        self.assertFalse(ended.exists(), "an ended launch's settings go")
        self.assertTrue(fresh.exists(), "a launch whose declaration is not recorded yet keeps its file")


class FakeLink:
    def __init__(self, rows, entries):
        self.rows, self.entries, self.calls = rows, entries, []

    def call(self, method, params):
        self.calls.append(method)
        if method == "resource.list":
            return {"resources": self.rows}
        return self.entries.get(params["key"])

    def close(self):
        pass


class WrapperTests(unittest.TestCase):
    def test_resolves_its_terminal_once_declared_and_granted(self):
        link = FakeLink([{"id": "r9", "name": "claude-abc"}, {"id": "r7", "name": "pi-zzz"}],
                        {"doc/agents/terminals/r9": {"value": {"launch": "abc"}}, "doc/agents/grants/tier1-r9": {"value": {}}})
        self.assertEqual(tier1_run.resolve(link, "abc"), "r9")

    def test_a_missing_grant_or_other_nonce_times_out(self):
        with patch.object(tier1_run, "DEADLINE_S", 0.3):
            link = FakeLink([{"id": "r9", "name": "claude-abc"}], {"doc/agents/terminals/r9": {"value": {"launch": "abc"}}})
            self.assertIsNone(tier1_run.resolve(link, "abc"))
            link = FakeLink([{"id": "r9", "name": "claude-abc"}],
                            {"doc/agents/terminals/r9": {"value": {"launch": "other"}}, "doc/agents/grants/tier1-r9": {"value": {}}})
            self.assertIsNone(tier1_run.resolve(link, "abc"))

    def test_exec_gets_the_agent_and_drops_the_nonce(self):
        link = FakeLink([{"id": "r9", "name": "pi-abc"}],
                        {"doc/agents/terminals/r9": {"value": {"launch": "abc"}}, "doc/agents/grants/tier1-r9": {"value": {}}})
        with patch.dict(os.environ, {"VQRO_TIER1_LAUNCH": "abc", "VQRO_SOCKET": "/x"}), \
                patch.object(tier1_run, "Core", return_value=link), patch.object(tier1_run.os, "execvpe") as execvpe:
            tier1_run.main(["--", "pi", "-e", "plugin.ts"])
        program, argv, env = execvpe.call_args[0]
        self.assertEqual((program, argv), ("pi", ["pi", "-e", "plugin.ts"]))
        self.assertEqual(env["VQRO_AGENT"], "r9")
        self.assertNotIn("VQRO_TIER1_LAUNCH", env)

    def test_never_blocks_the_human_when_the_core_is_away(self):
        with patch.dict(os.environ, {"VQRO_TIER1_LAUNCH": "abc", "VQRO_SOCKET": "/nonexistent"}), \
                patch.object(tier1_run.os, "execvpe") as execvpe, patch("sys.stderr"):
            tier1_run.main(["--", "claude"])
        self.assertNotIn("VQRO_AGENT", execvpe.call_args[0][2])


if __name__ == "__main__":
    unittest.main()
