"""Harness self-tests only: these fake contexts never qualify an adapter/runtime."""
import asyncio
from contextlib import asynccontextmanager
from copy import deepcopy
import json
from pathlib import Path
import shutil
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

# Shared registry must finish initialization before a slice registers itself.
from agent_conformance import cases
from agent_conformance import content_origin_cases as subject
from agent_conformance.content import examples
from agent_conformance.results import CheckFailed, HarnessGap, NotDeclared


class Native:
    def __init__(self):
        self.rows, self.messages = [], []

    def record(self, kind, **fields):
        self.rows.append({"seq": len(self.rows) + 1, "kind": kind, **fields})

    def mark(self):
        return len(self.rows)

    def records(self, kind=None, *, after=0):
        return [row for row in self.rows if row["seq"] > after and (kind is None or row["kind"] == kind)]

    def writes(self, after=0):
        return [row for row in self.records(after=after) if row.get("effectful")]

    async def call(self, verb, **params):
        if verb == "model_call":
            self.record("model_call", messages=deepcopy(self.messages))
        elif verb != "turn":
            raise AssertionError(verb)

    async def wait(self, predicate):
        if not predicate(self.rows):
            raise TimeoutError("test fixture has no insertion")


class Peer:
    def __init__(self, ctx, origin=None):
        self.ctx, self.core_origin = ctx, origin or {"principal": "undeclared", "actor": "undeclared"}

    async def wait(self, predicate):
        value = predicate()
        if not value:
            raise TimeoutError("test fixture omitted audit")
        return value

    async def call(self, method, params):
        if method == "resource.invoke":
            return self.ctx.evaluate(params, self.core_origin)
        if method == "command.invoke" and params["name"] == "k1.d17.probe":
            forbidden = {"error": {"data": {"kind": "forbidden"}}, "kind": "forbidden"}
            return {"stale": deepcopy(forbidden), "standing": deepcopy(forbidden)}
        raise AssertionError(method)


class Context:
    def __init__(self, shape="text_then_media", fault=None):
        self.native, self.evidence, self.events = Native(), [], []
        self.counter, self.fault = 0, fault
        self.declaration = {"content": {"types": ["text", "image"], "shape": shape},
                            "modes": {"start": {}}, "origin": [{"as": "attributed_user", "role": "user", "frame": "sender_header"}]}
        self.agents = [{"id": "fixture-agent"}]
        self.peer = Peer(self, {"principal": "fixture-owner", "actor": "fixture-owner"})
        self.entities = {}
        self.view = SimpleNamespace(initial="reset", reducer=SimpleNamespace(entities=self.entities, snapshots=1))

    async def facts(self):
        return {"epoch": "fixture-epoch", "caps": {"rev": 1, "agent.send": self.declaration}}

    async def ensure_session(self):
        return await self.facts()

    async def wait_facts(self, predicate):
        if not predicate({"state": "idle"}):
            raise AssertionError("unexpected predicate")

    async def attach(self, **kwargs):
        return self.view

    async def wait_view(self, view, predicate):
        if not predicate(view.reducer):
            raise CheckFailed("final public content missing/changed")

    def delivery(self):
        self.counter += 1
        return "fixture-delivery-" + str(self.counter)

    async def send(self, text, **args):
        content = args.get("content", [{"type": "text", "text": text}])
        origin = {"kind": "human", **self.peer.core_origin, **args.get("origin", {})}
        return self.evaluate({"args": {"content": content, "origin": origin, "render": ["attributed_user"],
                                       "delivery": self.delivery()}}, self.peer.core_origin)

    def evaluate(self, request, expected):
        args = request["args"]
        origin, content = deepcopy(args["origin"]), deepcopy(args["content"])
        effect = "fixture-effect-" + self.delivery()
        def result(outcome, **rest):
            audit = {"effect": effect, "outcome": outcome, **expected}
            if self.fault == "audit_actor":
                audit["actor"] = "D17-worker"
            self.events.append({"type": "effect", "data": audit})
            return {"effect": effect, "outcome": outcome, **rest}
        def refused(kind):
            if self.fault == "write_on_reject":
                self.native.record("command", effectful=True)
            response = result("rejected", error={"kind": kind})
            if self.fault == "rpc_rejection":
                response.pop("effect")
                response["rpc_error"] = True
                response["error"] = {"code": -32000, "data": {"outcome": "rejected", "effect": effect, "kind": kind}}
            return response
        for field in ("principal", "actor", "initiator"):
            if field in origin:
                if not isinstance(origin[field], str):
                    return refused("invalid")
                if origin[field] != expected.get(field):
                    return refused("forbidden")
        types = [block["type"] for block in content]
        if self.declaration["content"]["shape"] == "text_then_media" and (types.count("text") > 1 or "text" in types[1:]):
            return refused("unsupported")
        if args["render"] == ["user"] and (expected["principal"] != "fixture-owner" or origin.get("kind") != "human"):
            return refused("unsupported")
        origin.update(expected)
        origin["claimed"] = [field for field in ("kind", "agent", "sender") if field in args["origin"]]
        if self.fault == "receipt_actor":
            origin["actor"] = "D17-worker"
        if self.fault == "invent_agent":
            origin["agent"] = expected["principal"]
        if self.fault == "unclaimed":
            origin["claimed"] = []
        native = deepcopy(content)
        if expected["principal"] != "fixture-owner":
            sender = origin.get("sender", {}).get("principal", expected["principal"])
            label = " (sender asserted by extension)" if "sender" in origin and self.fault not in ("sender_unlabeled", "sender_assertion_decoy") else ""
            if "sender" in origin and self.fault == "sender_wrong_subject":
                label = " (asserted sender: someone-else)"
            claims = []
            for field in ("kind", "agent"):
                if field not in args["origin"] or self.fault == "model_agent_missing" and field == "agent":
                    continue
                qualifier = "" if self.fault == "model_kind_unasserted" and field == "kind" else "asserted "
                claims.append(f"{qualifier}{field}: {args['origin'][field]}")
            if (self.fault == "model_unqualified_invented_agent" and "agent" not in args["origin"]
                    or self.fault == "model_extra_unqualified_agent"):
                claims.append("agent: invented-endpoint")
            actor = expected["actor"]
            if self.fault == "model_actor_suffix" or self.fault == "model_undeclared_actor_suffix" and actor == "undeclared":
                actor += "-wrong"
            if self.fault == "model_principal_suffix":
                sender += "-wrong"
            delimiters = {"comma": ",", "comma_space": ", ", "semicolon": ";", "semicolon_space": "; ",
                          "space": " ", "tab": "\t"}
            separator, suffix = "; ", ""
            if self.fault and self.fault.startswith("model_extra_agent_"):
                suffix = delimiters[self.fault.removeprefix("model_extra_agent_")] + "agent: invented-endpoint"
            if self.fault and self.fault.startswith("model_valid_separator_"):
                separator = delimiters[self.fault.removeprefix("model_valid_separator_")]
            header = separator.join(claims) + suffix + f"\nfrom {sender} via {actor}{label}\n\n"
            native.insert(0, {"type": "text", "text": header})
        if self.fault == "drop":
            native = native[:-1]
        if self.fault == "reorder":
            native.reverse()
        if self.fault == "flatten":
            native = [{"type": "text", "text": repr(native)}]
        if self.fault == "media_bytes":
            for block in native:
                if block["type"] == "image":
                    block["data"] = "changed"
        if self.fault != "ack_only":
            self.native.record("command", effectful=True)
            self.native.record("input_received", content=deepcopy(native))
            self.native.record("inserted")
            if self.fault != "stale_model":
                message = {"role": "system" if self.fault == "privileged_role" else "user", "content": native}
                self.native.messages.append(message)
                value = {**deepcopy(message), "final": True}
                if self.fault == "public_drop":
                    value["content"] = []
                self.entities[("message", self.delivery())] = {"value": value}
        return result("accepted", data={"receipt": {"delivery": args["delivery"], "origin": origin}})


class Route:
    def __init__(self, ctx):
        self.ctx, self.actor = ctx, "extension:coord"
        self.owner = Peer(ctx, {"principal": "fixture-owner", "actor": "fixture-owner"})
        self.worker = Peer(ctx, {"principal": "D17-worker", "actor": "D17-worker"})
        self.principals = ["human:D17-principal", "agent:D17-principal"]
        self.transport = {"principal": self.actor, "actor": self.actor}

    def expected(self, principal):
        return {"principal": principal, "actor": self.actor, "initiator": "D17-worker"}

    async def forward(self, request, principal):
        return self.ctx.evaluate(request, self.expected(principal))

    async def autonomous(self, request):
        return self.ctx.evaluate(request, self.transport)

    async def connect(self):
        return Peer(self.ctx)


@asynccontextmanager
async def route(ctx):
    yield Route(ctx)


class ContentChecks(unittest.TestCase):
    def test_registry(self):
        self.assertIs(cases.HANDLERS["D16"], subject.content_shape)
        self.assertIs(cases.HANDLERS["D17"], subject.core_bound_origin)

    def test_shape_selection_is_declaration_driven(self):
        positive, negative = subject.content_vectors({"types": ["image", "text"], "shape": "text_then_media"})
        self.assertIn(["text", "image"], [[block["type"] for block in row] for row in positive])
        self.assertIn(["image", "text"], [[block["type"] for block in row] for row in negative])
        positive, negative = subject.content_vectors({"types": ["text", "image"], "shape": "ordered"})
        self.assertFalse(negative)
        self.assertIn(["text", "image", "text"], [[block["type"] for block in row] for row in positive])

    def test_unknown_mandatory_fixture_is_harness_failure(self):
        for declaration in ({}, {"types": ["future"], "shape": "ordered"},
                            {"types": ["text"], "shape": "future"}, {"types": ["text", "text"], "shape": "ordered"}):
            with self.subTest(declaration=declaration), self.assertRaises(HarnessGap):
                subject.content_vectors(declaration)

    def test_old_matching_message_is_not_new_insertion(self):
        content = [examples()["image"]]
        old = {"messages": [{"role": "user", "content": content}]}
        with self.assertRaises(CheckFailed):
            subject.new_model_message(old, deepcopy(old), content, sender_header=True)

    def test_duplicated_new_input_is_not_one_insertion(self):
        content = [examples()["text"]]
        with self.assertRaises(CheckFailed):
            subject.new_model_message({"messages": []}, {"messages": [{"content": content}] * 2}, content, sender_header=True)

    def test_all_bound_identities_and_claims_are_checked(self):
        expected = {"principal": "P", "actor": "extension:coord", "initiator": "W"}
        supplied = {"kind": "agent", "agent": "claimed-agent"}
        original = {**expected, **supplied, "claimed": ["kind", "agent"]}
        for field in ("principal", "actor", "initiator"):
            for value in (None, "different"):
                with self.subTest(field=field, value=value), self.assertRaises(CheckFailed):
                    subject.bound_origin({"origin": {**original, field: value}}, expected, supplied)
        with self.assertRaises(CheckFailed):
            subject.bound_origin({}, expected, supplied)

    def test_rpc_error_is_not_upgraded_to_rejection(self):
        unknown = subject.rpc_result({"code": -32000, "data": {"kind": "unavailable"}})
        self.assertIsNone(unknown["outcome"])
        with self.assertRaises(CheckFailed):
            cases.rejection(unknown, kinds=("unavailable",))

    def test_public_metadata_extras_do_not_change_content_bytes(self):
        message = {"role": "user", "content": [examples()["text"]]}
        public = {**deepcopy(message), "final": True}
        public["content"][0]["format"] = "plain"
        self.assertTrue(subject.public_content_matches(public, message))
        public["content"][0]["text"] = "changed"
        self.assertFalse(subject.public_content_matches(public, message))

    def test_model_claim_values_are_case_sensitive_and_field_local(self):
        subject.model_claims("asserted kind: agent; asserted agent: A1", {"kind": "agent", "agent": "A1"})
        subject.model_claims("kind: agent (claimed)\nagent: A1 (asserted by extension)", {"kind": "agent", "agent": "A1"})
        for header in ("asserted kind: agent; agent: A1", "asserted kind: agent; asserted agent: a1",
                       "asserted kind: agent; asserted agent: A1-longer",
                       "asserted kind: agent; not asserted agent: A1",
                       "not asserted kind: agent; asserted agent: A1"):
            with self.subTest(header=header), self.assertRaises(CheckFailed):
                subject.model_claims(header, {"kind": "agent", "agent": "A1"})
        for header in ("asserted kind: agent; asserted agent: A1", "asserted kind: agent; agent: A1 (claimed)"):
            with self.subTest(header=header), self.assertRaises(CheckFailed):
                subject.model_claims(header, {"kind": "agent"})

    def test_one_tokenizer_handles_delimiters_and_complete_values(self):
        supplied = {"kind": "agent", "agent": "A1"}
        for delimiter in (",", ", ", ";", "; "):
            with self.subTest(delimiter=delimiter):
                subject.model_claims("asserted kind: agent" + delimiter + "asserted agent: A1", supplied)
                for extra in ("agent: invented-endpoint", "asserted agent: wrong"):
                    with self.assertRaises(CheckFailed):
                        subject.model_claims("asserted kind: agent; asserted agent: A1" + delimiter + extra, supplied)
                with self.assertRaisesRegex(CheckFailed, "invented agent"):
                    subject.model_claims("asserted kind: agent" + delimiter + "agent: invented-endpoint", {"kind": "agent"})
        for suffix in (" extra", "\tagent: invented-endpoint", " (claimed)agent: invented-endpoint"):
            with self.subTest(suffix=suffix), self.assertRaises(CheckFailed):
                subject.model_claims("asserted kind: agent; asserted agent: A1" + suffix, supplied)
        # Full opaque supplied values and from/via identity contents are not re-lexed as invented labels.
        subject.model_claims("asserted kind: agent; asserted agent: A1,agent:literal", {"kind": "agent", "agent": "A1,agent:literal"})
        subject.model_claims("asserted kind: agent\nfrom human,agent:opaque via extension:coord", {"kind": "agent"})
        for delimiter in (",", ";"):
            with self.subTest(frame_delimiter=delimiter), self.assertRaises(CheckFailed):
                subject.model_claims("asserted kind: agent\nfrom P via extension:coord" + delimiter + "agent: invented", {"kind": "agent"})

    def test_sender_qualification_is_associated_with_actual_sender(self):
        sender = {"principal": "actual-sender"}
        self.assertTrue(subject.sender_assertion("from actual-sender via extension:coord (sender asserted by extension)", sender))
        self.assertTrue(subject.sender_assertion("asserted sender: actual-sender via extension:coord", sender))
        for header in ("asserted kind: agent\nfrom actual-sender via extension:coord",
                       "asserted kind: agent; from actual-sender via extension:coord",
                       "asserted sender: someone-else\nfrom actual-sender via extension:coord",
                       "asserted sender: actual-sender-other via extension:coord",
                       "asserted sender: different-actual-sender via extension:coord",
                       "not asserted sender: actual-sender via extension:coord"):
            with self.subTest(header=header):
                self.assertFalse(subject.sender_assertion(header, sender))

    def test_exact_from_via_fields_not_prefixes_other_fields_or_case_folding(self):
        self.assertTrue(subject.framed_origin("asserted kind: agent; From P via extension:coord", "P", "extension:coord"))
        self.assertTrue(subject.framed_origin("from undeclared via undeclared", "undeclared", "undeclared"))
        for header, principal, actor in (
                ("from P via extension:coord-wrong", "P", "extension:coord"),
                ("from P-wrong via extension:coord", "P", "extension:coord"),
                ("from p via extension:coord", "P", "extension:coord"),
                ("from P via Extension:coord", "P", "extension:coord"),
                ("from undeclared via undeclared-wrong", "undeclared", "undeclared"),
                ("from undeclared-wrong via undeclared", "undeclared", "undeclared"),
                ("asserted agent: extension:coord; from P via someone-else", "P", "extension:coord")):
            with self.subTest(header=header):
                self.assertFalse(subject.framed_origin(header, principal, actor))

    def test_unqualified_agent_cannot_be_invented_or_promoted(self):
        for annotation in ("agent: invented-endpoint", "Agent: invented-endpoint", "asserted agent: invented-endpoint",
                           "agent: invented-endpoint (claimed)"):
            header = "asserted kind: agent\n" + annotation + "\nfrom P via extension:coord"
            with self.subTest(annotation=annotation), self.assertRaisesRegex(CheckFailed, "invented agent"):
                subject.model_claims(header, {"kind": "agent"})
        with self.assertRaises(CheckFailed):
            subject.model_claims("asserted kind: agent; asserted agent: A1; agent: invented-endpoint", {"kind": "agent", "agent": "A1"})
        # A principal's opaque prefix is not a field declaration.
        subject.model_claims("asserted kind: agent\nfrom agent:D17-principal via extension:coord", {"kind": "agent"})

    def test_sender_qualification_names_this_identity_not_a_neighbor(self):
        sender = {"principal": "actual-sender"}
        for note in ("asserted sender: actual-sender", "sender asserted by extension", "sender claimed, not core-attested"):
            with self.subTest(note=note):
                self.assertTrue(subject.sender_assertion("from actual-sender via extension:coord (" + note + ")", sender))
        for note in ("asserted sender: someone-else", "sender assertion: someone-else", "sender asserted: someone-else",
                     "asserted sender: actual-sender-wrong", "sender asserted by core", "not asserted sender: actual-sender"):
            with self.subTest(note=note):
                self.assertFalse(subject.sender_assertion("from actual-sender via extension:coord (" + note + ")", sender))

    def test_body_claim_labels_do_not_rescue_missing_model_frame(self):
        expected = {"principal": "P", "actor": "extension:coord"}
        message = {"role": "user", "content": [{"type": "text", "text":
            "from P via extension:coord\nD17-body:asserted kind: agent; asserted agent: A1"}]}
        with self.assertRaisesRegex(CheckFailed, "asserted kind claim"):
            subject.model_origin(message, {"role": "user"}, expected, {"kind": "agent", "agent": "A1"})

    def test_proof_probe_needs_actual_core_error_not_provider_refusal(self):
        with self.assertRaises(CheckFailed):
            subject.core_forbidden({"result": {"outcome": "rejected", "error": {"kind": "forbidden"}}})


class HandlerChecks(unittest.IsolatedAsyncioTestCase):
    async def test_d16_both_shapes_require_native_and_public_witnesses(self):
        for shape in ("text_then_media", "ordered"):
            with self.subTest(shape=shape):
                ctx = Context(shape)
                await subject.content_shape(ctx)
                self.assertTrue(any(row["kind"] == "D16_native_shape" and row["public_message"] for row in ctx.evidence))

    async def test_qualified_rpc_refusals_preserve_audit_identity(self):
        ctx = Context(fault="rpc_rejection")
        await subject.content_shape(ctx)
        self.assertTrue(all(row["effect_audit"]["effect"] for row in ctx.evidence if row["kind"] == "D16_rejected_shape"))
        ctx = Context(fault="rpc_rejection")
        with patch.object(subject, "public_origin_route", route):
            await subject.core_bound_origin(ctx)

    async def test_missing_mandatory_model_or_audit_is_harness_failure(self):
        with self.assertRaises(HarnessGap):
            subject.model_messages({})
        with self.assertRaises(HarnessGap):
            await subject.final_audit(Context(), {"outcome": "rejected", "error": {"kind": "unsupported"}})

    async def test_d16_false_passes(self):
        for fault in ("ack_only", "drop", "reorder", "flatten", "media_bytes", "stale_model", "public_drop", "write_on_reject"):
            with self.subTest(fault=fault), self.assertRaises((CheckFailed, TimeoutError)):
                await subject.content_shape(Context(fault=fault))

    async def test_d17_all_clauses(self):
        ctx = Context()
        with patch.object(subject, "public_origin_route", route):
            await subject.core_bound_origin(ctx)
        origins = [row["origin"] for row in ctx.evidence if row["kind"] == "D17_model_origin"]
        self.assertEqual(5, len(origins))
        negatives = [row for row in ctx.evidence if row["kind"] == "D17_prewrite_rejection"]
        self.assertEqual(8, len(negatives))
        self.assertTrue(any(row["origin"]["kind"] == "human" and row["error_kind"] == "unsupported" for row in negatives))
        self.assertTrue(any(row["principal"] == "undeclared" for row in origins))
        sender = next(row for row in origins if "sender" in row)
        self.assertEqual("extension:coord", sender["principal"])
        self.assertNotIn("initiator", sender)
        self.assertEqual("inbox-record", sender["sender"]["provenance"])

    async def test_d17_false_passes(self):
        for fault in ("ack_only", "receipt_actor", "audit_actor", "invent_agent", "unclaimed", "sender_unlabeled", "privileged_role", "write_on_reject"):
            with self.subTest(fault=fault), self.assertRaises((CheckFailed, TimeoutError)):
                with patch.object(subject, "public_origin_route", route):
                    await subject.core_bound_origin(Context(fault=fault))

    async def test_d17_complete_handler_rejects_missing_native_agent_despite_good_receipt(self):
        ctx = Context(fault="model_agent_missing")
        with patch.object(subject, "public_origin_route", route), self.assertRaisesRegex(CheckFailed, "asserted agent claim"):
            await subject.core_bound_origin(ctx)
        self.assertTrue(any(row["kind"] == "inserted" for row in ctx.native.rows))
        self.assertNotIn("D17-asserted-agent", json.dumps(ctx.native.messages))

    async def test_d17_complete_handler_rejects_kind_assertion_as_sender_label_decoy(self):
        ctx = Context(fault="sender_assertion_decoy")
        with patch.object(subject, "public_origin_route", route), self.assertRaisesRegex(CheckFailed, "this sender's extension assertion"):
            await subject.core_bound_origin(ctx)
        native = json.dumps(ctx.native.messages[-1])
        self.assertIn("asserted kind: agent", native)
        self.assertIn("D17-original-inbox-sender", native)
        self.assertNotIn("sender asserted", native)

    async def test_d17_complete_handler_rejects_displayed_identity_prefixes(self):
        for fault in ("model_actor_suffix", "model_principal_suffix", "model_undeclared_actor_suffix"):
            ctx = Context(fault=fault)
            with self.subTest(fault=fault), patch.object(subject, "public_origin_route", route), self.assertRaisesRegex(CheckFailed, "exact sender/current-actor frame"):
                await subject.core_bound_origin(ctx)
            if fault == "model_undeclared_actor_suffix":
                text = json.dumps(ctx.native.messages[-1])
                self.assertIn("from undeclared via undeclared-wrong", text)

    async def test_d17_complete_handler_rejects_wrong_sender_subject_in_same_clause(self):
        ctx = Context(fault="sender_wrong_subject")
        with patch.object(subject, "public_origin_route", route), self.assertRaisesRegex(CheckFailed, "this sender's extension assertion"):
            await subject.core_bound_origin(ctx)
        text = json.dumps(ctx.native.messages[-1])
        self.assertIn("from D17-original-inbox-sender via extension:coord (asserted sender: someone-else)", text)

    async def test_d17_complete_handler_rejects_unqualified_agent_invention(self):
        ctx = Context(fault="model_unqualified_invented_agent")
        with patch.object(subject, "public_origin_route", route), self.assertRaisesRegex(CheckFailed, "invented agent"):
            await subject.core_bound_origin(ctx)
        self.assertIn("agent: invented-endpoint", json.dumps(ctx.native.messages[-1]))

    async def test_d17_complete_handler_rejects_extra_unqualified_agent_with_good_claim(self):
        with patch.object(subject, "public_origin_route", route), self.assertRaisesRegex(CheckFailed, "unqualified/changed agent"):
            await subject.core_bound_origin(Context(fault="model_extra_unqualified_agent"))

    async def test_d17_complete_handler_rejects_extra_agent_for_all_delimiters(self):
        for delimiter in ("comma", "comma_space", "semicolon", "semicolon_space", "space", "tab"):
            ctx = Context(fault="model_extra_agent_" + delimiter)
            with self.subTest(delimiter=delimiter), patch.object(subject, "public_origin_route", route), self.assertRaises(CheckFailed):
                await subject.core_bound_origin(ctx)
            native = json.dumps(ctx.native.messages[-1])
            self.assertIn("asserted agent: D17-asserted-agent", native)
            self.assertIn("agent: invented-endpoint", native)

    async def test_d17_complete_handler_accepts_qualified_separator_variants(self):
        for delimiter in ("comma", "comma_space", "semicolon", "semicolon_space"):
            with self.subTest(delimiter=delimiter), patch.object(subject, "public_origin_route", route):
                await subject.core_bound_origin(Context(fault="model_valid_separator_" + delimiter))

    async def test_d17_complete_handler_rejects_unasserted_native_kind(self):
        with patch.object(subject, "public_origin_route", route), self.assertRaisesRegex(CheckFailed, "asserted kind claim"):
            await subject.core_bound_origin(Context(fault="model_kind_unasserted"))

    async def test_missing_callback_cannot_be_exclusion_or_pass(self):
        @asynccontextmanager
        async def missing(ctx):
            raise HarnessGap("callback fixture unavailable")
            yield  # pragma: no cover
        with patch.object(subject, "public_origin_route", missing), self.assertRaises(HarnessGap):
            await subject.core_bound_origin(Context())

    async def test_absent_optional_send_is_unsupported_declared(self):
        ctx = Context()
        async def no_send():
            return {"caps": {}}
        ctx.facts = no_send
        with self.assertRaises(NotDeclared):
            await subject.content_shape(ctx)


class CallbackWireChecks(unittest.IsolatedAsyncioTestCase):
    async def test_exact_public_proof_forwarding_and_no_ambient_authority(self):
        """Exercise the actual callback script/SDK against a private fake wire.

        This catches fixture bugs; the fake server is NOT identity evidence for
        a D17 runtime pass. Only the real core's hello/callback/audit can do that.
        """
        # Small throwaway socket/scripts only; this path stays below 103 bytes.
        with tempfile.TemporaryDirectory(prefix="d17-cb-") as temporary:
            root = Path(temporary)
            script = root / "callback.py"
            script.write_text(subject.CALLBACK_SOURCE)
            sdk = Path(subject.__file__).resolve().parents[2] / "extensions/vqro_ext.py"
            shutil.copyfile(sdk, root / "vqro_ext.py")
            endpoint = root / "s"
            ready = asyncio.get_running_loop().create_future()
            pending, requests, handlers = {}, [], []
            registered, connection = set(), {}
            revoked = False
            async def serve(reader, writer):
                handlers.append(asyncio.current_task())
                connection["writer"] = writer
                try:
                    while data := await reader.readline():
                        row = json.loads(data)
                        if "method" not in row:
                            pending.pop(row["id"]).set_result(row)
                            continue
                        method, params = row["method"], row.get("params", {})
                        error, result = None, {}
                        if method == "hello":
                            self.assertEqual("coord", params["extension"])
                            result = {"principal": "extension:coord", "actor": "extension:coord"}
                        elif method == "command.register":
                            registered.add(params["name"])
                        elif method == "resource.invoke":
                            requests.append(params)
                            if params.get("args", {}).get("refuse"):
                                error = {"code": -32000, "message": "origin refused", "data": {"kind": "forbidden", "outcome": "rejected", "effect": "unit-rejected-effect"}}
                            elif (revoked and params.get("invocation") == "unit-scope") or "on_behalf_of" in params:
                                error = {"code": -32000, "message": "scope refused", "data": {"kind": "forbidden"}}
                            else:
                                result = {"outcome": "accepted", "effect": "unit-effect"}
                        else:
                            raise AssertionError(method)
                        reply = {"jsonrpc": "2.0", "id": row["id"], "error": error} if error else {"jsonrpc": "2.0", "id": row["id"], "result": result}
                        writer.write(json.dumps(reply).encode() + b"\n")
                        await writer.drain()
                        if len(registered) == 4 and not ready.done():
                            ready.set_result(True)
                finally:
                    writer.close()
                    await writer.wait_closed()
            server = await asyncio.start_unix_server(serve, path=str(endpoint))
            process = None
            try:
                process = await asyncio.create_subprocess_exec(sys.executable, "-W", "error", str(script),
                    env={"HOME": str(root), "PATH": "/usr/bin:/bin", "LANG": "C.UTF-8", "PYTHONDONTWRITEBYTECODE": "1",
                         "VQRO_SOCKET": str(endpoint), "VQRO_EXTENSION": "coord", "VQRO_EXTENSION_TOKEN": "unit-not-a-real-token"},
                    stdout=asyncio.subprocess.DEVNULL, stderr=asyncio.subprocess.PIPE)
                await asyncio.wait_for(ready, 3)
                async def callback(name, args, proof):
                    ident = 10000 + len(pending) + len(requests)
                    future = asyncio.get_running_loop().create_future()
                    pending[ident] = future
                    row = {"jsonrpc": "2.0", "id": ident, "method": "command.invoke", "params": {
                        "name": name, "args": args, "invocation": proof, "actor": "W", "principal": "P", "initiator": "W"}}
                    connection["writer"].write(json.dumps(row).encode() + b"\n")
                    await connection["writer"].drain()
                    response = await asyncio.wait_for(future, 3)
                    self.assertNotIn("error", response)
                    self.assertNotIn(proof, json.dumps(response))
                    return response["result"]
                request = {"resource": "a", "op": "agent.send", "args": {"origin": {"kind": "agent"}}}
                response = await callback("k1.d17.forward", {"request": request}, "unit-scope")
                self.assertEqual({"actor": "W", "principal": "P", "initiator": "W"}, response["callback"])
                self.assertEqual({**request, "invocation": "unit-scope"}, requests[-1])
                refused = await callback("k1.d17.forward", {"request": {**request, "args": {"refuse": True}}}, "unit-scope")
                result = subject.rpc_result(refused["rpc_error"])
                cases.rejection(result, kinds=("forbidden",))
                self.assertEqual("unit-rejected-effect", result["effect"])
                self.assertEqual("P", refused["callback"]["principal"])
                revoked = True
                probes = await callback("k1.d17.probe", {"request": request, "principal": "P"}, "unit-new-scope")
                subject.core_forbidden(probes["stale"])
                subject.core_forbidden(probes["standing"])
                self.assertEqual("unit-scope", requests[-2]["invocation"])
                self.assertNotIn("invocation", requests[-1])
                self.assertEqual("P", requests[-1]["on_behalf_of"])
                await callback("k1.d17.autonomous", {"request": request}, "unit-autonomous-control")
                self.assertNotIn("invocation", requests[-1])
                self.assertNotIn("on_behalf_of", requests[-1])
            finally:
                if process is not None:
                    if process.returncode is None:
                        process.terminate()
                    try:
                        await asyncio.wait_for(process.communicate(), 3)
                    except TimeoutError:
                        process.kill()
                        await asyncio.wait_for(process.communicate(), 3)
                server.close()
                await server.wait_closed()
                if handlers:
                    await asyncio.wait_for(asyncio.gather(*handlers), 3)


if __name__ == "__main__":
    unittest.main()
