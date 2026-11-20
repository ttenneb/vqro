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
    def invoke(self,name,args): self.invoked=getattr(self,"invoked",[])+[(name,args)]; return {}
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

    def test_extension_pages_from_live_settings_page_content_follow_interface(self):
        module=load();ext=module["ext"]
        ext.live["settings.page.sidebar"]={"title":"Sidebar","items":[{"type":"toggle","text":"Tabs","checked":False,
            "command":"sidebar.preset","args":{"show":{"tabs":True}}}]}
        ext.live["settings.page.broken"]="not a page"
        ext.live["unrelated"]={"title":"No","items":[]}
        module["on_event"]("view.content",{"view":"settings.page.sidebar"})
        pages=ext.published["settings"]["pages"]
        titles=[p["title"] for p in pages]
        self.assertEqual(titles[titles.index("Interface")+1],"Sidebar")
        self.assertNotIn("No",titles)
        self.assertEqual(pages[titles.index("Sidebar")]["items"],ext.live["settings.page.sidebar"]["items"],"the extension's own rows and state")
        del ext.live["settings.page.sidebar"]
        module["on_event"]("view.content",{"view":"settings.page.sidebar"})
        self.assertNotIn("Sidebar",[p["title"] for p in ext.published["settings"]["pages"]],"gone when the extension stops")

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
        ext.entries["binding/agents.config"]={"value":{"key":"P","command":"demo.configure","label":"agent settings","when":{"resource_kind":"agent"}}}
        ext.entries["doc/settings/tui"]={"value":{"prefix":"C-g"}}
        module["refresh"]()
        rows=next(p["items"] for p in ext.published["settings"]["pages"] if p["title"]=="Keys")
        row=next(r for r in rows if r.get("text")=="Ctrl-g p")
        self.assertEqual(row["description"],"prompt agent (only where resource kind is agent)")
        shift=next(r for r in rows if r.get("text")=="Ctrl-g P")
        self.assertEqual(shift["description"],"agent settings (only where resource kind is agent)")

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

    def dependency_fixture(self):
        module=load();ext=module["ext"]
        ext.entries["extension/tabs"]={"value":{"argv":["never-run"],"enabled":False}}
        ext.states["tabs"]={"display_status":"off","manifest":{"name":"tabs","title":"Tabs","provides":["command/tabs.show"]},
            "dependencies":{"status":"ok","needs":[],"works_with":[],"used_by":[
                {"name":"off","cap":"command/tabs.show","hard":True,"display_status":"off"},
                {"name":"third-party","cap":"command/tabs.show","hard":True,"display_status":"running"},
                {"name":"settings","cap":"command/tabs.show","hard":False,"display_status":"running"}]}}
        need={"cap":"command/tabs.show","status":"off","suggest":"tabs","why":"opens panels","providers":[{"name":"tabs","display_status":"off"}]}
        ext.states["off"]["dependencies"]={"status":"missing","needs":[need,{"cap":"ability/x.gpu","status":"missing","suggest":"gpu","providers":[]},
            {"cap":"ui/vqro.items","status":"lacking","lacking_in":["text"]}],"works_with":[],"used_by":[]}
        for user in ("third-party","settings"):
            ext.states[user]["dependencies"]={"status":"ok","needs":[],"works_with":[],"used_by":[]}
            ext.states[user]["dependencies"]["needs" if user=="third-party" else "works_with"]=[dict(need,status="satisfied")]
        rows={r["id"]:r for g in module["ext_rows"](ext.states,set()) for r in g["children"]}
        return module,ext,rows

    def test_turning_on_offers_needed_providers_in_one_confirmation(self):
        module,ext,rows=self.dependency_fixture()
        row=rows["extension/off"]
        self.assertEqual(row["command"],"settings.switch");self.assertEqual(row["args"],{"name":"off","on":True})
        self.assertEqual([o["value"] for o in row["options"]],["with","only","cancel"])
        self.assertEqual(row["options"][0]["label"],"Turn on Off and Tabs")
        self.assertIn("still needs gpu (not installed)",row["options"][1]["label"])
        self.assertIn({"text":"needs Tabs (off)","style":"warn"},row["badges"])
        detail=str(row["detail_panel"])
        for text in ("'Needs'","Tabs: command/tabs.show (off) · opens panels","gpu: ability/x.gpu (not installed)","Client features missing in: text"):
            self.assertIn(text,detail)
        chip=row["detail_panel"]["toolbar"][0]["chips"][0]
        self.assertEqual((chip["command"],chip["options"]),("settings.switch",row["options"]))
        # Choosing `with` turns on the provider, then the extension; nothing else is switched.
        ext.states["off"]["safe_mode"]=False
        self.assertEqual(module["switch"]({"name":"off","on":True,"value":"with"}),{"switched":["tabs","off"],"on":True})
        self.assertEqual(ext.invoked,[("vqro.extension.enable",{"name":"tabs"}),("vqro.extension.enable",{"name":"off"})])
        ext.invoked=[]
        module["switch"]({"name":"off","on":True,"value":"only"});self.assertEqual(ext.invoked,[("vqro.extension.enable",{"name":"off"})])
        ext.invoked=[]
        self.assertEqual(module["switch"]({"name":"off","on":True,"value":"cancel"}),{"cancelled":True});self.assertEqual(ext.invoked,[])
        ext.states["off"]["safe_mode"]=True
        module["switch"]({"name":"off","on":True,"value":"with"})
        self.assertEqual(ext.invoked,[("vqro.safe_mode.allow",{"name":"tabs","on":True}),("vqro.safe_mode.allow",{"name":"off","on":True})])

    def test_turning_off_warns_what_breaks_and_extensions_without_dependencies_switch_directly(self):
        module,ext,rows=self.dependency_fixture()
        ext.states["tabs"]["display_status"]="running";ext.entries["extension/tabs"]["value"]["enabled"]=True
        rows={r["id"]:r for g in module["ext_rows"](ext.states,set()) for r in g["children"]}
        row=rows["extension/tabs"]
        self.assertEqual(row["options"],[{"label":"Turn off Tabs: breaks Third-Party; limits Settings","value":"off"},{"label":"Cancel","value":"cancel"}])
        self.assertIn("Third-Party needs command/tabs.show (running)",str(row["detail_panel"]))
        self.assertIn("command/tabs.show",str(row["detail_panel"]).split("'Provides'")[1])
        module["switch"]({"name":"tabs","on":False,"value":"off"})
        self.assertEqual(ext.invoked,[("vqro.extension.disable",{"name":"tabs"})])
        self.assertEqual(rows["extension/broken"]["command"],"vqro.extension.disable")
        self.assertNotIn("options",rows["extension/broken"])
        # Only optional users left: they lose extras (shown under Used by), so no confirmation.
        ext.states["third-party"]["display_status"]="off"
        rows={r["id"]:r for g in module["ext_rows"](ext.states,set()) for r in g["children"]}
        self.assertEqual(rows["extension/tabs"]["command"],"vqro.extension.disable")
        self.assertNotIn("options",rows["extension/tabs"])

    def test_builtins_have_complete_readable_manifests(self):
        import json
        root=Path(__file__).resolve().parents[1]/"extensions"
        names={s["name"] for s in json.loads((root/"defaults.json").read_text())["extensions"]}
        self.assertEqual(len(names),16)
        for name in names:
            manifest=json.loads((root/"manifests"/(name+".json")).read_text())
            self.assertEqual(manifest["name"],name)
            self.assertTrue(manifest["summary"])
            self.assertNotIn("\n",manifest["summary"])
            self.assertGreater(len(manifest["description"]),80)
            self.assertTrue(manifest["contributes"])

if __name__=="__main__":unittest.main()
