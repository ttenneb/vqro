# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Public-observation checker controls with response doubles, not runtime grades."""
from copy import deepcopy
import hashlib
import json
from pathlib import Path
import shutil
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import AsyncMock, call

from agent_conformance.part_b.context import PublicCoordination
from agent_conformance.part_b.identity_request_cases import mail_matches_request
from agent_conformance.part_b.request_routing_binding import (
    NOTICE_PREFIX, PublicRequestRouting, native_notice, route_key, waiting_rows,
)
from agent_conformance.results import CheckFailed, FixtureFailed, HarnessGap


def question():
    return {"request": "q-current", "revision": 3, "scope": {"session": "s1", "run": "run1"},
        "title": "Owned native confirmation: q-current", "method": "confirm", "params": {},
        "status": "pending", "expires_at_ms": 9999999999999}


def payload():
    q = question()
    return {"agent": "r1", "epoch": "e7", "request": q["request"], "revision": q["revision"],
        "scope": deepcopy(q["scope"]), "expires_at_ms": q["expires_at_ms"], "question": q}


def content(value):
    return [{"type": "text", "text": NOTICE_PREFIX + json.dumps(value)}]


class Context:
    check = PublicCoordination.check

    def __init__(self, *, compact=False, fallback=False):
        self.manifest = {"coord_revision": "a" * 40, "request_routing": {"coord_revision": "a" * 40}}
        self.evidence, self.assertions = [], 0
        self.entry = {"version": 7, "value": {"route": "invalid-owned-probe" if fallback else "both"}}
        self.q = question()
        self.projection = {"agent": "r1", "key": route_key("r1"), "version": 7, "human": True,
            "coordinator": None if fallback else "r2", "fallback_reason": "invalid_request_route" if fallback else None,
            "request_keys": ["source-current"], "notices": [] if fallback else [{"source_key": "source-current",
                "request": self.q["request"], "revision": self.q["revision"], "delivery": "d1", "state": "held"}]}
        self.panel = {"items": [{"children": [{"id": "waiting", "text": self.q["title"],
            "command": "agents.answer", "args": {"resource": "r1"},
            "description": "Invalid request route; human fallback · Enter: answer…" if fallback else ""}]}]}
        self.records = [] if fallback else [{"delivery": "d1", "state": "held", "attempt": None,
            "to": {"agent": "r2", "incarnation": "i:a1"}, "request_notice": {"agent": "r1", "key": "source-current"},
            "from": {"principal": "extension:coord", "actor": "extension:coord", "author": "client:managed"},
            "content": content(payload())}]
        self.calls = []
        if compact:
            record = self.records[0]
            claim = {**deepcopy(record), "state": "claimed", "to": {"agent": "r2", "incarnation": "i:a1"}}
            self.calls = [{"op": "agent.send", "ref": "a1", "args": {"delivery": "d1", "mode": "admit", "content": content(payload())},
                "witness": {"entry": {"value": claim}}}]
            record.update(state="accepted", attempt={"id": "attempt"})
            record.pop("content")
        self.more = None
        self.peer = SimpleNamespace(call=self.rpc)
        self.native = SimpleNamespace(records=lambda kind: self.calls if kind == "invoke" else [])

    async def rpc(self, method, params=None, **_):
        if method == "command.invoke":
            assert params == {"name": "agents.show", "args": {"resource": "r1"}}
            return {"client": {"focus": "agents-r1"}}
        if method == "workspace.get":
            assert params == {"key": route_key("r1")}
            return deepcopy(self.entry)
        if method == "workspace.snapshot":
            return {"live": {"coord-requests-r1": deepcopy(self.projection), "agents-r1": deepcopy(self.panel)}}
        if method == "resource.list":
            return {"resources": [{"id": "r1", "data": {"agent": {"epoch": "e7", "requests": [deepcopy(self.q)]}}},
                {"id": "r2", "backend_ref": "a1", "incarnation": "i:a1"}]}
        if method == "operational.get":
            return {"entry": {"value": deepcopy(self.records[0])}}
        raise AssertionError((method, params))

    async def command(self, name, params):
        if name == "request_route":
            assert params == {"action": "inspect", "agent": "r1"}
            configured = self.entry["value"]["route"] if self.entry else None
            return {"key": route_key("r1"), "agent": "r1", "version": self.entry["version"] if self.entry else 0,
                "configured": configured, "route": configured if self.projection.get("coordinator") else "human",
                "human": self.projection["human"], "coordinator": self.projection.get("coordinator"),
                "fallback_reason": self.projection.get("fallback_reason")}
        assert name == "inbox.list" and params == {}, "do not filter away unrelated receivers"
        return {"items": deepcopy(self.records), "next": self.more}

    async def wait(self, predicate, **_):
        result = await predicate()
        if not result: raise CheckFailed("no positive current observation")
        return result


class PayloadControls(unittest.TestCase):
    def test_exact_key_no_alias_and_real_payload_parser(self):
        self.assertEqual(route_key("r17"), "doc/agents/request-routes/r17")
        for invalid in (None, "", "r1/alias", "r1\0"):
            with self.subTest(value=invalid), self.assertRaises(FixtureFailed): route_key(invalid)
        self.assertEqual(native_notice(content(payload())), payload())
        self.assertIsNone(native_notice([{ "type": "text", "text": json.dumps(payload())}]))
        self.assertIsNone(native_notice([{ "type": "text", "text": NOTICE_PREFIX + "not JSON"}]))
        self.assertIsNone(native_notice(content(payload()) * 2))

    def test_missing_empty_malformed_or_mismatched_pins_fail_before_effects(self):
        pairs = [(None, None), ("", ""), (True, 1), ([], []), ({}, {}), ("a" * 39, "a" * 39),
            ("g" * 40, "g" * 40), ("A" * 40, "A" * 40), ("a" * 40, None), ("a" * 40, "b" * 40)]
        for target, pinned in pairs:
            with self.subTest(target=target, pinned=pinned):
                ctx = SimpleNamespace(manifest={"coord_revision": target, "request_routing": {"coord_revision": pinned}})
                with self.assertRaises(HarnessGap): PublicRequestRouting(ctx)
        for absent in ({}, {"coord_revision": "a" * 40}, {"coord_revision": "a" * 40, "request_routing": None}):
            with self.subTest(manifest=absent), self.assertRaises(HarnessGap):
                PublicRequestRouting(SimpleNamespace(manifest=absent))
        self.assertIsNotNone(PublicRequestRouting(Context()))

    def test_recursive_waiting_row_is_actual_panel_data(self):
        self.assertEqual(waiting_rows({"items": [{"children": [{"id": "waiting", "text": "native question"}]}]})[0]["text"], "native question")
        self.assertEqual(waiting_rows({"items": [{"id": "other"}]}), [])


class SetupControls(unittest.IsolatedAsyncioTestCase):
    async def test_real_tabs_manifest_and_command_are_ready_before_agents_show(self):
        # Filesystem/restart/activation doubles only, not a target startup grade.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            ctx = Context(); ctx.root = root / "case"
            (ctx.root / "ext/manifests").mkdir(parents=True)
            ctx.backend = SimpleNamespace(env={})
            descriptors = {}
            for name in ("agents", "pi", "tabs"):
                source = root / (name + ".json")
                source.write_text(json.dumps({"name": name}))
                descriptors[name] = {"path": str(source), "sha256": hashlib.sha256(source.read_bytes()).hexdigest()}
            ctx.manifest["request_routing"].update(runtime_parent=str(root), manifests=descriptors)
            async def restarted(**kwargs):
                self.assertEqual(kwargs, {"crash": False})
                self.assertTrue(all((ctx.root / "ext/manifests" / (name + ".json")).is_file() for name in descriptors))
                binding.enable.assert_not_awaited()
            ctx.restart = AsyncMock(side_effect=restarted)
            ctx.peer = SimpleNamespace(call=AsyncMock(return_value={"commands": [{"name": "coord.request_route"}]}))
            binding = PublicRequestRouting(ctx); binding.enable = AsyncMock()
            try:
                await binding.prepare()
                self.assertEqual(binding.enable.await_args_list,
                    [call("tabs", "interface", "tabs.py"), call("agents", "management", "agents.py")])
                self.assertTrue(binding.prepared)
            finally:
                if binding.native_root and binding.native_root.exists(): shutil.rmtree(binding.native_root)


class CompleteObserverControls(unittest.IsolatedAsyncioTestCase):
    async def grade(self, ctx):
        return await PublicRequestRouting(ctx).observe("r1", question(), mail_matches_request)

    async def test_actual_held_and_compacted_native_witnesses_pass_separately(self):
        for compact in (False, True):
            with self.subTest(compact=compact):
                result = await self.grade(Context(compact=compact))
                self.assertEqual(len(result["mail"]), 1)
                self.assertEqual(len(result["human"]), 1)

    async def test_wrong_scope_epoch_revision_expiry_question_or_actor_never_passes(self):
        for field, wrong in (("scope", {"session": "wrong"}), ("epoch", "other"), ("revision", True),
                             ("expires_at_ms", 1), ("question", {"request": "forged"}), ("request", "other")):
            with self.subTest(field=field):
                ctx = Context(); value = payload(); value[field] = wrong; ctx.records[0]["content"] = content(value)
                with self.assertRaises(CheckFailed): await self.grade(ctx)
        for field in ("principal", "actor", "author"):
            with self.subTest(sender_field=field):
                ctx = Context(); ctx.records[0]["from"].pop(field)
                with self.assertRaises(CheckFailed): await self.grade(ctx)

    async def test_current_tagged_uninspectable_notice_cannot_hide_as_no_mail(self):
        ctx = Context(fallback=True)
        ctx.records = [{"delivery": "bad", "request_notice": {"agent": "r1", "key": "source-current"}, "content": []}]
        with self.assertRaises(CheckFailed): await self.grade(ctx)

    async def test_bad_or_missing_source_tag_cannot_hide_current_payload_to_another_holder(self):
        for tag in ({"agent": "r1", "key": "wrong"}, {"agent": "other", "key": "source-current"},
                    None, [], {"agent": "r1"}):
            with self.subTest(tag=tag):
                ctx = Context(fallback=True)
                record = deepcopy(Context().records[0])
                record["to"] = {"agent": "r3", "incarnation": "i:a2"}
                if tag is None: record.pop("request_notice")
                else: record["request_notice"] = tag
                ctx.records = [record]
                with self.assertRaises(CheckFailed): await self.grade(ctx)

    async def test_old_different_question_notice_is_not_reclassified_as_current(self):
        ctx = Context(fallback=True)
        old = deepcopy(Context().records[0])
        old["request_notice"]["key"] = "old-source"
        old_payload = payload(); old_payload["request"] = "q-old"; old_payload["question"]["request"] = "q-old"
        old["content"] = content(old_payload)
        ctx.records = [old]
        self.assertEqual((await self.grade(ctx))["mail"], [])

    async def test_wrong_current_ui_or_projection_or_truncation_is_not_visibility(self):
        for mutate in (lambda c: c.panel["items"][0]["children"][0].update(text="old question"),
                       lambda c: c.panel.update(items=[]),
                       lambda c: c.panel["items"][0]["children"][0].pop("command"),
                       lambda c: c.panel["items"][0]["children"][0].update(command="other.action"),
                       lambda c: c.panel["items"][0]["children"][0]["args"].update(resource="other"),
                       lambda c: c.projection.update(agent="other"),
                       lambda c: c.projection.update(version=6), lambda c: c.projection.update(request_keys=[]),
                       lambda c: c.projection["notices"][0].update(source_key="old"),
                       lambda c: setattr(c, "more", "unread-page")):
            with self.subTest(mutation=mutate):
                ctx = Context(); mutate(ctx)
                with self.assertRaises(CheckFailed): await self.grade(ctx)
        ctx = Context(fallback=True); ctx.panel["items"][0]["children"][0]["description"] = ""
        with self.assertRaises(CheckFailed): await self.grade(ctx)

    async def test_native_payload_cannot_come_from_wrong_endpoint_lane_or_claim(self):
        for mutate in (lambda c: c.calls[0].update(ref="wrong"),
                       lambda c: c.calls[0]["args"].update(mode="start"),
                       lambda c: c.calls[0]["witness"]["entry"]["value"]["to"].update(incarnation="wrong"),
                       lambda c: c.calls[0]["witness"]["entry"]["value"]["request_notice"].update(key="wrong")):
            with self.subTest(mutation=mutate):
                ctx = Context(compact=True); mutate(ctx)
                with self.assertRaises(CheckFailed): await self.grade(ctx)

    async def test_other_recipient_current_notice_is_not_filtered_out(self):
        ctx = Context()
        other = deepcopy(ctx.records[0]); other.update(delivery="d-other", to={"agent": "r3"})
        ctx.records.append(other)
        result = await self.grade(ctx)
        self.assertEqual({mail["to"] for mail in result["mail"]}, {"r2", "r3"})

    async def test_coordinator_only_status_row_is_not_a_human_answer_action(self):
        ctx = Context()
        ctx.entry["value"]["route"] = "coordinator"
        ctx.projection["human"] = False
        row = ctx.panel["items"][0]["children"][0]
        row.pop("command"); row.pop("args"); row["text"] = "waiting for coordinator r2"
        self.assertEqual((await self.grade(ctx))["human"], [])
        for command in ("agents.answer", "other.action"):
            with self.subTest(command=command):
                row["command"] = command
                with self.assertRaises(CheckFailed): await self.grade(ctx)

    async def test_malformed_visible_reason_is_actual_ui_text_not_an_internal_code(self):
        ctx = Context(fallback=True)
        row = ctx.panel["items"][0]["children"][0]
        self.assertNotIn("invalid_request_route", row["description"])
        self.assertEqual((await self.grade(ctx))["human"][0]["reason"], "invalid_request_route")
        row["description"] = "human fallback without a specific reason"
        with self.assertRaises(CheckFailed): await self.grade(ctx)

    async def test_actual_missing_and_malformed_human_rows_need_no_mail(self):
        result = await self.grade(Context(fallback=True))
        self.assertEqual(result["mail"], [])
        self.assertEqual(result["human"][0]["reason"], "invalid_request_route")
        ctx = Context(fallback=True); ctx.entry = None; ctx.projection.update(version=0, fallback_reason=None)
        ctx.panel["items"][0]["children"][0]["description"] = ""
        self.assertEqual(len((await self.grade(ctx))["human"]), 1)
