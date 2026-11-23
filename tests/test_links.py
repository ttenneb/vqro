"""links.open: URLs to the opener, files to an editor split, relative to the terminal's folder. No sockets."""
import os
import sys
import tempfile
import time
import unittest

from test_workspace_tabs import Core, load
from vqro_ext import VqroError


class Links(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.record = os.path.join(self.dir.name, "opened")
        opener = os.path.join(self.dir.name, "opener")
        with open(opener, "w", encoding="utf-8") as f:
            f.write("import sys\nwith open(sys.argv[1], 'a', encoding='utf-8') as out:\n"
                    "    out.write(''.join(arg + '\\n' for arg in sys.argv[2:]))\n")
        os.makedirs(os.path.join(self.dir.name, "src"))
        open(os.path.join(self.dir.name, "src", "main.rs"), "w").close()
        self.core = Core()
        self.core.resources["r0"] = {"id": "r0", "kind": "terminal", "argv": ["sh"], "state": "live", "cwd": self.dir.name}
        self.core.apply([
            {"op": "set", "key": "view/term-r0", "value": {"kind": "terminal", "resource": "r0"}},
            {"op": "set", "key": "layout/main", "value": {"tabs": [{"title": "sh", "node": {"view": "term-r0"}}], "active": 0}},
            {"op": "set", "key": "workspace/active", "value": {"layout": "main"}},
            {"op": "set", "key": "doc/settings/links", "value": {"opener": [sys.executable, opener, self.record], "editor": ["myedit", "--flag"]}}], "setup")
        mods = load(self.core, "panes", "links")
        self.core.vqro.call = self.call_through(self.core.call)
        self.mods = mods

    def call_through(self, call):
        """Nested command.invoke reaches the other extension, as the core routes it."""
        def routed(method, params=None):
            if method == "command.invoke":
                return self.core.invoke(params["name"], params.get("args"))
            return call(method, params)
        return routed

    def tearDown(self):
        self.dir.cleanup()

    def ctx(self, link):
        return {"layout": "main", "layout_version": self.core.version("layout/main"), "path": [0], "view": "term-r0",
                "kind": "terminal", "resource": "r0", "tab_path": [], "tab_index": 0, "link": link, "link_kind": link["kind"]}

    def opened(self):
        for _ in range(50):
            if os.path.exists(self.record):
                with open(self.record, encoding="utf-8") as f:
                    return f.read().split("\n")[:-1]
            time.sleep(0.05)
        return None

    def test_a_url_goes_to_the_opener(self):
        out = self.core.handlers["links.open"]({"context": self.ctx({"kind": "url", "text": "https://example.org/zz"})})
        self.assertEqual(out["opened"], "https://example.org/zz")
        self.assertEqual(self.opened(), ["https://example.org/zz"])

    def test_a_non_ascii_url_reaches_the_python_opener_unchanged(self):
        url = "https://example.org/café/λ"
        self.core.handlers["links.open"]({"context": self.ctx({"kind": "url", "text": url})})
        self.assertEqual(self.opened(), [url])

    def test_a_relative_file_opens_in_an_editor_split_at_its_line(self):
        out = self.core.handlers["links.open"]({"context": self.ctx({"kind": "path", "text": "src/main.rs", "line": 12, "column": 5})})
        path = os.path.join(self.dir.name, "src", "main.rs")
        spawn = [p for m, p in self.core.calls if m == "resource.spawn"][-1]
        self.assertEqual(spawn, {"cwd": os.path.dirname(path), "argv": ["myedit", "--flag", "+12", path]})
        tab = self.core.value("layout/main")["tabs"][0]["node"]
        self.assertEqual(tab["split"], "h")
        self.assertEqual(tab["children"][0], {"view": "term-r0"})
        self.assertEqual(out, {"client": {"focus": {"view": tab["children"][1]["view"]}}})

    def test_a_missing_file_says_so_and_changes_nothing(self):
        before = self.core.value("layout/main")
        with self.assertRaises(VqroError) as e:
            self.core.handlers["links.open"]({"context": self.ctx({"kind": "path", "text": "nope/x.rs"})})
        self.assertEqual(e.exception.kind, "not_found")
        self.assertEqual(self.core.value("layout/main"), before)

    def test_without_a_link_nothing_opens(self):
        with self.assertRaises(VqroError):
            self.core.handlers["links.open"]({"context": {}})
        self.assertIsNone(self.opened() if os.path.exists(self.record) else None)

    def test_menu_items_match_the_link_kind(self):
        url = self.core.value("menu/links.open-url")
        path = self.core.value("menu/links.open-file")
        self.assertEqual((url["when"], path["when"]), ({"link_kind": "url"}, {"link_kind": "path"}))


if __name__ == "__main__":
    unittest.main()
