"""Manifest-driven extension catalogue, without launching processes or a core."""
import copy
from pathlib import Path
import runpy
import sys
import types
import unittest
from unittest import mock


class Extension:
    def __init__(self, name, **_):
        self.name = name
        self.entries = {"extension/"+n: {"value": {"argv": ["never-run"], "enabled": n != "off"}} for n in (name,"off","broken","third-party")}
        self.live = {}
        self.info = {"state_dir":"private-state"}
        self.states = {n:{"display_status":"off" if n=="off" else "failed" if n=="broken" else "running",
                          "crashes":2 if n=="broken" else 0,"last_exit":"fixture failure" if n=="broken" else None,
                          "manifest":{"name":n,"title":n.title(),"kind":"interface","summary":"A searchable summary",
                                      "description":"Readable even while not running","settings_command":"demo.configure"}}
                       for n in (name,"off","broken","third-party")}
        self.prim = self
        self.published = {}
    def call(self, method, *_):
        if method == "extension.list": return {"extensions":self.states}
        if method == "command.list": return {"commands":[{"name":"demo.configure","available":True}]}
        raise AssertionError(method)
    def value(self,key): return self.entries.get(key,{}).get("value")
    def command(self,*_): return lambda f:f
    def on_event(self,f): return f
    def contribute(self,*_): pass
    def publish(self,name,value): self.published[name] = copy.deepcopy(value)
    def run(self): pass


def load():
    fake=types.SimpleNamespace(Extension=Extension,PrimError=RuntimeError)
    with mock.patch.dict(sys.modules,{"prim_ext":fake}):
        return runpy.run_path(str(Path(__file__).resolve().parents[1]/"extensions/settings.py"))


class SettingsTests(unittest.TestCase):
    def test_uniform_rows_include_self_and_manifest_settings_for_any_name(self):
        module=load();ext=module["ext"]
        page=ext.published["settings"]["pages"][0]
        self.assertEqual(page["list_detail"]["requires"],["list_detail"])
        rows=[r for group in page["items"] for r in group["children"]]
        self.assertEqual(len(rows),4)
        for row in rows:
            self.assertEqual(row["type"],"toggle")
            self.assertNotIn("disabled",row)
            self.assertEqual(row["description"],"A searchable summary")
            self.assertEqual(row["detail_panel"]["items"][0]["text"],"Readable even while not running")
        own=next(r for r in rows if r["id"]=="extension/settings")
        self.assertEqual(own["command"],"prim.extension.disable")
        third=next(r for r in rows if r["id"]=="extension/third-party")
        self.assertEqual(third["detail_panel"]["toolbar"][0]["chips"][-1]["command"],"demo.configure")
        off=next(r for r in rows if r["id"]=="extension/off")
        self.assertEqual(off["badges"][0]["text"],"off")
        self.assertEqual(off["command"],"prim.extension.enable")
        self.assertTrue(off["detail_panel"]["toolbar"][0]["chips"][-1]["disabled"])
        broken=next(r for r in rows if r["id"]=="extension/broken")
        self.assertIn("fixture failure",str(broken["detail_panel"]))

    def test_safe_mode_actions_use_session_allowance_not_saved_enabled(self):
        module=load();ext=module["ext"]
        ext.states["off"].update(safe_mode=True,allowed_in_safe_mode=False,display_status="off")
        groups=module["ext_rows"](ext.states,{"demo.configure"})
        row=next(r for g in groups for r in g["children"] if r["id"]=="extension/off")
        self.assertEqual(row["command"],"prim.safe_mode.allow")
        self.assertEqual(row["args"],{"name":"off","on":True})
        self.assertIn("normal enabled declaration is unchanged",str(row["detail_panel"]))

    def test_builtins_have_complete_readable_manifests(self):
        import json
        root=Path(__file__).resolve().parents[1]/"extensions"
        names={s["name"] for s in json.loads((root/"defaults.json").read_text())["extensions"]}
        self.assertEqual(len(names),15)
        for name in names:
            manifest=json.loads((root/"manifests"/(name+".json")).read_text())
            self.assertEqual(manifest["name"],name)
            self.assertTrue(manifest["summary"])
            self.assertNotIn("\n",manifest["summary"])
            self.assertGreater(len(manifest["description"]),80)
            self.assertTrue(manifest["contributes"])

if __name__=="__main__":unittest.main()
