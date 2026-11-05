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

class AttentionRowTargetTests(unittest.TestCase):
    """Where a sidebar attention entry sends a click."""

    PLACEMENTS = {
        "term-owner": {"pane_id": "pane_owner", "workspace_id": "ws", "tab_id": "tab"},
        "term-reporter": {"pane_id": "pane_reporter", "workspace_id": "ws", "tab_id": "tab"},
    }
    OWNER = {"class": "agent", "terminal_id": "term-owner", "agent": "pi"}
    REPORTER = {"class": "agent", "terminal_id": "term-reporter", "agent": "pi"}

    def target(self, task):
        return document.attention_row_target(task, self.PLACEMENTS)

    def test_a_waiting_row_points_at_the_pane_where_the_human_acts(self):
        # The ball is in the human's court, so there is no pane of theirs: the entry
        # lands on the agent that reported the task instead of doing nothing.
        task = {"id": "task_1", "state": "started", "owed_action": "answer",
                "owed_to": {"class": "human_unverified", "terminal_id": "human", "uid": 1000},
                "owner": self.OWNER, "creator": self.REPORTER}
        self.assertEqual(self.target(task)["placement"]["pane_id"], "pane_reporter")

    def test_an_owned_row_points_at_the_owner_pane(self):
        task = {"id": "task_2", "state": "blocked", "owed_action": "unblock",
                "owed_to": {"class": "agent", "terminal_id": "term-reporter", "agent": "pi"},
                "owner": self.OWNER, "creator": self.REPORTER}
        self.assertEqual(self.target(task)["placement"]["pane_id"], "pane_owner")

    def test_a_row_with_no_live_pane_carries_no_terminal(self):
        gone = {"class": "agent", "terminal_id": "term-gone", "agent": "pi"}
        task = {"id": "task_3", "state": "started", "owed_action": "start",
                "owed_to": gone, "owner": gone, "creator": gone}
        self.assertIsNone(self.target(task))


class MessageSubjectTests(unittest.TestCase):
    def test_a_subject_drops_internal_ids_and_keeps_the_sentence(self):
        # The human's dogfood report: a row read "smoke test message from
        # task_66c03c..." so the reader had to parse a store key.
        subject = document.message_subject({"body": "smoke test message from task_66c03c1d"})
        self.assertNotIn("task_66c03c1d", subject)
        self.assertIn("smoke test message from", subject)
        self.assertIn("a task", subject)

    def test_a_subject_stays_bounded_and_handles_an_empty_body(self):
        self.assertEqual("(no body)", document.message_subject({"body": "   "}))
        long = document.message_subject({"body": "x" * 400})
        self.assertLessEqual(len(long), document.MESSAGE_PREVIEW)


class DocumentTests(unittest.TestCase):
    def setUp(self):
        self.root=tempfile.TemporaryDirectory()
        self.store=provider.Store(self.root.name)
    def tearDown(self):
        self.store.db.close();self.root.cleanup()
    def invoke(self, payload, caller=H):
        if payload["action"] in ("create","decision"):
            payload={"done_when":"An observable result is recorded.",**payload}
        if payload["action"] == "decision":
            payload={"options":[{"key":"yes","label":"Yes"},{"key":"no","label":"No"}],"default":"yes","reversible":True,"waiting_cost":"Pending human choice",**payload}
        return self.store.invoke({"caller":caller,"payload":payload})
    def change(self,task,action,**fields):
        return self.invoke({"action":action,"id":task["id"],"revision":task["revision"],"operation_id":f"{action}-{task['revision']}",**fields})["task"]
    def snapshot(self, refs=None, revision=1):
        return {"generation":7,"revision":revision,"terminals":refs if refs is not None else [{"terminal_id":"human","placement":P}],"activities":[]}
    def test_pane_panels_slice_by_pane_and_link_the_counterparty(self):
        panel_a={"pane_id":"w1:p1","workspace_id":"w1","tab_id":"w1:t1"}
        panel_b={"pane_id":"w1:p2","workspace_id":"w1","tab_id":"w1:t1"}
        root=self.invoke({"action":"create","title":"Panel root","operation_id":"panel-root"},caller=A)["task"]
        root=self.change(root,"start")
        child=self.invoke({"action":"create","title":"Panel child","parent":root["id"],"operation_id":"panel-child"},caller=A)["task"]
        child=self.invoke({"action":"delegate","id":child["id"],"revision":child["revision"],
                           "operation_id":"panel-delegate","owner":{"class":"agent","terminal_id":"b","agent":"pi","execution":"boot:1:2"}},caller=A)["task"]
        refs=[{"terminal_id":"a","placement":panel_a},{"terminal_id":"b","placement":panel_b}]
        items=document.project(self.store,self.snapshot(refs))["items"]
        a_root=next(row for row in items if row["id"]=="panel:w1:p1:tasks")
        self.assertEqual({"pane_id":"w1:p1","view_id":"tasks","state":"in_progress"},a_root["view"])
        # The header counts the whole slice; rows carry the open work only.
        self.assertEqual("0/2 done · 1 active · 1 ready",a_root["detail"])
        a_child=next(row for row in items if row["id"]==f"panel:w1:p1:task:{child['id']}")
        self.assertEqual({"them","start"}, {a_child["view"]["waiting"], a_child["view"]["action"]})
        self.assertEqual({"terminal_id":"b","placement":panel_b},a_child["terminal"])
        self.assertEqual(f"panel:w1:p1:task:{root['id']}",a_child["parent_id"])
        self.assertEqual("open",a_child["view"]["state"])
        self.assertIsInstance(a_child["view"]["since"],int)
        b_root=next(row for row in items if row["id"]=="panel:w1:p2:tasks")
        self.assertEqual("0/1 done · 1 ready",b_root["detail"])
        b_child=next(row for row in items if row["id"]==f"panel:w1:p2:task:{child['id']}")
        self.assertEqual({"you","start"}, {b_child["view"]["waiting"], b_child["view"]["action"]})
        self.assertEqual({"terminal_id":"a","placement":panel_a},b_child["terminal"])
        # Placement is required: no placement, no panel row.
        self.assertFalse(any("panel:" in row["id"] for row in document.project(self.store,self.snapshot([]))["items"]))

    def test_messages_view_shows_waiting_claimed_and_missing_replies(self):
        panel_a={"pane_id":"w1:p1","workspace_id":"w1","tab_id":"w1:t1"}
        panel_b={"pane_id":"w1:p2","workspace_id":"w1","tab_id":"w1:t1"}
        agent_b={"class":"agent","terminal_id":"b","agent":"pi","execution":"boot:1:2"}
        self.invoke({"action":"create","title":"Message anchor","operation_id":"anchor"},caller=A)
        # The extension's own message path: A sends, B claims.
        sent=self.store.messages.mutate({"action":"message_send","body":"please review the panel","recipient":agent_b}, A, "2026-10-04T00:00:00.000+00:00")
        message_id=sent["message"]["id"]
        self.store.messages.mutate({"action":"message_claim","id":message_id}, agent_b, "2026-10-04T00:01:00.000+00:00")
        refs=[{"terminal_id":"a","placement":panel_a},{"terminal_id":"b","placement":panel_b}]
        items=document.project(self.store,self.snapshot(refs))["items"]
        a_root=next(row for row in items if row["id"]=="panel:w1:p1:messages")
        self.assertEqual("Messages",a_root["label"])
        self.assertEqual("1 no reply",a_root["detail"])
        self.assertTrue(any(row["detail"].startswith("no reply to") for row in items if row["id"].startswith("panel:w1:p1:message:")))
        b_rows=[row for row in items if row["id"].startswith("panel:w1:p2:message:")]
        self.assertEqual(1,len(b_rows))
        self.assertEqual("claimed",b_rows[0]["detail"])
        self.assertTrue(b_rows[0]["label"].startswith("pi @") or "please review" in b_rows[0]["label"], b_rows[0])

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
        self.assertTrue(any("badge:session:global:0:1:0" in row["id"] for row in global_doc["items"]))
        self.assertNotIn("unavailable",view.durable_attention(task,None))
        human_row=next(row for row in global_doc["items"] if row["id"]==document.PREFIX+"row:"+task["id"])
        self.assertNotIn("terminal",human_row)
        self.assertNotIn("not running",human_row["detail"])
        self.assertEqual({"blocked"},view.durable_attention(task,{}))
        self.assertEqual([],view.project(tasks,None,{},set(),True,set(),7,100,100,scope={"kind":"tab","ids":["w1:t1"]},placements={"human":moved}))
    def test_owner_kind_and_terminal_presence_control_unavailability(self):
        for owner in (H,A):
            for present in (False,True):
                with self.subTest(owner=owner["class"],present=present):
                    task=self.invoke({"action":"create","title":"placement","operation_id":f"create-{owner['class']}-{present}"})["task"]
                    for action,fields in (("delegate",{"owner":owner}),("start",{}),("block",{"text":"needs help"})):
                        task=self.invoke({"action":action,"id":task["id"],"revision":task["revision"],
                                          "operation_id":f"{action}-{owner['class']}-{present}",**fields},caller=H)["task"]
                    refs=[{"terminal_id":owner["terminal_id"],"placement":P}] if present else []
                    doc=document.project(self.store,self.snapshot(refs))
                    row=next(row for row in doc["items"] if row["id"]==document.PREFIX+"row:"+task["id"])
                    categories=view.durable_attention(task,{owner["terminal_id"]:P} if present else {})
                    self.assertEqual(owner==A and not present,"unavailable" in categories)
                    self.assertEqual(owner==A and not present,row["detail"]=="not running")
                    self.assertEqual(present,"terminal" in row)

    def test_no_parent_or_acceptor_placement_guess_and_no_self_work_human_debt(self):
        task={"id":"child","state":"reported","owner":A,"owed_action":"accept","owed_to":{"role":"acceptors","bindings":[H]}}
        self.assertEqual({"human","unavailable"},view.durable_attention(task,{"human":P}))
        self.assertFalse(view.in_scope(task,{"human":P},{"kind":"workspace","ids":["w1"]}))
        self.assertTrue(view.in_scope(task,{"human":P},{"kind":"session","ids":["global"]}))
        self.assertEqual(set(),view.durable_attention({**task,"state":"in_progress","owner":H,"owed_action":"report","owed_to":H},{"human":P}))
    def test_read_only_store_never_initializes_or_migrates_and_sees_new_commits(self):
        with tempfile.TemporaryDirectory() as missing:
            self.assertIsNone(provider.read_only_store(missing))
            empty=document.project(None,self.snapshot())
            self.assertFalse(empty["actions"])
            self.assertTrue(any(row["id"].endswith("view:vqro.tasks:tree") for row in empty["items"]))
            self.assertFalse(any("badge:" in row["id"] or "row:" in row["id"] for row in empty["items"]))
            self.assertEqual([],list(Path(missing).iterdir()))
            absent=Path(missing)/"not-created"
            self.assertIsNone(provider.read_only_store(absent))
            self.assertFalse(absent.exists())
        observed=provider.read_only_store(self.root.name)
        try:
            with self.assertRaises(sqlite3.OperationalError):observed.db.execute("CREATE TABLE forbidden (id TEXT)")
            self.assertFalse(document.project(observed,self.snapshot())["actions"])
            task=self.invoke({"action":"decision","title":"quiet decision","operation_id":"decision"})["task"]
            self.assertTrue(any(row["label"]==task["title"] for row in document.project(observed,self.snapshot())["items"]))
            self.store.db.execute("PRAGMA user_version=5")
            with self.assertRaisesRegex(ValueError,"current store version"):provider.read_only_store(self.root.name)
            self.assertEqual(5,self.store.db.execute("PRAGMA user_version").fetchone()[0])
        finally:observed.db.close()
    def test_missing_store_then_first_commit_is_seen_by_same_document_process(self):
        with tempfile.TemporaryDirectory() as root:
            missing=Path(root)/"fresh"
            proc=subprocess.Popen([sys.executable,"-B",str(Path(provider.__file__).resolve()),"--native-attention","--data-dir",str(missing)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
            def send(value):
                proc.stdin.write(json.dumps(value)+"\n");proc.stdin.flush()
                import select
                self.assertTrue(select.select([proc.stdout],[],[],5)[0],"document reply timed out")
                return json.loads(proc.stdout.readline())
            store=None
            try:
                send({"type":"welcome","protocol":2,"granted_capabilities":["action_invoke","host_document"],"methods":["action.invoke.v1","host.document.render"]})
                request={"type":"request","id":"read","method":"host.document.render","params":self.snapshot()}
                empty=send(request)
                self.assertIn("result",empty)
                self.assertFalse(missing.exists())
                store=provider.Store(missing)
                task=store.invoke({"caller":H,"payload":{"action":"decision","title":"first human decision","done_when":"Choice recorded.","options":[{"key":"yes","label":"Yes"},{"key":"no","label":"No"}],"default":"yes","reversible":True,"waiting_cost":"Work waits.","operation_id":"first"}})["task"]
                populated=send({**request,"id":"after-commit"})
                self.assertTrue(any(row["label"]==task["title"] for row in populated["result"]["items"]))
            finally:
                if store is not None:store.db.close()
                proc.stdin.close()
                try:proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    proc.kill();proc.wait(timeout=5)
                proc.stdout.close();proc.stderr.close()

    def test_present_corrupt_or_dangling_store_is_not_empty(self):
        with tempfile.TemporaryDirectory() as root:
            path=Path(root)/"tasks.sqlite"
            path.write_bytes(b"not a database")
            with self.assertRaises(sqlite3.DatabaseError):provider.read_only_store(root)
            path.unlink()
            path.symlink_to(Path(root)/"missing-target")
            with self.assertRaises(sqlite3.OperationalError):provider.read_only_store(root)

    def test_protocol_document_only_missing_store_no_writes_and_current_version_outside_receipt(self):
        with tempfile.TemporaryDirectory() as missing:
            welcome={"type":"welcome","protocol":2,"granted_capabilities":["action_invoke","host_document"],"methods":["action.invoke.v1","host.document.render"]}
            request={"type":"request","id":"read","method":"host.document.render","params":self.snapshot()}
            proc=subprocess.run([sys.executable,"-B",str(Path(provider.__file__).resolve()),"--native-attention","--data-dir",missing],input=json.dumps(welcome)+"\n"+json.dumps(request)+"\n",capture_output=True,text=True,timeout=5)
            self.assertEqual(0,proc.returncode,proc.stderr)
            lines=[json.loads(line) for line in proc.stdout.splitlines()]
            self.assertNotIn("error",lines[1])
            self.assertEqual("host.view.v1",lines[1]["result"]["contract"])
            self.assertFalse(any("badge:" in row["id"] or "row:" in row["id"] for row in lines[1]["result"]["items"]))
            self.assertEqual([],list(Path(missing).iterdir()))
        welcome={"type":"welcome","protocol":2,"granted_capabilities":["action_invoke","host_document"],"methods":["action.invoke.v1","host.document.render"]}
        create={"action":"create","title":"receipt","done_when":"Recorded.","operation_id":"original"}
        requests=[{"type":"request","id":str(n),"method":"action.invoke.v1","params":{"caller":H,"payload":payload}} for n,payload in enumerate([create,{**create,"operation_id":"later"},create])]
        proc=subprocess.run([sys.executable,"-B",str(Path(provider.__file__).resolve()),"--native-attention","--data-dir",self.root.name],input="\n".join(json.dumps(value) for value in [welcome,*requests])+"\n",capture_output=True,text=True,timeout=5)
        self.assertEqual(0,proc.returncode,proc.stderr)
        results=[json.loads(line)["result"] for line in proc.stdout.splitlines()[1:]]
        self.assertEqual([1,2,2],[result["document_revision"] for result in results])
        self.assertEqual(results[0]["value"],results[2]["value"])
        self.assertTrue(all("document_revision" not in json.loads(row[0]) for row in self.store.db.execute("SELECT outcome FROM operations")))
