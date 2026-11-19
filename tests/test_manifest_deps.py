"""Release-check lint: manifests declare provided capabilities and every cross-extension use."""
import contextlib
import io
import json
import os
import sys
import tempfile
import unittest

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

    def test_invalid_capability_names_fail(self):
        rc, out = self.run_lint({"tabs": {"provides": ["tabs.show"]}, "agents": {}})
        self.assertEqual(rc, 1)
        self.assertIn("invalid capability in provides: 'tabs.show'", out)


if __name__ == "__main__":
    unittest.main()
