"""Native attention policy and read-only producer characterization."""
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import sys
import tempfile
import unittest
import document
import provider
import view

H = {"class":"human_unverified", "terminal_id":"human", "uid":os.geteuid()}
A = {"class":"agent", "terminal_id":"a", "agent":"pi", "execution":"boot:1:1"}
P = {"pane_id":"w1:p1", "workspace_id":"w1", "tab_id":"w1:t1"}

class DocumentTests(unittest.TestCase):
    def setUp(self):
        self.root=tempfile.TemporaryDirectory()
        self.store=provider.Store(self.root.name)
    def tearDown(self):
        self.store.db.close();self.root.cleanup()
    def invoke(self, payload, caller=H):
        if payload["action"] in ("create","decision"):
            payload={"done_when":"An observable result is recorded.",**payload}
        return self.store.invoke({"caller":caller,"payload":payload})
    def change(self,task,action,**fields):
        return self.invoke({"action":action,"id":task["id"],"revision":task["revision"],"operation_id":f"{action}-{task['revision']}",**fields})["task"]
    def snapshot(self, refs=None, revision=1):
        return {"generation":7,"revision":revision,"terminals":refs if refs is not None else [{"terminal_id":"human","placement":P}],"activities":[]}
    def test_shared_durable_policy_and_placement_move_and_unknown(self):
        task=self.invoke({"action":"create","title":"native","operation_id":"create"})["task"]
        task=self.change(task,"delegate",owner=H)
        task=self.change(task,"start")
        self.assertEqual(set(),view.durable_attention(task,{"human":P}))
        task=self.change(task,"block",text="human setup blocker")
        doc=document.project(self.store,self.snapshot())
        badge=next(row for row in doc["items"] if "badge:tab:" in row["id"])
        self.assertEqual("1 blocked",badge["label"])
        tasks={task["id"]:task}
        rows=view.project(tasks,None,{},set(),True,set(),7,100,100,scope={"kind":"tab","ids":["w1:t1"]},placements={"human":P})
        self.assertEqual([task["id"]],[row[0] for row in rows])
        moved={**P,"workspace_id":"w2","tab_id":"w2:t3","pane_id":"w2:p2"}
        new=document.project(self.store,self.snapshot([{"terminal_id":"human","placement":moved}]))
        self.assertNotIn(badge["id"],[row["id"] for row in new["items"]])
        self.assertTrue(any("badge:tab:w2:t3" in row["id"] for row in new["items"]))
        global_doc=document.project(self.store,self.snapshot([]))
        self.assertTrue(any("badge:session:global:0:1:1" in row["id"] for row in global_doc["items"]))
        self.assertNotIn("unavailable",view.durable_attention(task,None))
        self.assertEqual([],view.project(tasks,None,{},set(),True,set(),7,100,100,scope={"kind":"tab","ids":["w1:t1"]},placements={"human":moved}))
    def test_no_parent_or_acceptor_placement_guess_and_no_self_work_human_debt(self):
        task={"id":"child","state":"reported","owner":A,"owed_action":"accept","owed_to":{"role":"acceptors","bindings":[H]}}
        self.assertEqual({"human","unavailable"},view.durable_attention(task,{"human":P}))
        self.assertFalse(view.in_scope(task,{"human":P},{"kind":"workspace","ids":["w1"]}))
        self.assertTrue(view.in_scope(task,{"human":P},{"kind":"session","ids":["global"]}))
        self.assertEqual(set(),view.durable_attention({**task,"state":"in_progress","owner":H,"owed_action":"report","owed_to":H},{"human":P}))
    def test_read_only_store_never_initializes_or_migrates_and_sees_new_commits(self):
        with tempfile.TemporaryDirectory() as missing:
            with self.assertRaises(sqlite3.OperationalError):provider.read_only_store(missing)
            self.assertEqual([],list(Path(missing).iterdir()))
        observed=provider.read_only_store(self.root.name)
        try:
            with self.assertRaises(sqlite3.OperationalError):observed.db.execute("CREATE TABLE forbidden (id TEXT)")
            self.assertFalse(document.project(observed,self.snapshot())["actions"])
            task=self.invoke({"action":"decision","title":"quiet decision","operation_id":"decision"})["task"]
            self.assertTrue(any(row["label"]==task["title"] for row in document.project(observed,self.snapshot())["items"]))
            self.store.db.execute("PRAGMA user_version=3")
            with self.assertRaisesRegex(ValueError,"current store version"):provider.read_only_store(self.root.name)
            self.assertEqual(3,self.store.db.execute("PRAGMA user_version").fetchone()[0])
        finally:observed.db.close()
    def test_protocol_document_only_missing_store_no_writes_and_current_version_outside_receipt(self):
        with tempfile.TemporaryDirectory() as missing:
            welcome={"type":"welcome","protocol":2,"granted_capabilities":["action_invoke","host_document"],"methods":["action.invoke.v1","host.document.render"]}
            request={"type":"request","id":"read","method":"host.document.render","params":self.snapshot()}
            proc=subprocess.run([sys.executable,"-B",str(Path(provider.__file__).resolve()),"--native-attention","--data-dir",missing],input=json.dumps(welcome)+"\n"+json.dumps(request)+"\n",capture_output=True,text=True,timeout=5)
            self.assertEqual(0,proc.returncode,proc.stderr)
            lines=[json.loads(line) for line in proc.stdout.splitlines()]
            self.assertIn("error",lines[1]);self.assertEqual([],list(Path(missing).iterdir()))
        welcome={"type":"welcome","protocol":2,"granted_capabilities":["action_invoke","host_document"],"methods":["action.invoke.v1","host.document.render"]}
        create={"action":"create","title":"receipt","done_when":"Recorded.","operation_id":"original"}
        requests=[{"type":"request","id":str(n),"method":"action.invoke.v1","params":{"caller":H,"payload":payload}} for n,payload in enumerate([create,{**create,"operation_id":"later"},create])]
        proc=subprocess.run([sys.executable,"-B",str(Path(provider.__file__).resolve()),"--native-attention","--data-dir",self.root.name],input="\n".join(json.dumps(value) for value in [welcome,*requests])+"\n",capture_output=True,text=True,timeout=5)
        self.assertEqual(0,proc.returncode,proc.stderr)
        results=[json.loads(line)["result"] for line in proc.stdout.splitlines()[1:]]
        self.assertEqual([1,2,2],[result["document_revision"] for result in results])
        self.assertEqual(results[0]["value"],results[2]["value"])
        self.assertTrue(all("document_revision" not in json.loads(row[0]) for row in self.store.db.execute("SELECT outcome FROM operations")))
