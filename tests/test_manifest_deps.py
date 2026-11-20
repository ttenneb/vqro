"""Release-check lint: manifests declare provided capabilities and every cross-extension use."""
import contextlib
import io
import json
import os
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts"))
import check_manifest_deps  # noqa: E402

TABS = '''
from vqro_ext import Extension
ext = Extension("tabs")
@ext.command("tabs.show", "Show")
def show(args): return {}
ext.publish("tab-data", {})
'''
AGENTS = '''
from vqro_ext import Extension
from helpers import open_it
ext = Extension("agents")
themes = ext.live.get("themes")
'''
HELPERS = '''
def open_it(ext): return ext.invoke("tabs.show", {})
def unused(ext): return ext.invoke("tabs.secret", {})
'''


def write(path, text):
    with open(path, "w", encoding="utf-8") as f:
        f.write(text)


class ManifestDepsLint(unittest.TestCase):
    def run_lint(self, manifests):
        with tempfile.TemporaryDirectory() as d:
            os.mkdir(os.path.join(d, "manifests"))
            for name, text in {"tabs.py": TABS, "agents.py": AGENTS, "helpers.py": HELPERS}.items():
                write(os.path.join(d, name), text)
            write(os.path.join(d, "defaults.json"), json.dumps(
                {"extensions": [{"name": n, "argv": ["python3", n + ".py"]} for n in manifests if n in ("tabs", "agents")]}))
            for name, m in manifests.items():
                write(os.path.join(d, "manifests", name + ".json"), json.dumps({"name": name, **m}))
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                rc = check_manifest_deps.main(["--extensions", d])
            return rc, out.getvalue()

    def test_bundled_manifests_pass(self):
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            rc = check_manifest_deps.main(["--extensions", os.path.join(check_manifest_deps.ROOT, "extensions")])
        self.assertEqual(rc, 0, out.getvalue())

    def test_undeclared_uses_and_provides_fail(self):
        rc, out = self.run_lint({"tabs": {"provides": ["command/tabs.show", "command/tabs.gone"]},
                                 "agents": {}, "themes": {"provides": ["data/themes"]}})
        self.assertEqual(rc, 1)
        self.assertIn("agents: uses command/tabs.show (provided by tabs) without declaring", out)
        self.assertIn("agents: uses data/themes (provided by themes)", out)
        self.assertIn("tabs: provides command/tabs.gone but never registers it", out)
        self.assertIn("tabs: publishes tab-data but provides neither", out)
        self.assertNotIn("tabs.secret", out)  # only imported helper functions are scanned

    def test_declared_uses_pass(self):
        rc, out = self.run_lint({"tabs": {"provides": ["command/tabs.show", "data/tab-data"]},
                                 "agents": {"needs": [{"cap": "command/tabs.show", "suggest": "tabs"}],
                                            "works_with": ["data/themes"]},
                                 "themes": {"provides": ["data/themes@1"]}})
        self.assertEqual(rc, 0, out)

    def test_prefix_registered_command_families_are_checked(self):
        """`ext.command("agents." + name)` hides names from the scan: references inside the family still count."""
        family = '''
from vqro_ext import Extension
ext = Extension("agents")
for name in ("show", "stop"):
    ext.command("agents." + name, name)(lambda a: {})
row = {"text": "Stop", "command": "agents.stop"}
'''
        user = '''
from vqro_ext import Extension
ext = Extension("sidebar")
menu = [{"label": "Stop", "command": "agents.stop"}, {"label": "Doc", "text": "agents.md"}]
'''
        with tempfile.TemporaryDirectory() as d:
            os.mkdir(os.path.join(d, "manifests"))
            write(os.path.join(d, "agents.py"), family)
            write(os.path.join(d, "sidebar.py"), user)
            write(os.path.join(d, "defaults.json"), json.dumps({"extensions": [{"name": n, "argv": ["python3", n + ".py"]} for n in ("agents", "sidebar")]}))

            def lint(agents, sidebar):
                write(os.path.join(d, "manifests", "agents.json"), json.dumps({"name": "agents", **agents}))
                write(os.path.join(d, "manifests", "sidebar.json"), json.dumps({"name": "sidebar", **sidebar}))
                out = io.StringIO()
                with contextlib.redirect_stdout(out):
                    return check_manifest_deps.main(["--extensions", d]), out.getvalue()

            rc, out = lint({"provides": ["command/agents.show"]}, {})
            self.assertEqual(rc, 1)
            self.assertIn("agents: uses command agents.stop of agents's registered family, which agents's manifest does not provide", out)
            self.assertIn("sidebar: uses command agents.stop of agents's registered family", out)
            rc, out = lint({"provides": ["command/agents.show", "command/agents.stop"]}, {})
            self.assertIn("sidebar: uses command/agents.stop (provided by agents) without declaring it", out)
            rc, out = lint({"provides": ["command/agents.show", "command/agents.stop"]}, {"works_with": ["command/agents.stop"]})
            self.assertEqual(rc, 0, out)

    def test_new_view_kinds_need_a_fallback(self):
        source = '''
from vqro_ext import Extension
ext = Extension("canvas")
ext.contribute({"view/canvas.a": {"kind": "canvas.native"}, "view/canvas.b": {"kind": "canvas.native", "fallback": "canvas.rows"},
                "view/canvas.rows": {"kind": "panel", "items": []}})
'''
        with tempfile.TemporaryDirectory() as d:
            os.mkdir(os.path.join(d, "manifests"))
            write(os.path.join(d, "canvas.py"), source)
            write(os.path.join(d, "defaults.json"), json.dumps({"extensions": [{"name": "canvas", "argv": ["python3", "canvas.py"]}]}))
            write(os.path.join(d, "manifests", "canvas.json"), json.dumps({"name": "canvas", "provides": ["view/canvas.*"]}))
            out = io.StringIO()
            with contextlib.redirect_stdout(out):
                rc = check_manifest_deps.main(["--extensions", d])
        self.assertEqual(rc, 1)
        self.assertIn("canvas: view/canvas.a has kind canvas.native, which base clients can't render, and no fallback view", out.getvalue())
        self.assertNotIn("canvas.b", out.getvalue())

    def test_only_generic_principal_documents_are_core_owned(self):
        for key, expected in (("doc/principals/owner", 0), ("doc/principals/delegations/a-b", 0),
                              ("doc/principals/other", 1), ("doc/agents/grants", 1)):
            with self.subTest(key=key), patch.dict(globals(), TABS=TABS+"\nkey = "+repr(key)+"\n"):
                rc, out = self.run_lint({"tabs": {"provides": ["command/tabs.show", "data/tab-data"]}})
            self.assertEqual(rc, expected, out)

    def test_invalid_capability_names_fail(self):
        rc, out = self.run_lint({"tabs": {"provides": ["tabs.show"]}, "agents": {}})
        self.assertEqual(rc, 1)
        self.assertIn("invalid capability in provides: 'tabs.show'", out)


if __name__ == "__main__":
    unittest.main()
