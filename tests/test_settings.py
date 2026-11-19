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
        self.event_types = _.get("event_types")
        self.entries = {"extension/"+n: {"value": {"argv": ["never-run"], "enabled": n != "off"}} for n in (name,"off","broken","third-party")}
        self.live = {}
        self.info = {"state_dir":"private-state"}
        self.states = {n:{"display_status":"off" if n=="off" else "failed" if n=="broken" else "running",
                          "crashes":2 if n=="broken" else 0,"last_exit":"fixture failure" if n=="broken" else None,
                          "manifest":{"name":n,"title":n.title(),"kind":"interface","summary":"A searchable summary",
                                      "description":"Readable even while not running","settings_command":"demo.configure"}}
                       for n in (name,"off","broken","third-party")}
        self.vqro = self
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
    fake=types.SimpleNamespace(Extension=Extension,VqroError=RuntimeError)
    with mock.patch.dict(sys.modules,{"vqro_ext":fake}):
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
        self.assertEqual(own["command"],"vqro.extension.disable")
        third=next(r for r in rows if r["id"]=="extension/third-party")
        self.assertEqual(third["detail_panel"]["toolbar"][0]["chips"][-1]["command"],"demo.configure")
        off=next(r for r in rows if r["id"]=="extension/off")
        self.assertEqual(off["badges"][0]["text"],"off")
        self.assertEqual(off["command"],"vqro.extension.enable")
        self.assertTrue(off["detail_panel"]["toolbar"][0]["chips"][-1]["disabled"])
        broken=next(r for r in rows if r["id"]=="extension/broken")
        self.assertIn("fixture failure",str(broken["detail_panel"]))

    def test_recent_crashes_and_total_have_distinct_explicit_labels(self):
        module=load();ext=module["ext"]
        ext.states["broken"].update(crashes=0,crashes_total=7,crash_window_seconds=300)
        groups=module["ext_rows"](ext.states,{"demo.configure"})
        broken=next(row for group in groups for row in group["children"] if row["id"]=="extension/broken")
        text=str(broken["detail_panel"])
        self.assertIn("Recent crashes (last 300 s): 0",text)
        self.assertIn("Total crashes this supervision: 7",text)
        self.assertIn("fixture failure",text,"last-exit history remains available even when no recent crashes remain")

    def test_safe_mode_actions_use_session_allowance_not_saved_enabled(self):
        module=load();ext=module["ext"]
        ext.states["off"].update(safe_mode=True,allowed_in_safe_mode=False,display_status="off")
        groups=module["ext_rows"](ext.states,{"demo.configure"})
        row=next(r for g in groups for r in g["children"] if r["id"]=="extension/off")
        self.assertEqual(row["command"],"vqro.safe_mode.allow")
        self.assertEqual(row["args"],{"name":"off","on":True})
        self.assertIn("normal enabled declaration is unchanged",str(row["detail_panel"]))

    def test_provider_connection_status_is_warned_and_refreshed_without_hiding_process_state(self):
        module=load();ext=module["ext"]
        self.assertIn("provider",ext.event_types,"handler must actually receive provider-only changes")
        ext.states["third-party"].update(status="running",display_status="provider-not-connected")
        module["on_event"]("provider",{"name":"different-provider-name","state":{"connected":False}})
        rows=[row for group in ext.published["settings"]["pages"][0]["items"] for row in group["children"]]
        row=next(row for row in rows if row["id"]=="extension/third-party")
        self.assertEqual(row["badges"],[{"text":"provider-not-connected","style":"warn"}])
        self.assertIn("Process: running",str(row["detail_panel"]))
        self.assertFalse(row["detail_panel"]["toolbar"][0]["chips"][-1]["disabled"],"configuration can repair an unreachable backend")

    def test_key_label_option_and_keys_page_keep_configured_ids_unchanged(self):
        module=load();ext=module["ext"]
        ext.entries["binding/modified"]={"value":{"key":"C-S-Left","command":"demo.configure","label":"modified"}}
        for mode,shown in (("full","Ctrl-g"),("short","C-g"),("unknown","Ctrl-g")):
            ext.entries["doc/settings/tui"]={"value":{"prefix":"C-g","key_labels":mode}}
            before=copy.deepcopy(ext.entries)
            module["refresh"]()
            pages=ext.published["settings"]["pages"]
            option=next(r for p in pages if p["title"]=="Interface" for r in p["items"] if r["text"]=="Key labels")
            self.assertEqual(option["command"],"settings.set")
            self.assertEqual(option["args"],{"tui":"key_labels"})
            self.assertEqual(option["value"],"short" if mode=="short" else "full")
            rows=next(p["items"] for p in pages if p["title"]=="Keys")
            prefix=next(r for r in rows if r.get("text")=="Prefix")
            self.assertEqual(prefix["options"][0]["label"],shown)
            self.assertEqual(prefix["options"][0]["value"],"C-g")
            self.assertIn("Keys after "+shown,str(rows))
            self.assertIn("C-S-Left" if mode=="short" else "Ctrl-Shift-Left",str(rows))
            self.assertEqual(ext.entries,before,"formatting never rewrites settings/bindings")
        self.assertEqual(module["norm_key"]("Ctrl-Shift-Left"),"C-S-Left")

    def test_keys_page_shows_conditional_keys_with_the_full_key_name(self):
        module=load();ext=module["ext"]
        ext.entries["binding/agents.prompt"]={"value":{"key":"p","command":"demo.configure","label":"prompt agent","when":{"resource_kind":"agent"}}}
        ext.entries["doc/settings/tui"]={"value":{"prefix":"C-g"}}
        module["refresh"]()
        rows=next(p["items"] for p in ext.published["settings"]["pages"] if p["title"]=="Keys")
        row=next(r for r in rows if r.get("text")=="Ctrl-g p")
        self.assertEqual(row["description"],"prompt agent (only where resource kind is agent)")

    def test_failed_budget_keeps_cause_time_total_and_explicit_restart(self):
        module=load();ext=module["ext"]
        at=1760000000000
        ext.states["broken"].update(status="failed",crashes=0,crashes_total=3,crash_window_seconds=300,
            failure={"kind":"crash_budget","at_ms":at,"reason":"3 exits in 300 seconds","crashes":3,"window_seconds":300},
            last_exits=[{"at_ms":at-i*1000,"reason":f"exit-{i}"} for i in range(3)])
        groups=module["ext_rows"](ext.states,set())
        row=next(r for group in groups for r in group["children"] if r["id"]=="extension/broken")
        details=str(row["detail_panel"])
        for text in ("Recent crashes (last 300 s): 0","Total crashes this supervision: 3","At failure: 3 exits",
                     module["time_text"](at),"exit-2","Stopped until explicitly restarted","built-in failed/recover"):
            self.assertIn(text,details)
        action=next(c for c in row["detail_panel"]["toolbar"][0]["chips"] if c["command"]=="vqro.extension.restart")
        self.assertEqual(action["args"],{"name":"broken"})
        self.assertFalse(action["disabled"])

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
