"""Native attention policy and read-only producer characterization."""
import copy
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

    def test_empty_tasks_view_needs_only_current_placement_not_a_store_or_binding(self):
        for store in (None, self.store):
            with self.subTest(store=store is not None):
                before = None if store is None else list(store.db.iterdump())
                doc = document.project(store, self.snapshot([{"terminal_id": "plain-shell", "placement": P}]))
                self.assertEqual("host.view.v1", doc["contract"])
                root = next(row for row in doc["items"] if row["id"] == "panel:w1:p1:tasks")
                self.assertEqual({"pane_id": "w1:p1", "view_id": "tasks", "state": "open"}, root["view"])
                self.assertEqual("0/0 done", root["detail"])
                rows = [row for row in doc["items"] if row.get("parent_id") == root["id"]]
                self.assertEqual(["No tasks"], [row["label"] for row in rows])
                self.assertNotIn("terminal", rows[0])
                self.assertEqual({"state": "open"}, rows[0]["view"])
                self.assertFalse(rows[0].get("action_ids"))
                self.assertEqual([], doc["actions"])
                if store is not None:
                    self.assertEqual(before, list(store.db.iterdump()))
                for refs in ([], None):
                    snapshot = self.snapshot([]) if refs == [] else {"generation": 7, "revision": 1}
                    self.assertFalse(any(row.get("view", {}).get("view_id") == "tasks"
                                         for row in document.project(store, snapshot)["items"]))
                moved = {**P, "pane_id": "w2:p7", "workspace_id": "w2", "tab_id": "w2:t3"}
                roots = [row["view"]["pane_id"] for row in document.project(
                    store, self.snapshot([{"terminal_id": "plain-shell", "placement": moved}]))["items"]
                         if row.get("view", {}).get("view_id") == "tasks"]
                self.assertEqual(["w2:p7"], roots)

    def test_tasks_in_another_scope_do_not_prevent_a_healthy_empty_tasks_view(self):
        self.invoke({"action": "create", "title": "Elsewhere", "operation_id": "elsewhere"}, caller=A)
        doc = document.project(self.store, self.snapshot())
        root = next(row for row in doc["items"] if row["id"] == "panel:w1:p1:tasks")
        self.assertEqual(["No tasks"], [row["label"] for row in doc["items"] if row.get("parent_id") == root["id"]])

    def test_empty_task_panels_keep_the_existing_item_budget(self):
        placements = {f"t{index}": {**P, "pane_id": f"w1:p{index}"} for index in range(300)}
        for room in (0, 1, 2, 3, 4, 7, 252):
            items = document.panel_items([], placements, room, empty_only=True)
            self.assertLessEqual(len(items), room)
            roots = {row["id"] for row in items if "parent_id" not in row}
            self.assertTrue(all(row["parent_id"] in roots for row in items if "parent_id" in row))

    def test_empty_task_panels_do_not_displace_populated_tasks_or_messages(self):
        task = self.invoke({"action": "create", "title": "Real task wins", "operation_id": "real-task"}, caller=A)["task"]
        self.store.messages.mutate({"action": "message_send", "body": "Real message wins", "recipient": A},
                                   H, "2026-10-04T00:00:00.000+00:00")
        refs = [{"terminal_id": f"empty-{index}", "placement": {**P, "pane_id": f"w1:p{index:03}"}}
                for index in range(300)]
        refs.append({"terminal_id": "a", "placement": {**P, "pane_id": "w1:pzz-last"}})
        doc = document.project(self.store, self.snapshot(refs))
        self.assertLessEqual(len(doc["items"]), 256)
        self.assertLessEqual(len(provider.canonical(doc).encode()), document.DOCUMENT_BYTES)
        self.assertTrue(any(row["label"] == task["title"] for row in doc["items"]))
        self.assertTrue(any("Real message wins" in row["label"] for row in doc["items"]))
        task = self.change(task, "report", text=f"> {task['done_when']}\nEvidence recorded")
        self.change(task, "accept")
        doc = document.project(self.store, self.snapshot([refs[-1]]))
        root = next(row for row in doc["items"] if row["id"] == "panel:w1:pzz-last:tasks")
        self.assertEqual("1/1 done", root["detail"])
        self.assertFalse(any(row["label"] == "No tasks" for row in doc["items"]))

    def test_empty_task_panels_preserve_attention_overflow_room(self):
        for index in range(260):
            self.invoke({"action": "create", "title": f"Unplaced {index}", "operation_id": f"unplaced-{index}"}, caller=A)
        refs = [{"terminal_id": f"empty-{index}", "placement": {**P, "pane_id": f"w1:p{index:03}"}}
                for index in range(300)]
        doc = document.project(self.store, self.snapshot(refs))
        self.assertLessEqual(len(doc["items"]), 256)
        self.assertTrue(any(row["id"] == document.PREFIX + "overflow:tasks" for row in doc["items"]))

    def test_unreadable_tasks_table_is_a_provider_error_not_an_empty_task_view(self):
        self.store.db.execute("DROP TABLE tasks")
        with self.assertRaises(sqlite3.OperationalError):
            document.project(self.store, self.snapshot())

    def test_empty_messages_view_needs_only_current_placement_not_a_store_or_binding(self):
        for store in (None, self.store):
            with self.subTest(store=store is not None):
                before = None if store is None else list(store.db.iterdump())
                doc = document.project(store, self.snapshot([{"terminal_id": "a", "placement": P}]))
                root = next(row for row in doc["items"] if row["id"] == "panel:w1:p1:messages")
                self.assertEqual({"pane_id": "w1:p1", "view_id": "messages", "state": "open"}, root["view"])
                rows = [row for row in doc["items"] if row.get("parent_id") == root["id"]]
                self.assertEqual(["No messages"], [row["label"] for row in rows])
                self.assertNotIn("terminal", rows[0])
                self.assertFalse(rows[0].get("action_ids"))
                self.assertEqual([], doc["actions"])
                if store is not None:
                    self.assertEqual(before, list(store.db.iterdump()))
                self.assertFalse(any(row.get("view", {}).get("view_id") == "messages"
                                     for row in document.project(store, self.snapshot([]))["items"]))
                moved = {**P, "pane_id": "w2:p7", "workspace_id": "w2", "tab_id": "w2:t3"}
                roots = [row["view"]["pane_id"] for row in document.project(
                    store, self.snapshot([{"terminal_id": "a", "placement": moved}]))["items"]
                         if row.get("view", {}).get("view_id") == "messages"]
                self.assertEqual(["w2:p7"], roots)

    def test_empty_message_panels_keep_the_existing_item_budget(self):
        placements = {f"t{index}": {**P, "pane_id": f"w1:p{index}"} for index in range(300)}
        for room in (0, 1, 2, 3, 4, 7, 252):
            items = document.message_panel_items(placements, [], ({}, {}, set()), room)
            self.assertLessEqual(len(items), room)
            roots = {row["id"] for row in items if "parent_id" not in row}
            self.assertTrue(all(row["parent_id"] in roots for row in items if "parent_id" in row))
        refs = [{"terminal_id": key, "placement": value} for key, value in placements.items()]
        doc = document.project(self.store, self.snapshot(refs))
        self.assertLessEqual(len(doc["items"]), 256)
        self.assertLessEqual(len(provider.canonical(doc).encode()), document.DOCUMENT_BYTES)

    def test_empty_slices_do_not_displace_real_messages_at_the_item_bound(self):
        self.store.messages.mutate({"action": "message_send", "body": "real message wins", "recipient": A},
                                   H, "2026-10-04T00:00:00.000+00:00")
        refs = [{"terminal_id": f"empty-{index}", "placement": {**P, "pane_id": f"w1:p{index:03}"}}
                for index in range(300)]
        refs.append({"terminal_id": "a", "placement": {**P, "pane_id": "w1:pzz-last"}})
        doc = document.project(self.store, self.snapshot(refs))
        self.assertLessEqual(len(doc["items"]), 256)
        self.assertTrue(any("real message wins" in row["label"] for row in doc["items"]))

    def test_unreadable_messages_table_is_a_provider_error_not_an_empty_inbox(self):
        self.store.db.execute("DROP TABLE messages")
        with self.assertRaises(sqlite3.OperationalError):
            document.project(self.store, self.snapshot([{"terminal_id": "a", "placement": P}]))

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
        self.assertFalse(any(row["label"] == "No messages" for row in items))
        self.assertEqual("UNRESOLVED · 0 recorded waiting",a_root["detail"])
        self.assertTrue(any(row.get("detail", "").startswith("no recorded reply") for row in items if row["id"].startswith("panel:w1:p1:message:")))
        b_rows=[row for row in items if row["id"].removeprefix("panel:w1:p2:message:").isdigit()]
        self.assertEqual(1,len(b_rows))
        self.assertEqual("claimed", b_rows[0]["detail"])
        context = next(row for row in items if row["id"] == b_rows[0]["parent_id"])
        self.assertEqual("Recorded recipient: pi · boot:1:2", context["label"])
        self.assertTrue(b_rows[0]["label"].startswith("pi @") or "please review" in b_rows[0]["label"], b_rows[0])

    def message_slice(self, doc, pane="w1:p1"):
        return [row for row in doc["items"] if row["id"].startswith(f"panel:{pane}:message")]

    def seed_messages(self, count, recipient=A, sender=None):
        sender = sender or {"class": "agent", "terminal_id": "b", "agent": "pi", "execution": "boot:1:2"}
        at = "2026-10-04T00:00:00.000+00:00"
        self.store.db.execute("BEGIN")
        self.store.db.executemany(
            "INSERT INTO messages(id,sender,recipient,recipient_execution,body,reply_to,state,revision,created_at,updated_at) "
            "VALUES (?,?,?,?,?,NULL,'waiting',1,?,?)",
            ((f"msg_{index:032x}", provider.canonical(sender), provider.canonical(recipient), recipient["execution"],
              f"Stored message {index}", at, at) for index in range(count)))
        self.store.db.commit()

    def test_replacement_and_single_historical_context_are_unresolved_not_actionable(self):
        self.invoke({"action": "create", "title": "old execution anchor", "operation_id": "old-anchor"}, caller=A)
        replacement = {**A, "execution": "boot:1:replacement"}
        self.store.messages.mutate({"action": "message_send", "recipient": replacement, "body": "replacement waiting"}, H,
                                   "2026-10-04T00:00:00.000+00:00")
        before = list(self.store.db.iterdump()), self.store.db.total_changes
        doc = document.project(self.store, self.agent_snapshot())
        rows = self.message_slice(doc)
        text = json.dumps(rows, ensure_ascii=False)
        self.assertIn("UNRESOLVED", text)
        self.assertIn("1 recorded waiting", text)
        self.assertIn("Recorded recipient: pi · boot:1:replacement", text)
        self.assertIn("replacement waiting", text)
        self.assertNotIn("No messages", text)
        self.assertTrue(all(not any(key in row.get("view", {}) for key in ("action", "waiting", "affordance")) for row in rows))
        self.assertTrue(all(not row.get("action_ids") for row in rows))
        self.assertEqual([], doc["actions"])
        self.assertEqual(before, (list(self.store.db.iterdump()), self.store.db.total_changes))
        # Even one recorded execution does not attest that it is current.
        self.assertIn("Current execution not established", text)

    def test_complete_quota_headers_keep_old_messages_visible_beyond_preview(self):
        other = {**A, "terminal_id": "other", "execution": "boot:2:2"}
        self.seed_messages(provider.MAX_RECORDS, recipient=other)
        self.store.db.execute("UPDATE messages SET recipient=?,recipient_execution=? WHERE id=?",
                              (provider.canonical(A), A["execution"], "msg_" + "0" * 32))
        before = list(self.store.db.iterdump())
        trace = []
        self.store.db.set_trace_callback(trace.append)
        try:
            doc = document.project(self.store, self.snapshot([
                {"terminal_id": "a", "placement": P},
                {"terminal_id": "other", "placement": {**P, "pane_id": "w1:p2"}}]))
        finally:
            self.store.db.set_trace_callback(None)
        self.assertIn("UNRESOLVED · 9999 recorded waiting", json.dumps(self.message_slice(doc, "w1:p2"), ensure_ascii=False))
        text = json.dumps(self.message_slice(doc), ensure_ascii=False)
        self.assertIn("UNRESOLVED", text)
        self.assertIn("1 recorded waiting", text)
        self.assertIn("Recorded recipient: pi · boot:1:1", text)
        self.assertIn("1 message previews omitted", text)
        self.assertNotIn("No messages", text)
        self.assertEqual(1, sum("SELECT id,sender,recipient,recipient_execution,state,reply_to" in query for query in trace))
        self.assertEqual(1, sum("SELECT * FROM messages" in query for query in trace))
        self.assertEqual("BEGIN", trace[0])
        self.assertEqual("ROLLBACK", trace[-1])
        self.assertEqual(before, list(self.store.db.iterdump()))
        self.assertLessEqual(len(doc["items"]), 256)
        self.assertLessEqual(len(provider.canonical(doc).encode()), document.DOCUMENT_BYTES)
        self.store.db.execute("INSERT INTO messages SELECT 'over-quota',sender,recipient,recipient_execution,body,reply_to,state,revision,created_at,updated_at,claimed_at FROM messages LIMIT 1")
        with self.assertRaisesRegex(ValueError, "exceeds store quota"):
            document.project(self.store, self.agent_snapshot())

    def test_reply_outside_preview_is_not_reported_missing_and_unrelated_scope_is_empty(self):
        self.seed_messages(201, recipient=A)
        # The newest preview includes message200 sent by b. Its recorded reply is
        # outside the body window; complete header evidence still discharges it.
        self.store.db.execute("UPDATE messages SET reply_to=? WHERE id=?", (f"msg_{200:032x}", f"msg_{0:032x}"))
        panel_b = {**P, "pane_id": "w1:p2"}
        refs = [{"terminal_id": "b", "placement": panel_b}, {"terminal_id": "unrelated", "placement": P}]
        doc = document.project(self.store, self.snapshot(refs))
        rows = self.message_slice(doc, "w1:p2")
        latest = next(row for row in rows if row["label"] == "Stored message 200")
        self.assertEqual("recorded reply", latest["detail"])
        context = next(row for row in rows if row["id"] == latest["parent_id"])
        self.assertEqual("Recorded sender: pi · boot:1:2", context["label"])
        self.assertNotIn("no recorded reply", latest["detail"])
        self.assertIn("UNRESOLVED · 0 recorded waiting", json.dumps(rows, ensure_ascii=False))
        self.assertEqual(["No messages"], [row["label"] for row in self.message_slice(doc) if "parent_id" in row])

    def test_malformed_header_outside_preview_fails_closed(self):
        self.seed_messages(201)
        self.store.db.execute("UPDATE messages SET recipient='{}' WHERE id=?", ("msg_" + "0" * 32,))
        before = list(self.store.db.iterdump())
        with self.assertRaises(ValueError):
            document.project(self.store, self.agent_snapshot())
        self.assertEqual(before, list(self.store.db.iterdump()))

    def test_message_headers_and_preview_share_one_snapshot_during_a_write(self):
        writer = provider.Store(self.root.name)
        changed = []
        def trace(query):
            if query.startswith("SELECT id,sender,recipient,recipient_execution,state,reply_to") and not changed:
                changed.append(True)
                writer.invoke({"caller": H, "payload": {"action": "message_send", "operation_id": "concurrent",
                               "recipient": A, "body": "concurrent message"}})
        self.store.db.set_trace_callback(trace)
        try:
            before = document.project(self.store, self.agent_snapshot())
        finally:
            self.store.db.set_trace_callback(None)
            writer.db.close()
        self.assertEqual([True], changed)
        self.assertIn("No messages", provider.canonical(self.message_slice(before)))
        after = json.dumps(self.message_slice(document.project(self.store, self.agent_snapshot())), ensure_ascii=False)
        self.assertIn("UNRESOLVED · 1 recorded waiting", after)
        self.assertIn("concurrent message", after)

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

    def test_protocol_empty_tasks_view_and_store_failures_remain_distinct(self):
        welcome = {"type": "welcome", "protocol": 2, "granted_capabilities": ["action_invoke", "host_document"],
                   "methods": ["action.invoke.v1", "host.document.render"]}
        request = {"type": "request", "id": "read", "method": "host.document.render", "params": self.snapshot()}
        for state in ("missing", "empty", "corrupt", "unsupported", "unreadable-tasks", "unreadable-messages"):
            with self.subTest(state=state), tempfile.TemporaryDirectory() as root:
                path = Path(root) / "tasks.sqlite"
                if state == "corrupt":
                    path.write_bytes(b"not a database")
                elif state != "missing":
                    store = provider.Store(root)
                    if state == "unsupported":
                        store.db.execute("PRAGMA user_version=5")
                    elif state.startswith("unreadable-"):
                        store.db.execute(f"DROP TABLE {state.removeprefix('unreadable-')}")
                    store.db.close()
                before = path.read_bytes() if path.exists() else None
                proc = subprocess.run([sys.executable, "-B", str(Path(provider.__file__).resolve()),
                                       "--native-attention", "--data-dir", root],
                                      input=json.dumps(welcome) + "\n" + json.dumps(request) + "\n",
                                      capture_output=True, text=True, timeout=5)
                self.assertEqual(0, proc.returncode, proc.stderr)
                reply = json.loads(proc.stdout.splitlines()[1])
                if state in ("missing", "empty"):
                    self.assertNotIn("error", reply)
                    self.assertEqual("host.view.v1", reply["result"]["contract"])
                    root_item = next(row for row in reply["result"]["items"] if row["id"] == "panel:w1:p1:tasks")
                    self.assertEqual(["No tasks"], [row["label"] for row in reply["result"]["items"]
                                                   if row.get("parent_id") == root_item["id"]])
                else:
                    self.assertIn("error", reply)
                    self.assertNotIn("result", reply)
                self.assertEqual(before, path.read_bytes() if path.exists() else None)
                if state == "missing":
                    self.assertEqual([], list(Path(root).iterdir()))

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

    def test_every_changed_message_projection_moves_its_publication_version(self):
        agent_b = {"class": "agent", "terminal_id": "b", "agent": "pi", "execution": "boot:1:2"}
        refs = [{"terminal_id": "a", "placement": P},
                {"terminal_id": "b", "placement": {**P, "pane_id": "w1:p2"}}]
        snapshot = self.snapshot(refs)
        def observed():
            return self.store.document_revision(), provider.canonical(document.project(self.store, snapshot))
        previous = observed()
        self.assertIn("No messages", previous[1])
        second = None
        for payload, caller in [
            ({"action": "message_send", "operation_id": "first", "recipient": agent_b, "body": "First message"}, A),
            ({"action": "message_send", "operation_id": "second", "recipient": agent_b, "body": "Second message"}, A),
            ({"action": "message_claim", "operation_id": "pickup"}, agent_b),
        ]:
            result = self.store.invoke({"caller": caller, "payload": payload})
            if payload["operation_id"] == "second":
                second = result["message"]["id"]
            current = observed()
            self.assertNotEqual(previous[1], current[1])
            self.assertGreater(current[0], previous[0])
            previous = current
        self.store.invoke({"caller": agent_b, "payload": {"action": "message_send", "operation_id": "reply",
                           "recipient": A, "body": "Review recorded", "reply_to": second}})
        current = observed()
        self.assertNotEqual(previous[1], current[1])
        self.assertGreater(current[0], previous[0])
        self.store.invoke({"caller": A, "payload": {"action": "message_send", "operation_id": "second",
                           "recipient": agent_b, "body": "Second message"}})
        self.store.invoke({"caller": A, "payload": {"action": "message_list"}})
        self.assertEqual(current, observed())

    def panel_rows(self, doc):
        return [row for row in doc["items"] if row["id"].startswith("panel:") and ":task:" in row["id"]]

    def agent_snapshot(self):
        return self.snapshot([{"terminal_id": "a", "placement": P}])

    def test_reported_human_debt_describes_inert_acceptance_at_displayed_revision(self):
        task = self.invoke({"action": "create", "title": "Agent report", "operation_id": "report-root"}, caller=A)["task"]
        task = self.change(task, "report", text=f"> {task['done_when']}\nEvidence for review")
        doc = document.project(self.store, self.agent_snapshot())
        row = next(row for row in self.panel_rows(doc) if row["id"].endswith(task["id"]))
        self.assertEqual("you", row["view"]["waiting"])
        self.assertEqual({"kind": "action", "label": "Accept work", "detail": task["title"],
                          "subject": {"id": task["id"], "revision": task["revision"]}}, row["view"]["affordance"])
        self.assertEqual([], doc["actions"])
        self.assertTrue(all(not row.get("action_ids") for row in doc["items"]))
        self.assertNotIn("caller", json.dumps(row["view"]["affordance"]))
        task = self.change(task, "accept")
        self.assertFalse(any(row["id"].endswith(task["id"]) for row in self.panel_rows(document.project(self.store, self.agent_snapshot()))))

    def test_decision_lists_exact_choices_default_and_decision_wide_reversibility(self):
        for reversible in (True, False):
            with self.subTest(reversible=reversible):
                task = self.invoke({"action": "decision", "title": "Choose a route", "operation_id": f"decision-{reversible}",
                                    "options": [{"key": "wait", "label": "Wait"}, {"key": "go", "label": "Go now"}],
                                    "default": "wait", "reversible": reversible}, caller=A)["task"]
                doc = document.project(self.store, self.agent_snapshot())
                row = next(row for row in self.panel_rows(doc) if row["id"].endswith(task["id"]))
                self.assertEqual("you", row["view"]["waiting"])
                hint = row["view"]["affordance"]
                self.assertEqual({"kind": "choice", "label": "Answer", "detail": task["title"], "subject": {"id": task["id"], "revision": task["revision"]},
                                  "options": task["decision"]["options"], "default": "wait", "reversible": reversible}, hint)
                self.assertFalse(any("reversible" in option for option in hint["options"]))
                self.assertEqual([], doc["actions"])
                # A hint is not authorization: the ordinary provider still rejects an agent.
                with self.assertRaisesRegex(ValueError, "human-only"):
                    self.invoke({"action": "answer", "id": task["id"], "revision": task["revision"],
                                 "option": "go", "operation_id": f"agent-answer-{reversible}"}, caller=A)

    def test_human_class_metadata_does_not_assume_the_server_account_is_the_caller(self):
        task = self.invoke({"action": "create", "title": "Human review", "operation_id": "human-review"})["task"]
        task = self.change(task, "report", text=f"> {task['done_when']}\nEvidence")
        # Same presentation for human roles and explicit human accounts, including
        # a foreign account: actual eligibility belongs only to provider invocation.
        for binding in ({"class": "human_unverified"}, {"role": "human_unverified"},
                        {**H, "uid": (H["uid"] + 1) % 2**32},
                        {"class": "human_unverified", "terminal_id": "elsewhere", "sid": "S-1-5-21-123"}):
            candidate = {**task, "owed_to": {"role": "acceptors", "bindings": [A, binding]}}
            self.assertIsNotNone(document.human_affordance(candidate, set()))
        for owed in (None, A, {"role": "acceptors", "bindings": []}, {"role": "acceptors", "bindings": [A]}):
            self.assertIsNone(document.human_affordance({**task, "owed_to": owed}, set()))
        for state in ("open", "in_progress", "blocked", "accepted", "done"):
            self.assertIsNone(document.human_affordance({**task, "state": state}, set()))
        for action in ("start", "work/report", "unblock", "acceptor_missing"):
            self.assertIsNone(document.human_affordance({**task, "owed_action": action}, set()))

    def test_decision_dependencies_and_legacy_unfinished_children_keep_rows_read_only(self):
        prerequisite = self.invoke({"action": "create", "title": "Prerequisite", "operation_id": "prerequisite"}, caller=A)["task"]
        task = self.invoke({"action": "decision", "title": "Blocked decision", "depends_on": [prerequisite["id"]],
                            "operation_id": "dependent-decision"}, caller=A)["task"]
        row = next(row for row in self.panel_rows(document.project(self.store, self.agent_snapshot())) if row["id"].endswith(task["id"]))
        self.assertNotIn("affordance", row["view"])
        self.assertIsNone(document.human_affordance({**task, "waiting_on": []}, {task["id"]}))
        self.assertIsNotNone(document.human_affordance({**task, "waiting_on": []}, set()))
        # Current creation rejects decision children; characterize restored legacy
        # records too, rather than advertising an answer the provider cannot apply.
        self.store.db.execute("DELETE FROM dependencies WHERE task_id=?", (task["id"],))
        self.store.db.execute("UPDATE tasks SET parent=? WHERE id=?", (task["id"], prerequisite["id"]))
        row = next(row for row in self.panel_rows(document.project(self.store, self.agent_snapshot())) if row["id"].endswith(task["id"]))
        self.assertNotIn("affordance", row["view"])
        self.store.db.execute("UPDATE tasks SET state='done' WHERE id=?", (prerequisite["id"],))
        row = next(row for row in self.panel_rows(document.project(self.store, self.agent_snapshot())) if row["id"].endswith(task["id"]))
        self.assertIn("affordance", row["view"])

    def test_unsupported_or_malformed_optional_hints_never_offer_free_text(self):
        task = self.invoke({"action": "decision", "title": "Listed choices only", "operation_id": "malformed"}, caller=A)["task"]
        terms = task["decision"]
        malformed = [None, {}, {**terms, "options": []}, {**terms, "options": terms["options"] * 3},
                     {**terms, "options": [terms["options"][0]] * 2}, {**terms, "default": "missing"},
                     {**terms, "reversible": "yes"}, {**terms, "resolution": {"option": "yes"}},
                     {**terms, "options": [{"key": "a", "label": "x" * 257}, {"key": "b", "label": "B"}]},
                     {**terms, "options": [{"key": "a b", "label": "A"}, {"key": "b", "label": "B"}]},
                     {**terms, "options": [{"key": "a", "label": "bad\nlabel"}, {"key": "b", "label": "B"}]},
                     {**terms, "options": [{"key": "a", "label": "A", "text": "free"}, {"key": "b", "label": "B"}]}]
        for value in malformed:
            with self.subTest(terms=value):
                self.assertIsNone(document.human_affordance({**task, "decision": value}, set()))
        for revision in (None, True, 0, -1, "1", 2**63):
            self.assertIsNone(document.human_affordance({**task, "revision": revision}, set()))
        for kind in ("legacy", "unknown"):
            self.assertIsNone(document.human_affordance({**task, "kind": kind}, set()))
        # A legacy unstructured decision is still displayed but has no hint.
        self.store.db.execute("UPDATE tasks SET decision=NULL WHERE id=?", (task["id"],))
        row = next(row for row in self.panel_rows(document.project(self.store, self.agent_snapshot())) if row["id"].endswith(task["id"]))
        self.assertNotIn("affordance", row["view"])
        self.assertEqual(task["title"], row["label"])

    def test_serialized_hint_budget_preserves_read_only_overflow_and_never_writes_store(self):
        options = [{"key": str(n), "label": "é" * 128} for n in range(4)]
        for n in range(document.PANEL_ROW_LIMIT + 1):
            self.invoke({"action": "decision", "title": f"Question {n}", "operation_id": f"budget-{n}",
                         "options": options, "default": "0"}, caller=A)
        before = self.store.db.total_changes, self.store.document_revision(), list(self.store.db.iterdump())
        observed = provider.read_only_store(self.root.name)
        try:
            doc = document.project(observed, self.agent_snapshot())
            encoded = provider.canonical(doc).encode()
            rows = self.panel_rows(doc)
            hints = [row["view"]["affordance"] for row in rows if "affordance" in row["view"]]
            self.assertGreater(len(hints), 0)
            self.assertLess(len(hints), len(rows))
            self.assertLessEqual(len(doc["items"]), 256)
            self.assertLessEqual(len(encoded), document.DOCUMENT_BYTES)
            self.assertLessEqual(sum(len(provider.canonical(hint).encode()) for hint in hints), document.AFFORDANCES_BYTES)
            self.assertTrue(all(len(provider.canonical(hint).encode()) <= document.AFFORDANCE_BYTES for hint in hints))
            self.assertEqual([], doc["actions"])
            self.assertTrue(all(not row.get("action_ids") for row in doc["items"]))
            self.assertEqual(0, observed.db.total_changes)
        finally:
            observed.db.close()
        self.assertEqual(before, (self.store.db.total_changes, self.store.document_revision(), list(self.store.db.iterdump())))

    def test_hint_is_a_detached_copy_of_the_displayed_task_not_a_mutation(self):
        task = self.invoke({"action": "decision", "title": "Snapshot", "operation_id": "snapshot"}, caller=A)["task"]
        original = copy.deepcopy(task)
        hint = document.human_affordance(task, set())
        hint["options"][0]["label"] = "Local presentation only"
        hint["subject"]["revision"] += 1
        self.assertEqual(original, task)
