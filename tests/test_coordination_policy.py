"""Pure policy regressions: no core, provider, model or real user state."""
import copy
import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / "extensions"))
from coordination_policy import (Policy, PolicyError, QuotaLimits, NAMES, MEMBERS,
                                 GRANTS, OWNER, FairQueue, check_admission,
                                 bounded_page, encoded, record_charge)


def entry(value, version=1):
    return {"version": version, "value": value}


def resource(rid, principal=None, **extra):
    result = {"id": rid, "kind": "agent", "data": {"agent": {"lifecycle": "live"}}, **extra}
    if principal:
        result["principal"] = principal
    return result


def grant(who, can, on="*", until_ms=None):
    return {"who": who, "can": can, "on": on, "until_ms": until_ms}


def mail(delivery="d:1", sender="p:a", target="r1", state="held", text="hello", **extra):
    return {"delivery": delivery, "from": {"principal": sender}, "to": {"agent": target},
            "state": state, "content": [{"type": "text", "text": text}], **extra}


class Fixture(unittest.TestCase):
    def setUp(self):
        self.entries = {OWNER: entry("p:owner"),
                        MEMBERS + "r1": entry({"project": "p", "reports_to": None}),
                        MEMBERS + "r2": entry({"project": "p", "reports_to": "r1"}),
                        MEMBERS + "r3": entry({"project": "p", "reports_to": "r2"}),
                        MEMBERS + "r4": entry({"project": "q", "reports_to": None}),
                        NAMES + "@lead": entry({"agent": "r1", "principal": "p:a"}),
                        NAMES + "@worker": entry({"agent": "r2", "principal": "p:b"})}
        self.resources = {"r" + str(i): resource("r" + str(i), "p:" + chr(96 + i)) for i in range(1, 5)}

    def policy(self, **kwargs):
        return Policy(self.entries, self.resources, now_ms=1000, **kwargs)

    def rejects(self, kind, fn, *args, **kwargs):
        with self.assertRaises(PolicyError) as caught:
            fn(*args, **kwargs)
        self.assertEqual(caught.exception.kind, kind, str(caught.exception))


class IdentityAndScopes(Fixture):
    def test_handles_resolve_exactly_and_no_name_implicitly_grants(self):
        p = self.policy()
        self.assertEqual(p.resolve("@worker"), "r2")
        self.assertEqual(p.resolve("agent:r2"), "r2")
        self.assertFalse(p.has("p:b", "resume", "@worker"))
        self.assertTrue(p.has("p:owner", "resume", "r2"))
        self.rejects("not_found", p.resolve, "@absent")
        self.rejects("invalid", p.resolve, "r0")

    def test_engine_api_aliases_and_tombstone_resolution(self):
        p = self.policy(tombstones={"r9": {"state": "retired"}, "r10": {"state": "unavailable"}})
        self.assertEqual(p.resolve_target("@worker"), "r2")
        self.assertTrue(p.allowed("p:owner", "pause", "r2"))
        self.rejects("retired", p.resolve_target, "r9")
        self.assertEqual(p.resolve_target("r10"), "r10")
        p = self.policy(tombstones={"r2": {"lifecycle": "retired"}})
        self.rejects("retired", p.resolve_target, "@worker")
        self.assertEqual(len(p.names_ops("p:owner", "@new", "r4", expect=0)), 1)
        self.assertEqual(len(p.membership_ops("p:owner", "r4", "q", None, expect=1)), 1)
        self.assertEqual(len(p.grant_ops("p:owner", "x", grant("p:b", ["resume"]), expect=0)), 1)

    def test_snapshot_does_not_change_when_caller_mutates_source(self):
        p = self.policy()
        self.entries[NAMES + "@worker"]["value"]["agent"] = "r4"
        self.resources["r2"]["state"] = "finished"
        self.assertEqual(p.resolve("@worker"), "r2")

    def test_ended_retired_and_unavailable_are_not_conflated(self):
        self.resources["r2"]["data"]["agent"]["lifecycle"] = "ended"
        self.rejects("ended", self.policy().resolve, "@worker")
        self.resources["r2"]["data"]["agent"]["lifecycle"] = "retired"
        self.rejects("retired", self.policy().resolve, "@worker")
        self.resources["r2"]["data"]["agent"]["lifecycle"] = "disconnected"
        self.assertEqual(self.policy().resolve("@worker"), "r2")

    def test_scope_contains_absent_members_and_orders_parents_before_children(self):
        del self.resources["r3"]
        p = self.policy()
        self.assertEqual(p.scope("team:r1"), ["r1", "r2", "r3"])
        self.assertEqual(p.scope("project:q"), ["r4"])
        self.assertEqual(p.scope("workspace"), ["r1", "r2", "r3", "r4"])

    def test_deep_team_does_not_recurse_and_200_members_fit_one_scope(self):
        self.entries = {OWNER: entry("p:owner")}
        self.resources = {}
        for i in range(1, 1001):
            self.entries[MEMBERS + "r" + str(i)] = entry({"project": "p", "reports_to": "r" + str(i-1) if i > 1 else None})
        p = self.policy()
        result = p.scope("team:r1")
        self.assertEqual(len(result), 1000)
        self.assertEqual(result[-1], "r1000")
        p = self.policy(limits=QuotaLimits(scope_capacity=200))
        self.rejects("quota", p.scope, "team:r1")

    def test_explicit_scope_bound_does_not_narrow_individual_grant_checks(self):
        for i in range(5, 206):
            self.entries[MEMBERS + "r" + str(i)] = entry({"project": "p", "reports_to": "r1"})
        self.entries[GRANTS + "large"] = entry(grant("p:b", ["resume"], "team:r1"))
        p = self.policy(limits=QuotaLimits(scope_capacity=200))
        self.rejects("quota", p.scope, "team:r1", limit=200)
        self.assertTrue(p.has("p:b", "resume", "r205"))
        self.rejects("quota", p.scope, "workspace", limit=16)

    def test_cycles_and_cross_project_parent_are_invalid(self):
        self.entries[MEMBERS + "r1"]["value"]["reports_to"] = "r3"
        self.rejects("invalid", self.policy)
        self.entries[MEMBERS + "r1"]["value"]["reports_to"] = "r4"
        self.rejects("invalid", self.policy)

    def test_membership_revision_changes_with_versions_not_names(self):
        old = self.policy().membership_revision
        self.entries[NAMES + "@lead"]["version"] = 2
        self.assertEqual(self.policy().membership_revision, old)
        self.entries[MEMBERS + "r1"]["version"] = 2
        self.assertNotEqual(self.policy().membership_revision, old)


class Grants(Fixture):
    def test_additive_union_expiry_and_no_undeclared_wildcard(self):
        self.entries[GRANTS + "a"] = entry(grant("p:b", ["resume"], "agent:r2", 1001))
        self.entries[GRANTS + "b"] = entry(grant("p:b", ["send:*"], "agent:r2"))
        self.entries[GRANTS + "wild"] = entry(grant("*", ["answer"]))
        p = self.policy()
        self.assertTrue(p.has("p:b", "resume", "r2"))
        self.assertTrue(p.has("p:b", "send:preempt", "r2"))
        self.assertFalse(p.has("p:b", "resume", "r3"))
        self.assertFalse(p.has("undeclared", "answer", "r2"))
        self.entries[GRANTS + "a"]["value"]["until_ms"] = 1000
        self.assertFalse(self.policy().has("p:b", "resume", "r2"))

    def test_team_and_project_subjects_use_explicit_endpoint_principals(self):
        self.entries[GRANTS + "team"] = entry(grant("team:r1", ["receive"], "project:q"))
        self.entries[GRANTS + "project"] = entry(grant("project:q", ["answer"], "team:r1"))
        p = self.policy()
        self.assertTrue(p.has("p:c", "receive", "r4"))
        self.assertTrue(p.has("p:d", "answer", "r3"))
        self.assertFalse(p.has("host:r1", "receive", "r4"))
        self.assertFalse(p.has("p:a", "answer", "r3"))

    def test_name_subject_requires_bound_principal_not_similar_label(self):
        self.entries[GRANTS + "name"] = entry(grant("@worker", ["answer"], "agent:r3"))
        p = self.policy()
        self.assertTrue(p.has("p:b", "answer", "r3"))
        self.assertFalse(p.has("@worker", "answer", "r3"))
        self.assertFalse(p.has("p:c", "answer", "r3"))

    def test_public_grant_aliases_are_pinned_once_not_transferred_on_rebind(self):
        p = self.policy()
        ops = p.grant_ops("p:owner", "pin", grant("@worker", ["answer"], "@worker"), expect=0)
        self.assertEqual(ops[0]["value"]["who"], "p:b")
        self.assertEqual(ops[0]["value"]["on"], "agent:r2")
        self.entries[GRANTS + "pin"] = entry(ops[0]["value"])
        self.entries[NAMES + "@worker"] = entry({"agent": "r4", "principal": "p:d"}, 2)
        after = self.policy()
        self.assertTrue(after.has("p:b", "answer", "r2"))
        self.assertFalse(after.has("p:d", "answer", "r4"))

    def test_ambiguous_alias_principals_fail_closed(self):
        self.entries[NAMES + "@worker"]["value"]["principal"] = "p:other"
        self.entries[GRANTS + "raw"] = entry(grant("@worker", ["answer"]))
        p = self.policy()
        self.assertFalse(p.has("p:b", "answer", "r2"))
        self.assertFalse(p.has("p:other", "answer", "r2"))
        self.rejects("invalid", p.grant_ops, "p:owner", "new", grant("@worker", ["answer"]), expect=0)

    def test_selected_strategy_and_each_step_can_be_checked_separately(self):
        self.entries[GRANTS + "send"] = entry(grant("p:a", ["send:start"], "agent:r2"))
        p = self.policy()
        self.assertTrue(p.has("p:a", "send:start", "r2"))
        for right in ("send:preempt", "send:interrupt_then_start", "queue", "interrupt"):
            self.rejects("no_grant", p.require, "p:a", right, "r2")

    def test_manager_cannot_expand_scope_or_rights(self):
        self.entries[GRANTS + "manager"] = entry(grant("p:a", ["grants.manage", "resume"], "agent:r2"))
        p = self.policy()
        valid = {"id": "new", "value": grant("p:b", ["resume"], "agent:r2"), "expect": 0}
        self.assertEqual(len(p.config_ops("coord.grants", valid, "p:a")), 1)
        for on in ("*", "agent:r3", "team:r2", "project:p"):
            args = copy.deepcopy(valid)
            args["value"]["on"] = on
            self.rejects("no_grant", p.config_ops, "coord.grants", args, "p:a")
        valid["value"]["can"] = ["answer"]
        self.rejects("no_grant", p.config_ops, "coord.grants", valid, "p:a")

    def test_grant_may_not_outlive_issuer_standing(self):
        self.entries[GRANTS + "manager"] = entry(grant("p:a", ["grants.manage", "resume"], "team:r1", 2000))
        p = self.policy()
        args = {"id": "new", "value": grant("p:b", ["resume"], "agent:r2", 2000), "expect": 0}
        self.assertEqual(len(p.config_ops("coord.grants", args, "p:a")), 1)
        for expiry in (None, 2001):
            args["value"]["until_ms"] = expiry
            self.rejects("no_grant", p.config_ops, "coord.grants", args, "p:a")

    def test_replacing_and_deleting_need_rights_on_existing_scope(self):
        self.entries[GRANTS + "other"] = entry(grant("p:d", ["answer"], "agent:r4"), 3)
        self.entries[GRANTS + "manager"] = entry(grant("p:a", ["grants.manage", "resume"], "agent:r2"))
        p = self.policy()
        self.rejects("no_grant", p.config_ops, "coord.grants", {"id": "other", "value": None, "expect": 3}, "p:a")
        result = p.config_ops("coord.grants", {"id": "other", "value": None, "expect": 3}, "p:owner")
        self.assertEqual(result, [{"op": "delete", "key": GRANTS + "other", "expect": 3}])


class Configuration(Fixture):
    def test_handle_limit_and_exact_cas(self):
        for i in range(3):
            self.entries[NAMES + "@w" + str(i)] = entry({"agent": "r2"})
        p = self.policy()
        self.rejects("quota", p.config_ops, "coord.names", {"handle": "@fifth", "agent": "r2", "expect": 0}, "p:owner")
        result = p.config_ops("coord.names", {"handle": "@worker", "agent": "r2", "expect": 1}, "p:owner")
        self.assertEqual(result[0]["expect"], 1)
        self.rejects("conflict", p.config_ops, "coord.names", {"handle": "@worker", "agent": "r2", "expect": 0}, "p:owner")

    def test_name_rebinding_requires_both_source_and_destination_rights(self):
        self.entries[GRANTS + "names"] = entry(grant("p:b", ["names"], "agent:r2"))
        self.rejects("no_grant", self.policy().config_ops, "coord.names", {"handle": "@worker", "agent": "r4", "expect": 1}, "p:b")

    def test_invalid_handles_never_create_ops(self):
        for handle in ("worker", "@Upper", "@a/b/c/d", "@-x", "@" + "x" * 33, "@x/"):
            self.rejects("invalid", self.policy().config_ops, "coord.names", {"handle": handle, "agent": "r2", "expect": 0}, "p:owner")

    def test_move_changes_subtree_project_atomically_and_cas_each_changed_row(self):
        p = self.policy()
        ops = p.config_ops("coord.membership.set", {"agent": "r2", "project": "q", "reports_to": "r4", "expect": 1}, "p:owner")
        self.assertEqual({op["key"] for op in ops}, {MEMBERS + "r2", MEMBERS + "r3"})
        self.assertTrue(all(op["expect"] == 1 and op["value"]["project"] == "q" for op in ops))
        self.assertEqual(self.entries[MEMBERS + "r3"]["value"]["project"], "p")

    def test_move_cycle_missing_parent_and_stale_selection_refuse_whole(self):
        p = self.policy()
        args = {"agent": "r1", "project": "p", "reports_to": "r3", "expect": 1}
        self.rejects("invalid", p.config_ops, "coord.membership.set", args, "p:owner")
        args["reports_to"] = "r99"
        self.rejects("invalid", p.config_ops, "coord.membership.set", args, "p:owner")
        args["reports_to"] = None
        args["membership_revision"] = "old"
        self.rejects("conflict", p.config_ops, "coord.membership.set", args, "p:owner")

    def test_ordinary_membership_changes_need_no_operational_capacity(self):
        p = self.policy(limits=QuotaLimits(domain_bytes=1, domain_records=1))
        ops = p.config_ops("coord.membership.set", {"agent": "r2", "project": "q", "reports_to": "r4", "expect": 1}, "p:owner")
        self.assertEqual(len(ops), 2)

    def test_worker_no_rights_cannot_self_resume_move_or_issue_grants(self):
        p = self.policy()
        self.rejects("no_grant", p.require, "p:b", "resume", "r2")
        self.rejects("no_grant", p.config_ops, "coord.membership.set", {"agent": "r2", "project": "q", "reports_to": "r4", "expect": 1}, "p:b")
        self.rejects("no_grant", p.config_ops, "coord.grants", {"id": "x", "value": grant("p:b", ["resume"]), "expect": 0}, "p:b")


class Admission(Fixture):
    def test_finite_default_and_explicit_expiry_never_renew(self):
        p = self.policy()
        args = {"mode": "boundary", "content": [{"type": "text", "text": "go"}]}
        result = p.validate_send(args)
        self.assertEqual(result["expires_at_ms"], 301000)
        self.assertNotIn("expires_at_ms", args)
        args["expires_at_ms"] = 1002
        self.assertEqual(p.validate_send(args)["expires_at_ms"], 1002)
        args["expires_at_ms"] = 1000
        self.rejects("expired", p.validate_send, args)

    def test_follow_current_cannot_weaken_explicit_session_guard(self):
        args = {"mode": "start", "content": [{"type": "text", "text": "go"}],
                "follow_current_session": True, "guard": {"session": "s1"}}
        self.rejects("invalid", self.policy().validate_send, args)

    def test_content_bytes_are_utf8_and_oversize_rejected_not_truncated(self):
        p = self.policy()
        args = {"mode": "start", "content": [{"type": "text", "text": "😀" * 9000}]}
        self.rejects("quota", p.validate_send, args)
        self.assertEqual(len(args["content"][0]["text"]), 9000)
        self.rejects("invalid", p.validate_send, {"mode": "start", "content": "go"})
        self.rejects("unsupported", p.validate_send, {"mode": "start", "content": [{"type": "mystery"}]})
        self.rejects("invalid", p.validate_send, {"mode": "start", "content": [{"type": "text", "text": float("nan")}]})

    def test_sender_target_count_includes_send_new_and_unknown_evidence(self):
        p = self.policy()
        records = [mail("d:" + str(i), state="unknown" if i % 2 else "held") for i in range(8)]
        self.rejects("quota", p.check_quota, records, mail("d:new", send_new=True))
        records[0]["state"] = "accepted"
        p.check_quota(records, mail("d:new"))

    def test_other_sender_target_and_aggregate_caps_are_independent(self):
        limits = QuotaLimits(sender_target_records=2, sender_records=3, target_records=3)
        p = self.policy(limits=limits)
        records = [mail("d:1", target="r1"), mail("d:2", target="r2"), mail("d:3", target="r3")]
        self.rejects("quota", p.check_quota, records, mail("d:4", target="r4"))
        records = [mail("d:1", sender="p:a"), mail("d:2", sender="p:b"), mail("d:3", sender="p:c")]
        self.rejects("quota", p.check_quota, records, mail("d:4", sender="p:d"))

    def test_reserve_accounts_gates_tombstones_and_outcome_before_admission(self):
        p = self.policy()
        candidate = mail()
        charge = record_charge(candidate)
        p.check_quota([], candidate, reserved_bytes=p.limits.normal_bytes - charge)
        self.rejects("quota", p.check_quota, [], candidate, reserved_bytes=p.limits.normal_bytes - charge + 1)
        self.rejects("quota", p.check_quota, [], candidate, reserved_records=p.limits.normal_records)
        self.assertGreaterEqual(charge, len(encoded(candidate["content"])) + 3072)

    def test_recipe_reserves_one_compact_outcome_per_step(self):
        content = [{"type": "text", "text": "recipe"}]
        one = check_admission([], "p:a", "r1", content)
        three = check_admission([], "p:a", "r1", content, outcome_steps=3)
        self.assertEqual(three["bytes"] - one["bytes"], 2048)
        self.assertEqual(record_charge(mail(outcome_steps=3)) - record_charge(mail()), 2048)
        for steps in (0, 4, True, 1.5):
            self.rejects("invalid", check_admission, [], "p:a", "r1", content, outcome_steps=steps)
        limits = QuotaLimits(domain_bytes=6000)
        self.rejects("quota", check_admission, [], "p:a", "r1", content,
                     outcome_steps=3, reserved_bytes=1000, limits=limits)

    def test_actual_metadata_above_floor_still_reserves_each_future_outcome(self):
        record = mail(outcome_steps=3, derived_attribution="x" * 5000)
        self.assertGreater(len(encoded(record)), len(encoded(record["content"])) + 2048)
        self.assertEqual(record_charge(record), len(encoded(record)) + 3 * 1024)
        record["outcome_steps"] = 1
        self.assertEqual(record_charge(record), len(encoded(record)) + 1024)

    def test_admission_total_occupancy_does_not_double_charge_records(self):
        rows = [mail()]
        limits = QuotaLimits(domain_bytes=10000)
        content = [{"type": "text", "text": "new"}]
        # Both open records reserve ~3KiB each. Current total includes old mail.
        check_admission(rows, "p:b", "r1", content, reserved_bytes=len(encoded(rows[0])) + 100,
                        reserved_records=2, limits=limits)
        self.rejects("quota", check_admission, rows, "p:b", "r1", content,
                     reserved_bytes=8500, reserved_records=2, limits=limits)

    def test_supersession_only_excludes_own_editable_same_target_record(self):
        p = self.policy(limits=QuotaLimits(sender_target_records=1))
        p.check_quota([mail()], mail("d:new", supersedes="d:1"))
        for old in (mail(sender="p:other"), mail(target="r2"), mail(state="claimed"), mail(state="unknown")):
            self.rejects("conflict", p.check_quota, [old], mail("d:new", supersedes="d:1"))


class Fairness(Fixture):
    def test_flood_does_not_starve_another_sender_and_sender_order_is_stable(self):
        queue = FairQueue()
        rows = [mail("a:" + str(i), mode="boundary", order=i) for i in range(5)]
        rows += [mail("b", sender="p:b", mode="boundary", order=100)]
        first = queue.pick(rows)
        self.assertEqual(first["delivery"], "a:0")
        rows.remove(first)
        self.assertEqual(queue.pick(rows)["delivery"], "b")
        rows.pop()
        self.assertEqual(queue.pick(rows)["delivery"], "a:1")
        self.assertEqual(rows[0]["state"], "held", "selector must not claim")

    def test_held_start_does_not_block_urgent_lanes_and_targets_alternate(self):
        queue = FairQueue()
        rows = [mail("start", mode="start", order=1),
                mail("b1", mode="boundary", order=2),
                mail("b2", target="r2", mode="boundary", order=3)]
        first = queue.pick(rows)
        self.assertEqual(first["delivery"], "b1")
        self.assertEqual(queue.pick(rows)["delivery"], "b2")
        self.assertIsNone(queue.pick([]))
        for mode in ("preempt", "admit"):
            self.assertEqual(FairQueue().pick([rows[0], mail(mode, mode=mode, order=5)])["mode"], mode)

    def test_pure_admission_api_reports_reservation_without_writing(self):
        rows = [mail()]
        before = copy.deepcopy(rows)
        result = check_admission(rows, "p:b", "r1", [{"type": "text", "text": "new"}])
        self.assertEqual(result["records"], 1)
        self.assertGreater(result["bytes"], 3072)
        self.assertEqual(rows, before)


class Pagination(Fixture):
    def test_bounded_pages_return_each_id_once_and_do_not_consume(self):
        rows = [mail("d:" + str(i).zfill(3)) for i in range(200)]
        before = copy.deepcopy(rows)
        output, cursor = [], None
        while True:
            result = bounded_page(rows, cursor)
            self.assertLessEqual(len(result["items"]), 64)
            self.assertLessEqual(len(encoded(result)), 256 * 1024)
            output.extend(row["delivery"] for row in result["items"])
            cursor = result["next"]
            if cursor is None:
                break
        self.assertEqual(output, [row["delivery"] for row in rows])
        self.assertEqual(rows, before)

    def test_order_delivery_tuple_cursor_keeps_numeric_order(self):
        rows = [mail("d:z", order=2), mail("d:a", order=10), mail("d:b", order=2)]
        key = lambda row: (row["order"], row["delivery"])
        first = bounded_page(rows, limit=1, key=key)
        self.assertEqual(first["items"][0]["delivery"], "d:b")
        second = bounded_page(rows, first["next"], limit=1, key=key)
        third = bounded_page(rows, second["next"], limit=1, key=key)
        self.assertEqual(second["items"][0]["delivery"], "d:z")
        self.assertEqual(third["items"][0]["delivery"], "d:a")
        self.assertIsNone(third["next"])

    def test_changed_cut_malformed_cursor_and_duplicate_ids_fail_closed(self):
        rows = [mail("d:1"), mail("d:2")]
        page = bounded_page(rows, limit=1)["next"]
        rows[1]["state"] = "unknown"
        self.rejects("conflict", bounded_page, rows, page)
        self.rejects("invalid", bounded_page, rows, "not-base64")
        self.rejects("invalid", bounded_page, [mail(), mail()])
        self.rejects("invalid", bounded_page, rows, limit=65)

    def test_byte_bound_includes_envelope_and_cursor(self):
        rows = [mail("d:" + str(i), text="x" * 300) for i in range(10)]
        result = bounded_page(rows, byte_limit=1100)
        self.assertLessEqual(len(encoded(result)), 1100)
        self.assertIsNotNone(result["next"])
        self.assertLess(len(result["items"]), 10)
        self.rejects("too_large", bounded_page, rows, byte_limit=100)


if __name__ == "__main__":
    unittest.main()
