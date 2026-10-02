"""Pure view/reader characterization, not real-host acceptance."""
import copy
import datetime as dt
import unittest
import tempfile
from unittest import mock
import provider
import view

A = {"class": "agent", "terminal_id": "term_a", "agent": "pi", "execution": "boot:1:10"}
B = {**A, "terminal_id": "term_b", "execution": "boot:2:20"}
H = {"class": "human_unverified", "terminal_id": "human", "uid": view.os.geteuid()}
STAMP = "2026-10-01T01:02:03.000+00:00"
WALL = dt.datetime(2026, 10, 2, tzinfo=dt.timezone.utc).timestamp()


def task(key="root", owner=A, state="open", parent=None, **fields):
    return {"id": key, "owner": owner, "state": state, "parent": parent, "title": key, "kind": "task", "creator": A,
            "delegator": None, "owed_to": owner, "owed_action": "start", "owed_since": STAMP, "updated_at": STAMP, **fields}


def agent(owner=A, status="idle", **fields):
    return {**owner, "name": "a" if owner == A else "b", "pane_id": "w1:p1", "agent_status": status, "state_change_seq": 1, **fields}


class ViewTests(unittest.TestCase):
    def test_exact_binding_and_unavailability_are_not_metadata_substitution(self):
        self.assertTrue(view.exact(A, agent()))
        for field in ("terminal_id", "agent", "execution"):
            self.assertFalse(view.exact(A, agent(**{field: "replacement"})))
        self.assertEqual("unknown", view.availability(A, None))
        self.assertEqual("unknown", view.availability(A, {"term_a": agent(execution=None)}))
        self.assertEqual("unavailable here", view.availability(A, {}))
        self.assertEqual("unavailable here", view.availability(A, {"term_a": agent(execution="boot:1:11")}))
        self.assertEqual("human", view.availability(H, {}))

    def test_stalled_is_observed_idle_leaf_only_not_parent_waiting_for_children(self):
        tasks = {"root": task(), "child": task("child", B, parent="root")}
        agents = {"term_a": agent(), "term_b": agent(B)}
        pending = {"root"}
        self.assertNotIn("stalled", view.categories(tasks["root"], pending, agents, {A["execution"]: 0}, 100))
        self.assertNotIn("stalled", view.categories(tasks["child"], pending, agents, {B["execution"]: 0}, 59))
        self.assertIn("stalled", view.categories(tasks["child"], pending, agents, {B["execution"]: 0}, 60))
        self.assertNotIn("stalled", view.categories(task(state="blocked"), set(), agents, {A["execution"]: 0}, 100))
        self.assertNotIn("stalled", view.categories(task(), set(), None, {A["execution"]: 0}, 100))

    def test_human_debt_durable_state_and_live_filters_are_distinct(self):
        self.assertIn("waiting on me", view.categories(task(owed_to={"role": "human_unverified"}), set(), {}, {}, 100))
        self.assertIn("waiting on me", view.categories(task(owner=H), set(), {}, {}, 100))
        self.assertEqual({"done/accepted"}, view.categories(task(state="accepted"), set(), {"term_a": agent(status="working")}, {}, 100))
        self.assertEqual({"working"}, view.categories(task(), set(), {"term_a": agent(status="working")}, {}, 100))
        self.assertEqual({"blocked", "idle"}, view.categories(task(state="blocked"), set(), {"term_a": agent()}, {}, 100))

    def test_legacy_debtor_shape_keeps_human_and_exact_agent_meaning(self):
        role = {"role": "human_unverified"}
        self.assertEqual([role], view.debtors(role))
        self.assertEqual("human (unverified)", view.label(role, {}))
        self.assertIn("waiting on me", view.categories(task(state="reported", owed_to=role, owed_action="accept"), set(), {}, {}, 100))
        self.assertEqual([B], view.debtors(B))
        self.assertEqual("b", view.label(B, {"term_b": agent(B)}))
        self.assertNotIn("waiting on me", view.categories(task(owed_to=B), set(), {}, {}, 100))
        self.assertEqual([], view.debtors(None))

    def test_acceptor_set_shape_uses_only_provider_bindings_not_inferred_humans(self):
        agents = {"term_a": agent(), "term_b": agent(B)}
        owed = {"role": "acceptors", "bindings": [A, H]}
        self.assertEqual([A, H], view.debtors(owed))
        self.assertEqual("acceptors: a, human (unverified)", view.label(owed, agents))
        current = task(owner=B, state="reported", owed_to=owed, owed_action="accept")
        before = copy.deepcopy(current)
        self.assertIn("waiting on me", view.categories(current, set(), agents, {}, 100))
        self.assertEqual(before, current)
        current.update(owed_to={"role": "acceptors", "bindings": [A]}, creator=H, delegator=H)
        self.assertNotIn("waiting on me", view.categories(current, set(), agents, {}, 100))
        current["owed_to"]["bindings"] = []
        self.assertNotIn("waiting on me", view.categories(current, set(), agents, {}, 100))
        self.assertEqual("acceptors: unknown", view.label(current["owed_to"], agents))
        current["owed_action"] = "acceptor_missing"
        self.assertEqual(B, view.recipient(current, {}))
        self.assertIn("blocked", view.categories(current, set(), agents, {}, 100))
        self.assertEqual([("root", 0, False, False)], view.project({"root": current}, agents, {}, view.DEFAULT, True, set(), 7, 100, WALL))
        current.update(kind="decision", owed_action="answer")
        self.assertIn("waiting on me", view.categories(current, set(), agents, {}, 100))
        current["state"] = "done"
        self.assertEqual({"done/accepted"}, view.categories(current, set(), agents, {}, 100))

    def test_entirely_positive_stale_acceptance_debt_enters_default_attention_only(self):
        current = task(owner=B, state="reported", owed_action="accept", owed_to={"role": "acceptors", "bindings": [A]})
        agents = {"term_b": agent(B)}
        args = ({"root": current}, agents, {}, view.DEFAULT, True, set(), 7, 100, WALL)
        self.assertTrue(view.stale_acceptors(current, agents))
        self.assertEqual([("root", 0, False, False)], view.project(*args))
        self.assertNotIn("blocked", view.categories(current, set(), agents, {}, 100))
        self.assertEqual([], view.project({"root": current}, agents, {}, {"blocked"}, False, set(), 7, 100, WALL))
        for observed in (None, {**agents, "term_a": agent(execution=None)}, {**agents, "term_a": agent()}):
            self.assertFalse(view.stale_acceptors(current, observed))
            self.assertEqual([], view.project({"root": current}, observed, {}, view.DEFAULT, True, set(), 7, 100, WALL))
        current["owed_to"]["bindings"] = [A, H]
        self.assertFalse(view.stale_acceptors(current, agents))
        current["owed_to"]["bindings"] = []
        self.assertFalse(view.stale_acceptors(current, agents))
        current.update(state="done", owed_to=None)
        self.assertEqual([], view.project({"root": current}, agents, {}, view.DEFAULT, True, set(), 7, 100, WALL))

    def test_acceptor_display_distinguishes_stale_unknown_and_other_uid(self):
        owed = {"role": "acceptors", "bindings": [A]}
        self.assertEqual("acceptors: [stale here] pi @term_a", view.label(owed, {}))
        self.assertEqual("acceptors: [stale here] pi @term_a", view.label(owed, {"term_a": agent(execution="replacement")}))
        self.assertEqual("acceptors: [unknown] pi @term_a", view.label(owed, None))
        self.assertEqual("acceptors: [unknown] pi @term_a", view.label(owed, {"term_a": agent(execution=None)}))
        foreign = {**H, "uid": H["uid"] + 1}
        owed["bindings"] = [foreign]
        self.assertIn(f"uid {foreign['uid']}", view.label(owed, {}))
        self.assertNotIn("waiting on me", view.categories(task(owed_to=owed), set(), {}, {}, 100))

    def test_recipient_composition_preserves_delegator_and_decision_creator(self):
        tasks = {"root": task()}
        self.assertEqual(A, view.recipient(task("child", B, parent="root"), tasks))
        self.assertEqual(H, view.recipient(task("child", B, parent="root", delegator=H), tasks))
        self.assertEqual(B, view.recipient(task(owner=B, owed_action="accept"), tasks))
        self.assertEqual(A, view.recipient(task(owner=B, owed_action="answer"), tasks))

    def test_filters_preserve_dimmed_context_and_collapse_is_read_only(self):
        tasks = {"root": task(), "child": task("child", B, "blocked", "root")}
        before = copy.deepcopy(tasks)
        args = (tasks, {}, {}, {"blocked"}, False)
        self.assertEqual([("root", 0, True, True), ("child", 1, False, False)], view.project(*args, set(), 7, 100, WALL))
        self.assertEqual([("root", 0, True, True)], view.project(*args, {"root"}, 7, 100, WALL))
        self.assertEqual(before, tasks)

    def test_default_attention_includes_positive_unavailable_not_query_failure(self):
        tasks = {"root": task()}
        self.assertEqual(1, len(view.project(tasks, {}, {}, view.DEFAULT, True, set(), 7, 100, WALL)))
        self.assertEqual([], view.project(tasks, None, {}, view.DEFAULT, True, set(), 7, 100, WALL))

    def test_attention_filter_round_trip_restores_unavailable_rows(self):
        selected = set(view.DEFAULT)
        selected.symmetric_difference_update({"stalled"})
        self.assertFalse(view.attention_filters(selected))
        selected.symmetric_difference_update({"stalled"})
        self.assertTrue(view.attention_filters(selected))
        selected.add("old")
        self.assertTrue(view.attention_filters(selected))
        self.assertEqual(1, len(view.project({"root": task()}, {}, {}, selected, view.attention_filters(selected), set(), 7, 100, WALL)))

    def test_verified_owner_prompt_is_blocked_without_changing_task_debt(self):
        current = task()
        before = copy.deepcopy(current)
        self.assertIn("blocked", view.categories(current, set(), {"term_a": agent(status="blocked")}, {}, 100))
        self.assertNotIn("blocked", view.categories(current, set(), {"term_a": agent(status="blocked", execution="replacement")}, {}, 100))
        self.assertNotIn("blocked", view.categories(current, set(), None, {}, 100))
        self.assertEqual(before, current)
        self.assertEqual({"done/accepted"}, view.categories(task(state="done"), set(), {"term_a": agent(status="blocked")}, {}, 100))

    def test_age_never_hides_unfinished_debt_or_needed_ancestor(self):
        old = "2020-01-01T00:00:00+00:00"
        self.assertFalse(view.old_final(task(updated_at=old), 7, WALL))
        self.assertTrue(view.old_final(task(state="done", updated_at=old), 7, WALL))
        tasks = {"root": task(state="done", updated_at=old), "child": task("child", state="blocked", parent="root")}
        rows = view.project(tasks, {}, {}, {"blocked", "done/accepted"}, False, set(), 7, 100, WALL)
        self.assertEqual([("root", 0, True, True), ("child", 1, False, False)], rows)
        self.assertFalse(view.project(tasks, {}, {}, {"done/accepted", "old"}, False, set(), 7, 100, WALL)[0][2])

    def test_maximum_depth_is_iterative_and_linear_ancestor_context(self):
        tasks = {str(i): task(str(i), state="blocked" if i == 9999 else "open", parent=str(i-1) if i else None) for i in range(10000)}
        rows = view.project(tasks, {}, {}, {"blocked"}, False, set(), 7, 100, WALL)
        self.assertEqual(10000, len(rows))
        self.assertEqual(("9999", 9999, False, False), rows[-1])

    def test_all_tasks_shows_unknown_but_never_fabricates_live_owner(self):
        tasks = {"root": task()}
        self.assertEqual([("root", 0, False, False)], view.project(tasks, None, {}, view.DEFAULT, False, set(), 7, 100, WALL, all_tasks=True))
        self.assertEqual((None, {}), view.live_facts({"agents": {"term_a": agent()}, "idle_since": {A["execution"]: 0}, "observed_at": 100, "error": None}, 106))
        model = {"agents": {"term_a": agent()}, "idle_since": {A["execution"]: 0}, "observed_at": 100, "error": None}
        self.assertEqual(model["agents"], view.live_facts(model, 105)[0])
        model["error"] = "provider missing"
        self.assertEqual((None, {}), view.live_facts(model, 101))

    def test_unseen_agent_done_is_idle_not_durable_task_done(self):
        self.assertEqual("idle", view.activity(agent(status="done")))
        self.assertEqual({"idle"}, view.categories(task(), set(), {"term_a": agent(status="done")}, {}, 100))
        self.assertNotIn("done/accepted", view.categories(task(), set(), {"term_a": agent(status="done")}, {}, 100))

    def test_control_and_bidi_text_cannot_emit_terminal_controls(self):
        self.assertEqual("x [31m y z", view.clean("x\x1b[31m\ny\u202ez"))
        self.assertEqual("2026-10-01 01:02:03Z", view.since_label(STAMP))
        self.assertEqual("—", view.since_label(None))
        self.assertEqual("你好", view.clip("你好🙂", 4))
        self.assertEqual("e\u0301", view.clip("e\u0301x", 1))


class ReaderTests(unittest.TestCase):
    def test_revision_skips_pages_and_state_sequence_or_gap_resets_idle(self):
        reader = view.Reader()
        calls = []
        def read(action, **fields):
            calls.append(action)
            return {"revision": 1} if action == "status" else {"tasks": [task()], "next_cursor": None}
        with mock.patch.object(view, "task_read", side_effect=read), mock.patch.object(view, "api", return_value={"agents": [agent()]}), mock.patch.object(view.time, "monotonic", side_effect=[100, 102, 104, 110]):
            self.assertEqual(100, reader.refresh()["idle_since"][A["execution"]])
            self.assertEqual(100, reader.refresh()["idle_since"][A["execution"]])
            with mock.patch.object(view, "api", return_value={"agents": [agent(state_change_seq=2)]}):
                self.assertEqual(104, reader.refresh()["idle_since"][A["execution"]])
                self.assertEqual(110, reader.refresh()["idle_since"][A["execution"]])
        self.assertEqual(1, calls.count("list"))

    def test_merged_provider_status_list_refresh_parent_only_acceptor_change(self):
        # Real current provider/SQLite reads; only neutral agent facts are mocked.
        with tempfile.TemporaryDirectory() as directory:
            store = provider.Store(directory)
            self.addCleanup(store.db.close)
            operations = []
            reads = []
            def mutate(caller, action, **fields):
                operations.append(action)
                return store.invoke({"caller": caller, "payload": {"action": action, "operation_id": str(len(operations)), **fields}})["task"]
            def read(action, **fields):
                reads.append(action)
                return store.invoke({"caller": {"class": "unknown"}, "payload": {"action": action, **fields}})
            self.assertEqual({"revision": 0}, read("status"))
            parent = mutate(H, "create", title="parent", done_when="Proof recorded.")
            parent = mutate(H, "delegate", id=parent["id"], revision=parent["revision"], owner=A)
            child = mutate(A, "create", title="child", parent=parent["id"], done_when="Proof recorded.")
            child = mutate(A, "delegate", id=child["id"], revision=child["revision"], owner=B)
            child = mutate(B, "start", id=child["id"], revision=child["revision"])
            child = mutate(B, "report", id=child["id"], revision=child["revision"], text="> Proof recorded.\nLocal evidence.")
            reader = view.Reader()
            with mock.patch.object(view, "task_read", side_effect=read), mock.patch.object(view, "api", return_value={"agents": [agent(), agent(B)]}):
                before = reader.refresh()
                first = before["tasks"][child["id"]]
                reader.refresh()
                self.assertEqual(1, reads.count("list"))
                mutate(H, "delegate", id=parent["id"], revision=parent["revision"], owner=H)
                after = reader.refresh()
                second = after["tasks"][child["id"]]
            self.assertGreater(after["revision"], before["revision"])
            self.assertEqual(first["revision"], second["revision"])
            self.assertEqual(first["updated_at"], second["updated_at"])
            self.assertEqual([A], first["owed_to"]["bindings"])
            self.assertEqual([A, H], second["owed_to"]["bindings"])
            self.assertNotIn("waiting on me", view.categories(first, set(), {}, {}, 100))
            self.assertIn("waiting on me", view.categories(second, set(), {}, {}, 100))
            self.assertEqual(2, reads.count("list"))
            self.assertEqual(after["revision"], read("status")["revision"])

    def test_changed_revision_retains_previous_task_snapshot(self):
        reader = view.Reader()
        reader.tasks, reader.revision = {"previous": task("previous")}, 1
        with mock.patch.object(view, "task_read", side_effect=[{"revision": 2}, {"tasks": [task()], "next_cursor": None}, {"revision": 3}]):
            with self.assertRaisesRegex(RuntimeError, "changed during read"):
                reader.refresh()
        self.assertEqual(["previous"], list(reader.tasks))
        self.assertEqual(1, reader.revision)


if __name__ == "__main__":
    unittest.main()
