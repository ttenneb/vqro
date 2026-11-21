"""D16/D17 public-core cases; independent native evidence, never ACK-only grades.

Imported by cases.py only after its shared helpers have been defined. Local unit
suites import cases first. No adapter modules or protected-store mutation.
"""
import asyncio
from contextlib import asynccontextmanager
from copy import deepcopy
import hashlib
from pathlib import Path
import re
import shutil
import sys

from .cases import accepted, audit_fence, caps, case, fresh_view, rejection
from .content import compare_content, examples
from .results import CheckFailed, HarnessGap, check
from .wire import Peer, RpcError


def model_messages(row):
    messages = row.get("messages")
    if not isinstance(messages, list) or any(not isinstance(message, dict) for message in messages):
        raise HarnessGap("native model-request oracle omitted its actual message array")
    return messages


def new_model_message(before, after, content, *, sender_header):
    """Require a new native envelope, not an old matching message or an ACK."""
    old, new = model_messages(before), model_messages(after)
    check(len(new) > len(old) and new[:len(old)] == old,
          "native model request lost/rewrote its prior messages or contains no new input")
    matched = []
    for message in new[len(old):]:
        try:
            compare_content(content, message.get("content"), sender_header=sender_header)
        except CheckFailed:
            continue
        matched.append(message)
    check(len(matched) == 1, "new native model input was missing, duplicated, reordered or changed")
    return matched[0]


async def model_cut(ctx, label):
    mark = ctx.native.mark()
    await ctx.native.call("model_call", id_ref=label)
    rows = ctx.native.records("model_call", after=mark)
    if len(rows) != 1:
        raise HarnessGap("model-call control did not produce exactly one independent envelope")
    model_messages(rows[0])
    return rows[0]


async def idle(ctx):
    await ctx.native.call("turn", state="settled")
    await ctx.wait_facts(lambda facts: facts.get("state") == "idle")


def rpc_result(error):
    """Keep a real wire error's outcome; an unqualified error is NOT rejection."""
    if not isinstance(error, dict):
        raise HarnessGap("callback failure omitted its actual public wire error")
    data = error.get("data") or {}
    return {"outcome": data.get("outcome"), "effect": data.get("effect"), "error": error, "rpc_error": True}


async def final_audit(ctx, result):
    # PublicImplementation.invoke preserves error.data verbatim, but does not
    # lift its effect ID. Read that actual core-issued ID, never synthesize one.
    effect = result.get("effect") or (result.get("error", {}).get("data") or {}).get("effect")
    if not isinstance(effect, str) or not effect:
        raise HarnessGap("native-effect result omitted its core-issued audit identifier")
    return await audit_fence(ctx, {**result, "effect": effect})


async def refresh_request(ctx, request):
    await idle(ctx)
    facts = await ctx.facts()
    request = deepcopy(request)
    request["args"].update(epoch=facts["epoch"], caps_rev=facts["caps"]["rev"])
    return request


def content_vectors(declaration):
    types, shape = declaration.get("types"), declaration.get("shape")
    if not isinstance(types, list) or not types or any(not isinstance(tag, str) for tag in types) or len(set(types)) != len(types):
        raise HarnessGap("D16 needs a nonempty declared content type set")
    samples = examples()
    unknown = [tag for tag in types if tag not in samples]
    if unknown:
        raise HarnessGap("D16 has no native content fixture for declared types: " + repr(unknown))
    if shape not in ("ordered", "text_then_media"):
        raise HarnessGap("D16 has no scenario for declared content shape: " + repr(shape))
    positive = [[deepcopy(samples[tag])] for tag in types]
    negative = []
    text = samples["text"] if "text" in types else None
    media = [samples[tag] for tag in types if tag != "text"]
    if shape == "text_then_media":
        if text:
            negative.append([text, {"type": "text", "text": "D16-separate-second-text"}])
        for block in media:
            if text:
                positive.append([text, block])
                negative.extend(([block, text], [text, block, text]))
    else:
        positive.extend(([deepcopy(samples[tag]) for tag in types],
                         [deepcopy(samples[tag]) for tag in reversed(types)]))
        if text:
            positive.append([text, *media, {"type": "text", "text": "D16-separate-final-text"}])
    # Each vector is distinct: a late publication of a prior test input cannot
    # stand in for this input's final public content witness.
    return [row for index, row in enumerate(positive) if row not in positive[:index]], negative


def public_content_matches(value, message):
    if value.get("final") is not True or value.get("role") != message.get("role"):
        return False
    try:
        compare_content(message["content"], value.get("content"))
    except CheckFailed:
        return False
    return True


@case("D16")
async def content_shape(ctx):
    declaration = await caps(ctx, "agent.send")
    positive, negative = content_vectors(declaration.get("content", {}))
    await idle(ctx)
    framed = any(row.get("frame") == "sender_header" for row in declaration.get("origin", []))
    for index, content in enumerate(negative):
        mark = ctx.native.mark()
        result = await ctx.send("unused", content=deepcopy(content))
        rejection(result, kinds=("unsupported",))
        audit = (await final_audit(ctx, result))["data"]
        await model_cut(ctx, f"D16-rejection-fence-{index}")
        check(not ctx.native.writes(mark), "invalid text_then_media order reached native input")
        ctx.evidence.append({"kind": "D16_rejected_shape", "types": [block["type"] for block in content],
                             "native_mark": mark, "effectful_writes": 0, "effect_audit": audit,
                             "error": result.get("error")})
    view = await fresh_view(ctx)
    for index, content in enumerate(positive):
        await idle(ctx)
        previous = set(view.reducer.entities)
        before = await model_cut(ctx, f"D16-before-{index}")
        mark = ctx.native.mark()
        result = await ctx.send("unused", content=deepcopy(content))
        receipt = accepted(result)
        await final_audit(ctx, result)
        await ctx.native.wait(lambda rows: any(row["kind"] == "inserted" and row["seq"] > mark for row in rows))
        received = ctx.native.records("input_received", after=mark)
        inserted = ctx.native.records("inserted", after=mark)
        check(len(received) == 1 and len(inserted) == 1, "valid shape was duplicated or never inserted natively")
        compare_content(content, received[0].get("content"), sender_header=framed)
        after = await model_cut(ctx, f"D16-after-{index}")
        message = new_model_message(before, after, content, sender_header=framed)
        def final_public(reducer):
            return [(key, row["value"]) for key, row in reducer.entities.items()
                    if key not in previous and key[0] == "message" and public_content_matches(row["value"], message)]
        await ctx.wait_view(view, lambda reducer: bool(final_public(reducer)))
        public = final_public(view.reducer)
        check(len(public) == 1, "final public typed content was duplicated")
        ctx.evidence.append({"kind": "D16_native_shape", "shape": declaration["content"]["shape"],
                             "types": [block["type"] for block in content], "delivery": receipt["delivery"],
                             "input_seq": received[0]["seq"], "inserted_seq": inserted[0]["seq"],
                             "model_seq": after["seq"], "native_message": message,
                             "public_message": public[0]})


# A test extension, not a coordination implementation. Its sender payload is an
# explicit extension assertion; this case does not certify upstream inbox storage.
# The public SDK dispatches command callbacks; no custom/privileged wire is added.
CALLBACK_SOURCE = '''import os
import threading
from concurrent.futures import ThreadPoolExecutor
from vqro_ext import Vqro, VqroError

hello = None
last_proof = None
registered = threading.Event()
executor = ThreadPoolExecutor(max_workers=1)
def handle(method, params):
    global last_proof
    if method != "command.invoke":
        raise RuntimeError("unexpected callback")
    name = params["name"]
    if name == "k1.d17.meta":
        return {"hello": {k: hello.get(k) for k in ("actor", "principal", "initiator")},
                "pid": os.getpid(), "ready": registered.is_set()}
    request = dict(params["args"]["request"])
    if name == "k1.d17.forward":
        proof = params.get("invocation")
        if not isinstance(proof, str) or not proof:
            raise RuntimeError("core callback omitted invocation proof")
        last_proof = proof
        request["invocation"] = proof
        wrapper = {"callback": {k: params.get(k) for k in ("actor", "principal", "initiator")}}
        try:
            wrapper["result"] = client.call("resource.invoke", request)
        except VqroError as error:
            wrapper["rpc_error"] = error.wire_error
        return wrapper
    if name == "k1.d17.autonomous":
        # No ambient invocation in this thread: current transport authority only.
        return {"result": executor.submit(client.call, "resource.invoke", request).result(3)}
    if name == "k1.d17.probe":
        def attempt(request):
            try:
                return {"result": client.call("resource.invoke", request)}
            except VqroError as error:
                return {"error": error.wire_error, "kind": error.kind}
        stale = dict(request, invocation=last_proof)
        # A fresh thread has no ambient callback proof. This tests standing
        # delegation rather than the new probe command's scoped authority.
        standing = dict(request, on_behalf_of=params["args"]["principal"])
        return {"stale": executor.submit(attempt, stale).result(3),
                "standing": executor.submit(attempt, standing).result(3)}
    raise RuntimeError("unexpected command")

client = Vqro(os.environ["VQRO_SOCKET"], handle)
hello = client.call("hello", {"extension": os.environ["VQRO_EXTENSION"],
                             "extension_token": os.environ["VQRO_EXTENSION_TOKEN"]})
for name in ("k1.d17.meta", "k1.d17.forward", "k1.d17.probe", "k1.d17.autonomous"):
    client.call("command.register", {"name": name, "title": "D17 isolated callback fixture"})
registered.set()
threading.Event().wait()
'''


def bound_origin(receipt, expected, supplied):
    origin = receipt.get("origin")
    check(isinstance(origin, dict), "receipt omitted core-bound origin")
    for field in ("principal", "actor", "initiator"):
        check(origin.get(field) == expected.get(field), "receipt changed core " + field)
    claimed = origin.get("claimed")
    check(isinstance(claimed, list), "receipt omitted asserted-claim classification")
    for field in ("kind", "agent", "sender"):
        if field in supplied:
            check(origin.get(field) == supplied[field] and field in claimed,
                  "caller " + field + " was changed or promoted to core authority")
        else:
            check(field not in origin and field not in claimed, "origin inferred " + field + " from identity spelling")
    check(not any(field in claimed for field in ("principal", "actor", "initiator")),
          "core identities were relabeled as caller assertions")
    return origin


def model_claims(header, supplied):
    """Grade explicit field/value assertions in the frame, not body keywords.

    Recognize labeled claims ("asserted kind: agent") and field-local
    parentheticals ("agent: endpoint (claimed)"). A claim about one field never
    qualifies a different field, and opaque values remain case-sensitive.
    """
    qualifier = r"(?i:asserted|claimed)"
    start = r"(?:^|[\r\n;,()\[\]])[ \t]*"
    end = r"[ \t]*(?=$|[\r\n;,)\]])"
    # One tokenizer consumes complete qualified values OR rejects an annotation.
    # Positive claims and extra fields cannot use different delimiter rules.
    # Consume from/via scalar identities without interpreting their contents as
    # labels; model_origin independently checks their exact values below.
    alternatives = [r"(?P<identity>(?:^|[\r\n;])[ \t]*(?i:from)[ \t]+\S+[ \t]+(?i:via)[ \t]+[^\s(\[;,]+)"]
    for field in ("kind", "agent"):
        if field not in supplied:
            continue
        label, value = rf"(?i:{field})", re.escape(supplied[field])
        before = rf"{qualifier}\s+{label}\s*[:=]\s*{value}"
        after = rf"{label}\s*[:=]\s*{value}\s*\(\s*{qualifier}\b[^()\r\n;,]*\)"
        alternatives.append(rf"(?P<claim_{field}>{start}(?:{before}|{after}){end})")
    alternatives.append(rf"{start}(?:{qualifier}\s+)?(?P<unqualified>(?i:kind|agent))\s*[:=]")
    seen = set()
    for token in re.finditer("|".join(alternatives), header):
        if token.group('identity') is not None:
            continue
        if token.group('unqualified') is not None:
            field = token.group('unqualified').lower()
            check(False, "model invented " + field + " absent from the supplied origin" if field not in supplied
                  else "model added an unqualified/changed " + field + " annotation; omitted/changed the asserted " + field + " claim")
        for field in ("kind", "agent"):
            if field in supplied and token.group('claim_' + field) is not None:
                seen.add(field)
    for field in ("kind", "agent"):
        if field in supplied:
            check(field in seen, "model envelope omitted/changed the asserted " + field + " claim")


def sender_assertion(header, sender):
    """Bind an explicit value, or a local valueless note, to THIS sender.

    Merely sharing a clause with `asserted sender: someone-else` is not proof.
    A valueless `(sender asserted by extension)` note belongs to its enclosing
    exact from/via frame; a note naming a sender must name the supplied identity.
    """
    identity = re.escape(sender['principal'])
    actor = r"[^\s()]+"
    label = r"(?i:(?:asserted|claimed)\s+sender(?:\s+provenance)?|sender(?:\s+provenance)?\s+(?:assertion|(?:is\s+)?(?:asserted|claimed)))"
    explicit = rf"{label}\s*[:=]\s*{identity}(?:\s+(?i:via)\s+{actor})?"
    local_note = r"(?i:sender(?:\s+provenance)?\s+(?:is\s+)?(?:asserted|claimed)(?:\s+by\s+(?:the\s+)?extension)?(?:,\s*not\s+core-(?:proven|attested))?)"
    for clause in re.split(r"[\r\n;]+", header):
        clause = clause.strip()
        if re.fullmatch(explicit, clause):
            return True
        frame = re.fullmatch(rf"(?i:from)\s+{identity}\s+(?i:via)\s+{actor}\s+\((?P<note>[^()]*)\)", clause)
        if frame:
            note = frame['note'].strip()
            if re.fullmatch(explicit, note) or re.fullmatch(local_note, note):
                return True
    return False


def framed_origin(header, principal, actor):
    """Match actual from/via fields, including when principal equals actor.

    An ID in another field, or as a prefix of a different opaque ID, is not the
    requested native sender/transport identity. Labels may vary in case; IDs may not.
    """
    pair = rf"(?i:from)\s+{re.escape(principal)}\s+(?i:via)\s+{re.escape(actor)}(?=$|[\s(\[])"
    return any(re.match(pair, clause.strip()) is not None for clause in re.split(r"[\r\n;]+", header))


def model_origin(message, rendering, expected, supplied, *, sender=None):
    check(message.get("role") == rendering.get("role"), "native model role differs from declared rendering")
    check(message.get("role") not in ("system", "developer"), "caller claim gained privileged model authority")
    content = message.get("content", [])
    text = "\n".join(block.get("text", "") for block in content if isinstance(block, dict))
    # Original content starts at this independently checked literal marker.
    # Claims planted in the input body cannot stand in for framing metadata.
    header = text.split("D17-body:", 1)[0]
    model_claims(header, supplied)
    principal = sender["principal"] if sender else expected["principal"]
    check(framed_origin(header, principal, expected["actor"]), "native envelope omitted exact sender/current-actor frame")
    if sender:
        check(sender_assertion(header, sender),
              "inbox-record provenance was not labeled as this sender's extension assertion in the model frame")
        check(expected["principal"] != principal and expected.get("initiator") != principal,
              "dispatch identity was rewritten to the original inbox sender", component="harness")
    return text


def core_forbidden(result):
    error = result.get("error")
    check(isinstance(error, dict) and error.get("data", {}).get("kind") == "forbidden"
          and not error["data"].get("effect") and result.get("result") is None,
          "callback proof unexpectedly granted persistent/reusable authority", component="core")


class OriginRoute:
    def __init__(self, ctx):
        self.ctx, self.owner, self.worker = ctx, None, None
        self.peers, self.owned, self.principals = [], [], []

    async def connect(self, identity=None):
        peer = await Peer.connect(self.ctx.backend.env["VQRO_SOCKET"], max_record=8 << 20)
        self.peers.append(peer)
        if identity is not None:
            hello = await peer.call("hello", {"client": "D17-fixture", "identity": identity})
            peer.core_origin = {key: hello[key] for key in ("principal", "actor", "initiator")
                                if isinstance(hello.get(key), str) and hello[key]}
            check(all(peer.core_origin.get(key) for key in ("principal", "actor")),
                  "public hello omitted actual connection attribution", component="core")
        return peer

    async def create(self, key, value):
        check(await self.owner.call("workspace.get", {"key": key}) is None,
              "D17 fixture key already exists: " + key, component="harness")
        await self.owner.call("workspace.apply", {"ops": [{"op": "set", "key": key, "value": value, "expect": 0}]})
        self.owned.append((key, value))

    async def start(self):
        self.owner = await self.connect({"kind": "owner"})
        if self.owner.core_origin["principal"] == "owner-unset":
            await self.owner.call("workspace.owner.bootstrap", {"principal": "D17-owner"})
            self.owner = await self.connect({"kind": "owner"})
        self.worker = await self.connect({"kind": "principal", "principal": "D17-worker"})
        # Changing only the spelling/prefix must not synthesize kind/agent.
        for index, name in enumerate(("human:D17-principal", "agent:D17-principal")):
            peer = await self.connect({"kind": "principal", "principal": name})
            principal = peer.core_origin["principal"]
            self.principals.append(principal)
            await self.create(f"doc/principals/delegations/k1-d17-{index}",
                              {"from": principal, "to": self.worker.core_origin["principal"], "until_ms": None})
        directory = self.ctx.root / "d17-callback"
        directory.mkdir()
        sdk = Path(__file__).resolve().parents[2] / "extensions" / "vqro_ext.py"
        if not sdk.is_file():
            raise HarnessGap("D17 callback requires the harness checkout's public Python SDK")
        shutil.copyfile(sdk, directory / "vqro_ext.py")
        script = directory / "callback.py"
        script.write_text(CALLBACK_SOURCE)
        await self.create("extension/coord", {"enabled": True, "argv": [sys.executable, str(script)]})
        try:
            async with asyncio.timeout(5):
                while True:
                    try:
                        metadata = await self.owner.call("command.invoke", {"name": "k1.d17.meta"})
                        if metadata.get("ready") is True:
                            break
                        await asyncio.sleep(.05)
                    except RpcError as error:
                        if error.kind not in ("not_found", "unavailable"):
                            raise
                        await asyncio.sleep(.05)
        except TimeoutError as error:
            raise HarnessGap("D17 supervised public callback did not register within five seconds") from error
        hello = metadata.get("hello", {})
        self.actor = hello.get("actor")
        self.transport = {"principal": hello.get("principal") or "undeclared", "actor": self.actor}
        if hello.get("initiator"):
            self.transport["initiator"] = hello["initiator"]
        check(self.actor == "extension:coord", "callback actor was not core-authenticated extension:coord", component="core")
        # Include this owned supervised child in the existing PID/birth cleanup.
        from pi_provider_e2e import identity
        self.ctx.backend.tracked.append(identity(metadata["pid"]))
        self.ctx.evidence.append({"kind": "D17_public_callback", "actor": self.actor,
                                  "principals": self.principals, "worker": self.worker.core_origin,
                                  "sdk_sha256": hashlib.sha256((directory / "vqro_ext.py").read_bytes()).hexdigest(),
                                  "fixture_sha256": hashlib.sha256(CALLBACK_SOURCE.encode()).hexdigest(),
                                  "sender_scope": "extension-asserted input; upstream inbox persistence not certified"})

    def expected(self, principal):
        return {"principal": principal, "actor": self.actor, "initiator": self.worker.core_origin["principal"]}

    async def forward(self, request, principal):
        wrapper = await self.worker.call("command.invoke", {"name": "k1.d17.forward", "on_behalf_of": principal,
                                                            "args": {"request": request}}, timeout=3)
        callback = wrapper.get("callback", {})
        check(callback.get("principal") == principal and callback.get("initiator") == self.worker.core_origin["principal"],
              "public callback lost delegated principal/initiator", component="core")
        return rpc_result(wrapper["rpc_error"]) if "rpc_error" in wrapper else wrapper["result"]

    async def autonomous(self, request):
        wrapper = await self.owner.call("command.invoke", {"name": "k1.d17.autonomous", "args": {"request": request}}, timeout=3)
        return wrapper["result"]

    async def close(self):
        try:
            for key, value in reversed(self.owned):
                row = await self.owner.call("workspace.get", {"key": key})
                check(row is not None and row.get("value") == value, "D17 fixture entry changed before cleanup", component="harness")
                await self.owner.call("workspace.apply", {"ops": [{"op": "delete", "key": key, "expect": row["version"]}]})
        finally:
            for peer in reversed(self.peers):
                await peer.close()


@asynccontextmanager
async def public_origin_route(ctx):
    route = OriginRoute(ctx)
    try:
        await route.start()
        yield route
    finally:
        await route.close()


async def send_request(ctx, origin, render):
    facts = await ctx.ensure_session()
    declaration = facts.get("caps", {}).get("agent.send", {})
    mode = next((mode for mode in ("start", "admit") if mode in declaration.get("modes", {})), None)
    if mode is None:
        raise HarnessGap("D17 needs a declared start/admit send mode")
    return {"resource": ctx.agents[0]["id"], "op": "agent.send", "deadline_ms": 1000,
            "args": {"v": 0, "epoch": facts["epoch"], "caps_rev": facts["caps"]["rev"], "mode": mode,
                     "delivery": ctx.delivery(), "origin": deepcopy(origin), "render": render,
                     "content": [{"type": "text", "text": "D17-body:" + ctx.delivery()}]}}


async def rejected_origin(ctx, invoke, request, kind):
    request = await refresh_request(ctx, request)
    mark = ctx.native.mark()
    result = await invoke(request)
    rejection(result, kinds=(kind,))
    audit = (await final_audit(ctx, result))["data"]
    await model_cut(ctx, "D17-rejection-fence")
    check(not ctx.native.writes(mark), "rejected origin reached native input")
    ctx.evidence.append({"kind": "D17_prewrite_rejection", "error_kind": kind, "native_mark": mark,
                         "origin": request["args"]["origin"], "effect_audit": audit,
                         "error": result.get("error")})


async def inserted_origin(ctx, invoke, request, expected, rendering, *, sender=None):
    request = await refresh_request(ctx, request)
    before = await model_cut(ctx, "D17-before")
    mark = ctx.native.mark()
    result = await invoke(request)
    receipt = accepted(result)
    check(receipt["delivery"] == request["args"]["delivery"], "origin receipt belongs to another delivery")
    audit = (await final_audit(ctx, result))["data"]
    for field in ("principal", "actor", "initiator"):
        check((audit.get(field) or ("undeclared" if field != "initiator" else None)) == expected.get(field),
              "effect audit differs from actual core " + field, component="core")
    bound_origin(receipt, expected, request["args"]["origin"])
    await ctx.native.wait(lambda rows: any(row["kind"] == "inserted" and row["seq"] > mark for row in rows))
    check(len(ctx.native.records("input_received", after=mark)) == 1 and len(ctx.native.records("inserted", after=mark)) == 1,
          "origin-bearing input was never inserted or was duplicated")
    after = await model_cut(ctx, "D17-after")
    message = new_model_message(before, after, request["args"]["content"], sender_header=True)
    model_origin(message, rendering, expected, request["args"]["origin"], sender=sender)
    ctx.evidence.append({"kind": "D17_model_origin", "origin": receipt["origin"], "effect_audit": audit,
                         "delivery": receipt["delivery"],
                         "native_message": message, "model_seq": after["seq"]})


@case("D17")
async def core_bound_origin(ctx):
    declaration = await caps(ctx, "agent.send")
    renderings = {row["as"]: row for row in declaration.get("origin", [])}
    selected = next((name for name in ("participant", "attributed_user") if name in renderings), None)
    if selected is None:
        raise HarnessGap("D17 needs a declared framed participant/attributed-user fixture")
    render = [selected]
    async with public_origin_route(ctx) as route:
        principal = route.principals[0]
        expected = route.expected(principal)
        async def forwarded(request):
            return await route.forward(request, principal)
        origin = {**expected, "kind": "agent", "agent": "D17-asserted-agent"}
        # Distinct valid core-bound identities, followed by wrong-type refusals.
        for field, value in (("actor", expected["initiator"]), ("principal", "D17-spoof"),
                             ("initiator", principal)):
            await rejected_origin(ctx, forwarded, await send_request(ctx, {**origin, field: value}, render), "forbidden")
        for field in ("principal", "actor", "initiator"):
            await rejected_origin(ctx, forwarded, await send_request(ctx, {**origin, field: 17}, render), "invalid")
        await inserted_origin(ctx, forwarded, await send_request(ctx, origin, render), expected, renderings[selected])
        # Omitted IDs are derived from core, never from a principal's spelling.
        for principal in route.principals:
            async def forwarded(request):
                return await route.forward(request, principal)
            expected = route.expected(principal)
            await inserted_origin(ctx, forwarded, await send_request(ctx, {"kind": "agent"}, render), expected, renderings[selected])
        # A kind label alone is not owner proof, even if user rendering is listed.
        await rejected_origin(ctx, forwarded, await send_request(ctx, {"kind": "human"}, ["user"]), "unsupported")
        request = await send_request(ctx, {"kind": "agent"}, render)
        mark = ctx.native.mark()
        probes = await route.owner.call("command.invoke", {"name": "k1.d17.probe", "args": {"request": request, "principal": principal}})
        for name in ("stale", "standing"):
            core_forbidden(probes.get(name, {}))
        await model_cut(ctx, "D17-proof-fence")
        check(not ctx.native.writes(mark), "expired callback proof or standing delegation reached native input")
        ctx.evidence.append({"kind": "D17_proof_scope", "stale": probes["stale"], "standing": probes["standing"]})
        # No hello/identity: recovery calls carry no implicit owner identity.
        unknown = await route.connect()
        async def direct(request):
            try:
                return await unknown.call("resource.invoke", request)
            except RpcError as error:
                return rpc_result(error.error)
        await rejected_origin(ctx, direct, await send_request(ctx, {"kind": "human", "principal": route.owner.core_origin["principal"]}, render), "forbidden")
        await inserted_origin(ctx, direct, await send_request(ctx, {"kind": "human"}, render),
                              {"principal": "undeclared", "actor": "undeclared"}, renderings[selected])
        sender = {"principal": "D17-original-inbox-sender", "kind": "human", "provenance": "inbox-record"}
        await inserted_origin(ctx, route.autonomous, await send_request(ctx, {"kind": "agent", "sender": sender}, render),
                              route.transport, renderings[selected], sender=sender)
