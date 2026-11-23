"""RPC admission/negotiation tests. Only private fake executables; never a model."""
import contextlib
import io
import json
import os
from pathlib import Path
import sys
import tempfile
import time
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
from pi_bridge.testing import Plugin, controlled
from pi_bridge.program import Bridge
from pi_bridge import launch
from testing_setup import rpc_rule_notice, rpc_launches, RPC_RULE_NOTICE
from pi_testing import rpc_testing_args
from testing_unit import setup_status


class RpcTestingTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="rpc-testing-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.plugin = self.root / "tier1/vqro-testing.ts"
        self.skill = self.root / "tier1/skills/vqro-setup-testing/SKILL.md"
        self.skill.parent.mkdir(parents=True)
        self.skill.write_text("skill")
        self.plugin.write_text("plugin")
        launch._HELP.clear()
        launch._WARNED.clear()

    def rows(self):
        return [{"name": "vqro-testing-status", "source": "extension", "sourceInfo": {"path": str(self.plugin)}},
                {"name": "skill:vqro-setup-testing", "source": "skill", "sourceInfo": {"path": str(self.skill)}}]

    def test_only_installed_regular_plugin_and_derived_skill(self):
        self.assertEqual(rpc_testing_args(self.root), ["--testing-plugin", str(self.plugin)])
        allowed = Plugin(self.plugin, self.root, "p:one")
        self.assertEqual(allowed.arguments(), ["-e", str(self.plugin), "--skill", str(self.skill)])
        for path in (self.root / "other.ts", self.root, self.skill):
            with self.subTest(path=path), self.assertRaises((ValueError, OSError)):
                Plugin(path, self.root)
        self.skill.unlink()
        with self.assertRaises(OSError):
            Plugin(self.plugin, self.root)
        self.assertEqual(rpc_testing_args(self.root), [])

    @unittest.skipIf(os.name == "nt", "unprivileged symlink creation is not portable")
    def test_plugin_symlink_cannot_escape_installed_root(self):
        outside = self.root / "outside.ts"
        outside.write_text("not the installed plugin")
        self.plugin.unlink()
        self.plugin.symlink_to(outside)
        with self.assertRaises(ValueError):
            Plugin(self.plugin, self.root)
        self.assertEqual(rpc_testing_args(self.root), [])

    def test_native_argv_never_accepts_explicit_interceptors(self):
        for flag in ("-e", "--extension=x", "--prompt-template=y", "--print", "-p"):
            with self.subTest(flag=flag), self.assertRaises(ValueError):
                controlled(["pi", flag])
        self.assertEqual(controlled(["pi", "--mode", "rpc"]), ["pi", "--mode", "rpc"])

    def test_legacy_native_environment_adds_exactly_two_names(self):
        names = ["VQRO_TESTING_SOCKET", "VQRO_TESTING_STATE_DIR", "VQRO_TESTING_EXTENSIONS_DIR", "VQRO_EXTENSION_TOKEN",
                 "VQRO_BRIDGE_TOKEN", "VQRO_STATE_DIR", "HERDR_SOCKET", "PATH"]
        bridge = Bridge(["fixture"], cwd=self.root, env=dict.fromkeys(names, "fixture"), token="fake-bootstrap")
        self.assertEqual(set(bridge.env), {"VQRO_TESTING_SOCKET", "VQRO_TESTING_STATE_DIR", "PATH"})

    def test_inventory_evidence_must_match_command_and_skill_paths(self):
        plugin = Plugin(self.plugin, self.root, "p:one")
        rows = self.rows()
        plugin.observe(rows[:1])
        self.assertEqual(plugin.fact()["state"], "unverified")
        plugin.observe(rows)
        self.assertEqual(plugin.fact()["state"], "loaded")
        rows[0]["sourceInfo"]["path"] = str(self.root / "other.ts")
        plugin.observe(rows)
        self.assertEqual(plugin.fact()["state"], "unverified")
        plugin.observe(None)
        self.assertEqual(plugin.fact()["state"], "unverified")

    def test_scope_is_per_launch_and_old_or_unavailable_bridges_never_clear_it(self):
        plugin = Plugin(self.plugin, self.root, "p:one")
        plugin.observe(self.rows())
        resource = {"id": "r1", "kind": "agent", "name": "Pi", "state": "live", "available": True,
                    "data": {"agent": {"testing_plugin": plugin.fact()}}}
        snapshot = {"rpc_resources": [resource]}
        self.assertEqual(rpc_rule_notice(snapshot), RPC_RULE_NOTICE)  # Never globally upgrade coverage.
        self.assertIsNone(rpc_rule_notice(snapshot, "p:one"))
        self.assertEqual(rpc_rule_notice(snapshot, "p:other"), RPC_RULE_NOTICE)
        resource["available"] = False
        self.assertEqual(rpc_rule_notice(snapshot, "p:one"), RPC_RULE_NOTICE)
        resource["available"] = True
        resource["data"]["agent"].pop("testing_plugin")  # Old bridge.
        self.assertEqual(rpc_rule_notice(snapshot, "p:one"), RPC_RULE_NOTICE)
        self.assertEqual(rpc_launches(snapshot)[0]["state"], "unverified")
        resource["data"] = None
        self.assertEqual(rpc_rule_notice(snapshot, "p:one"), RPC_RULE_NOTICE)

    def test_failed_resource_read_is_serializable_and_never_confirms_rpc_scope(self):
        class Client:
            def call(self, method, params=None):
                if method == "workspace.snapshot":
                    return {"epoch": "fixture", "entries": {}}
                raise ConnectionError("fixture resource lookup unavailable")
        status = setup_status(self.root / "state", self.root, client=Client(), hello={"server": "fixture"}, rpc_principal="p:one")
        json.dumps(status)
        self.assertFalse(status["rpc_testing"]["verified"])
        self.assertIn("lookup unavailable", status["rpc_testing"]["lookup_error"])
        self.assertIn(RPC_RULE_NOTICE, status["not_verified_reasons"])

    def executable(self, body):
        path = self.root / "bridge"
        path.write_text("#!" + sys.executable + "\n" + body)
        path.chmod(0o700)
        return str(path)

    @unittest.skipIf(os.name == "nt", "private shebang executable fixture")
    def test_help_probe_new_old_cache_and_mtime_invalidation(self):
        count = self.root / "probes"
        binary = self.executable("import pathlib\np=pathlib.Path(" + repr(str(count)) + ")\np.write_text(p.read_text()+'x' if p.exists() else 'x')\nprint('--testing-plugin')\n")
        args = ["--testing-plugin", str(self.plugin), "--", "pi", "--mode", "rpc"]
        self.assertEqual(launch.negotiated(args, binary), args)
        self.assertEqual(launch.negotiated(args, binary), args)
        self.assertEqual(count.read_text(), "x")
        binary = self.executable("print('older bridge')\n")
        with contextlib.redirect_stderr(io.StringIO()) as notices:
            self.assertEqual(launch.negotiated(args, binary), ["--", "pi", "--mode", "rpc"])
            self.assertEqual(launch.negotiated(args, binary), ["--", "pi", "--mode", "rpc"])
            self.assertEqual(launch.negotiated(["--testing-plugin=" + str(self.plugin), "--", "pi"], binary), ["--", "pi"])
        self.assertEqual(notices.getvalue().count("NOT VERIFIED"), 1)
        # A native flag is not mistaken for the wrapper's dedicated field.
        native = ["--", "pi", "--testing-plugin", str(self.plugin)]
        self.assertEqual(launch.negotiated(native, binary), native)

    @unittest.skipIf(os.name == "nt", "private shebang executable fixture")
    def test_help_probe_bounds_time_output_and_missing_executable(self):
        binary = self.executable("import time\ntime.sleep(10)\n")
        started = time.monotonic()
        self.assertFalse(launch.supports_testing(binary))
        self.assertLess(time.monotonic() - started, 3)
        binary = self.executable("print('x'*100000)\n")
        self.assertFalse(launch.supports_testing(binary))
        self.assertFalse(launch.supports_testing(str(self.root / "absent")))


if __name__ == "__main__":
    unittest.main()
