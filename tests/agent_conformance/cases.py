"""Part A public-wire cases. No runtime names or native command spellings here."""
import asyncio
from copy import deepcopy
import json

from .content import compare_content, examples
from .results import CheckFailed, FixtureFailed, HarnessGap, NotDeclared, check, declared

HANDLERS = {}


def case(ident):
    def registered(function):
        HANDLERS[ident] = function
        return function
    return registered


def integer(value):
    return isinstance(value, int) and not isinstance(value, bool) and value >= 0


def identifier(value):
    return isinstance(value, str) and 0 < len(value.encode()) <= 128


def restriction(lower, upper, path=""):
    """Provider enum sets, numeric maxima, boolean admission and subobjects."""
    if lower is None:
        return True
    if isinstance(lower, dict) and isinstance(upper, dict):
        return all(key in upper and restriction(value, upper[key], path + "." + key) for key, value in lower.items())
    if isinstance(upper, list):
        values = lower if isinstance(lower, list) else [lower]
        def member(value, candidate):
            if isinstance(value, (dict, list)):
                return restriction(value, candidate, path)
            return type(value) is type(candidate) and value == candidate
        return all(any(member(value, candidate) for candidate in upper) for value in values)
    if isinstance(lower, bool) and isinstance(upper, bool):
        return not lower or upper
    if integer(lower) and integer(upper):
        return lower <= upper
    if isinstance(lower, str):
        return False  # provider enum values must be sets, never scalar equality
    return type(lower) is type(upper) and lower == upper


def rejection(result, *, kinds=("unsupported", "invalid", "stale")):
    check(result.get("outcome") == "rejected", f"not a definite rejection: {result}")
    error = result.get("error", {})
    kind = (error.get("data") or {}).get("kind", error.get("kind"))
    check(kind in kinds, f"wrong rejection kind {kind!r}; expected {kinds}")


def accepted(result):
    check(result.get("outcome") == "accepted", f"input not acknowledged: {result}")
    receipt = result.get("data", {}).get("receipt")
    check(isinstance(receipt, dict) and identifier(receipt.get("delivery")), "acknowledgement lacks delivery receipt")
    return receipt


async def caps(ctx, operation):
    return declared((await ctx.facts()).get("caps", {}), operation)


async def mode_caps(ctx, mode):
    send = await caps(ctx, "agent.send")
    if mode not in send.get("modes", {}):
        raise NotDeclared("agent.send.modes." + mode, send.get("modes"))
    return send["modes"][mode]


@case("C1")
async def declarations(ctx):
    upper = ctx.provider.get("capabilities", {})
    agent = upper.get("agent")
    check(isinstance(agent, dict) and type(agent.get("v")) is int and agent["v"] == 0, "provider did not declare agent {v:0,modules}")
    modules = agent.get("modules")
    check(isinstance(modules, dict), "provider agent.modules is not an object map")
    known_modules = {"queue", "sessions", "config", "history", "children", "terminals", "stop"}
    for name, module in modules.items():
        check(name in known_modules or name.startswith("_"), "unknown non-namespaced module " + name)
        check(isinstance(module, dict) and integer(module.get("v")), "module needs an explicit version: " + name)
    check("items.stream" in upper, "structured adapter did not declare mandatory item observation")
    check(ctx.provider.get("host") == ctx.host, "provider not bound to its actual private execution host")
    for resource in ctx.agents:
        facts = resource.get("data", {}).get("agent", {})
        check(type(facts.get("v")) is int and facts["v"] == 0, "resource lacks data.agent.v:0")
        check(len(json.dumps(facts, ensure_ascii=False, separators=(",", ":")).encode()) <= 64 << 10, "agent facts exceed 64 KiB")
        check(identifier(facts.get("epoch")), "missing/oversized observation epoch")
        check(isinstance(facts.get("state"), str) and isinstance(facts.get("lifecycle"), str), "missing state/lifecycle")
        state = facts["state"]
        coarse = "error" if state == "idle" and (facts.get("completion") or {}).get("reason") == "error" else {
            "idle": "idle", "running": "working", "requires_action": "blocked"}.get(state, "working")
        check(isinstance(resource.get("status"), dict) and resource["status"].get("state") == coarse,
              "coarse status disagrees with native-evidence facts")
        check(isinstance(facts.get("incomplete"), list), "facts need explicit incomplete list")
        host = facts.get("host", {})
        check(identifier(host.get("id")) and isinstance(host.get("kind"), str) and isinstance(host.get("owns_lifecycle"), bool), "invalid host declaration")
        lower = facts.get("caps", {})
        check(isinstance(lower, dict) and integer(lower.get("rev")), "missing capability revision")
        check("items.stream" in lower, "resource omitted mandatory item observation")
        host_caps = lower.get("host", {})
        check(isinstance(host_caps, dict) and isinstance(host_caps.get("kind"), str)
              and isinstance(host_caps.get("endpoints"), str), "missing declared host kind/topology")
        check(host_caps["kind"] == host["kind"], "host facts disagree with host capability kind")
        if "agent.respond" in lower:
            respond = declared(lower, "agent.respond")
            check(isinstance(respond.get("methods"), list) and isinstance(respond.get("scopes"), list)
                  and isinstance(respond.get("background_requests"), bool), "requests need methods/scopes/background declaration")
        for name, value in lower.items():
            if name == "rev":
                continue
            check(name in upper and restriction(value, upper[name], name), f"resource capability exceeds provider declaration: {name}")
        if "agent.send" in lower:
            send = declared(lower, "agent.send")
            check(isinstance(send.get("modes"), dict) and send["modes"], "send.modes must be nonempty")
            content = send.get("content", {})
            check(isinstance(content, dict) and isinstance(content.get("types"), list)
                  and all(isinstance(tag, str) for tag in content["types"])
                  and isinstance(content.get("shape"), str), "invalid typed-content shape declaration")
            check(isinstance(send.get("guards"), dict), "missing guard-strength declaration")
            check(integer(send.get("receipts", {}).get("retain")) and send["receipts"]["retain"] >= 256, "receipt retention below 256")
            for field in ("correlation", "literal", "handled"):
                check(isinstance(send.get(field), str), "missing send." + field)
            renderings = send.get("origin")
            check(isinstance(renderings, list) and all(isinstance(item, dict) and isinstance(item.get("as"), str)
                  and isinstance(item.get("role"), str) for item in renderings), "origin requires rendering descriptors, not strings")
            for mode, profile in send["modes"].items():
                check(isinstance(profile, dict), f"invalid mode profile: {mode}")
                if mode == "start":
                    check(profile.get("busy") == "reject", "start must reject busy work")
                    if send["guards"].get("run") != "runtime":
                        check(profile.get("preflight") == "serialized", "non-atomic start needs serialized preflight")
                if mode in ("boundary", "settle"):
                    check(isinstance(profile.get("point"), str) and isinstance(profile.get("batch"), str)
                          and isinstance(profile.get("idle"), str), f"incomplete {mode} edge declaration")
    ctx.evidence.append({"kind": "capability_maps", "provider": upper,
                         "resources": [{"resource": r["id"], "caps": r["data"]["agent"]["caps"]} for r in ctx.agents]})


@case("C2")
async def preconditions(ctx):
    fact = await ctx.facts()
    operations = [name for name in fact.get("caps", {}) if name.startswith("agent.") and name != "agent.receipt"]
    if not operations:
        mark = ctx.native.mark()
        rejection(await ctx.invoke("agent._conformance_undeclared"))
        check(not ctx.native.writes(mark), "undeclared operation wrote to native runtime")
        return
    if "agent.send" in operations:
        op = "agent.send"
        modes = fact["caps"][op]["modes"]
        mode = next(iter(modes))
        if not (fact.get("session") or {}).get("id"):
            raise HarnessGap("C2 needs a live session before its otherwise-valid mutation probes")
        base = {"mode": mode, "content": [{"type": "text", "text": "must-not-write"}],
                "delivery": ctx.delivery(), "origin": {"principal": "owner", "actor": "test", "kind": "human"},
                "render": ["user", "participant", "attributed_user"]}
        unknown = {"mode": "_conformance_unknown"}
    elif "agent.interrupt" in operations:
        op, base, unknown = "agent.interrupt", {}, None
    else:
        raise HarnessGap("C2 needs a valid argument factory for the declared non-send/non-interrupt operation")
    invalids = [{"v": 1}, {"v": False}, {"v": None}, {"epoch": "foreign-conformance-epoch"}, {"caps_rev": fact["caps"]["rev"] + 1}]
    if unknown:
        invalids.append(unknown)
    for invalid in invalids:
        mark = ctx.native.mark()
        result = await ctx.invoke(op, {**base, **invalid})
        rejection(result)
        check(not ctx.native.writes(mark), f"invalid precondition performed native control: {invalid}")
    ctx.evidence.append({"kind": "native_write_count", "invalid_calls": len(invalids), "writes": 0})


@case("C3")
async def state_evidence(ctx):
    await ctx.native.call("turn", state="start")
    running = await ctx.wait_facts(lambda f: f.get("state") == "running")
    previous = running.get("completion")
    await ctx.native.call("turn", state="end")
    await asyncio.sleep(.5)  # deliberate fixture silence, not an insertion oracle
    middle = await ctx.facts()
    check(middle.get("state") != "idle", "agent_end/silence fabricated idle before native settlement")
    check(middle.get("completion") == previous, "low-level end fabricated a foreground completion")
    await ctx.native.call("turn", state="settled")
    settled = await ctx.wait_facts(lambda f: f.get("state") == "idle" and f.get("completion") != previous)
    check(identifier((settled.get("completion") or {}).get("id")), "native settlement lacks fresh completion identity")
    ctx.evidence.append({"kind": "silence_window_ms", "value": 500})


@case("C4")
async def lost_reply_no_replay(ctx):
    await ctx.native.call("pause_after", phase="acceptance")
    caller = await ctx.viewer()
    mark, pid = ctx.native.mark(), ctx.native.state["pid"]
    pending = asyncio.create_task(ctx.send("C4-lost-public-reply", peer=caller, deadline_ms=1200))
    try:
        await ctx.native.wait(lambda rows: any(row["seq"] > mark and row["kind"] == "paused" and row.get("phase") == "acceptance" for row in rows))
        await caller.close()
        await asyncio.gather(pending, return_exceptions=True)
        await ctx.native.call("release", phase="acceptance")
        await ctx.native.wait(lambda rows: any(row["seq"] > mark and row["kind"] == "input_received" for row in rows))
    finally:
        await ctx.native.call("release", phase="all")
        if not pending.done():
            pending.cancel()
        await asyncio.gather(pending, return_exceptions=True)
    await ctx.restart_core()
    await ctx.wait_facts(lambda f: f.get("state") == "running")
    check((await ctx.native.call("state"))["pid"] == pid, "lost-reply recovery replaced native execution")
    check(len([row for row in ctx.native.writes(mark) if row.get("operation") == "send"]) == 1,
          "public reply loss replayed native input")
    await ctx.native.call("drop_reply", operation="send")
    mark = ctx.native.mark()
    uncertain = await ctx.send("C4-lost-native-reply", "admit", deadline_ms=120)
    check(uncertain.get("outcome") in ("dispatched", "indeterminate"), "missing native reply became definite acceptance/rejection")
    await ctx.native.wait(lambda rows: any(row["seq"] > mark and row["kind"] == "input_received" for row in rows))
    await ctx.restart_core()
    await ctx.wait_facts(lambda f: f.get("state") == "running")
    check(len([row for row in ctx.native.writes(mark) if row.get("operation") == "send"]) == 1,
          "uncertain native send was automatically resent")
    ctx.evidence.append({"kind": "lost_reply", "public_and_native": True, "native_send_writes_each": 1,
                         "uncertain_result": uncertain})


@case("C5")
async def post_acceptance_failure(ctx):
    result = await ctx.send("C5-accepted-input")
    receipt = accepted(result)
    await ctx.native.call("failure", message="C5-native-error-after-acceptance")
    failed = await ctx.wait_facts(lambda f: (f.get("completion") or {}).get("reason") == "error")
    check(failed.get("lifecycle") != "ended", "turn failure incorrectly ended endpoint")
    check(result["outcome"] == "accepted", "earlier accepted input was retroactively rejected")
    ctx.evidence.append({"kind": "post_acceptance_error", "receipt": receipt, "completion": failed["completion"]})


@case("C7")
async def restart_no_replay(ctx):
    rid, before = ctx.agents[0]["id"], await ctx.resource()
    await ctx.native.call("turn", state="start")
    await ctx.wait_facts(lambda f: f.get("state") == "running")
    mark, pid = ctx.native.mark(), ctx.native.state["pid"]
    await ctx.restart_core()
    await ctx.wait_facts(lambda f: f.get("state") == "running", rid)
    after = await ctx.resource(rid)
    check(after["id"] == rid and after["instance"] == before["instance"], "core restart replaced the exact endpoint")
    check((await ctx.native.call("state"))["pid"] == pid, "core restart replaced native execution")
    check(not ctx.native.writes(mark), "native input/control was replayed across core restart")
    ctx.evidence.append({"kind": "reconnect", "resource": rid, "same_native_pid": True, "effect_writes": 0})


@case("C8")
async def literal_text(ctx):
    declaration = await caps(ctx, "agent.send")
    if declaration.get("literal") == "none":
        raise NotDeclared("agent.send.literal", "none")
    for text in ("/fixture-template must remain literal", "literal\r\nEnter\x1b[31m\u2028snow ☃"):
        await ctx.native.call("turn", state="settled")
        await ctx.wait_facts(lambda f: f.get("state") == "idle")
        mark = ctx.native.mark()
        accepted(await ctx.send(text))
        await ctx.native.wait(lambda rows: any(r["kind"] == "input_received" and r["seq"] > mark for r in rows))
        received = ctx.native.records("input_received", after=mark)
        check(len(received) == 1 and text in received[0]["text"], "literal input changed, split or duplicated")
        check(not ctx.native.records("command_executed", after=mark), "slash text ran a native command/template")
        check(all(r.get("operation") == "send" for r in ctx.native.writes(mark)), "text caused an extra native control effect")


@case("C9")
async def qualified_causality(ctx):
    snapshot, facts = await ctx.peer.call("workspace.snapshot"), await ctx.facts()
    cause = {"kind": "effect", "id": "same-local-effect", "endpoint": ctx.agents[0]["id"],
             "epoch": facts["epoch"], "boot": snapshot["epoch"], "provenance": "asserted"}
    correlation = "C9-caller-thread"
    result = await ctx.send("C9-qualified-cause", cause=cause, correlation=correlation)
    receipt = accepted(result)
    def effect_event():
        return next((row["data"] for row in ctx.events if row.get("type") == "effect"
                     and row.get("data", {}).get("effect") == result.get("effect")), None)
    try:
        event = await ctx.peer.wait(effect_event)
    except asyncio.TimeoutError as error:
        raise CheckFailed("core did not publish the effect audit", component="core") from error
    ctx.evidence.append({"kind": "causality_echo", "expected": cause, "receipt": receipt, "effect_event": event})
    check(event.get("cause") == cause and event.get("correlation") == correlation,
          "generic core effect audit dropped caller causality", component="core")
    check(receipt.get("cause") == cause and receipt.get("correlation") == correlation,
          "adapter receipt dropped/changed asserted caller causality")
    raise HarnessGap("C9 echo exercised; dual-endpoint request and reused-effect-after-restart branches pending")


@case("C10")
async def endpoint_model(ctx):
    groups = {}
    for resource in ctx.agents:
        fact = await ctx.facts(resource["id"])
        host, declaration = fact.get("host", {}), fact.get("caps", {}).get("host", {})
        check(identifier(host.get("id")), "endpoint has no exact host identity")
        check(host.get("kind") == declaration.get("kind"), "host kind differs from per-resource declaration")
        groups.setdefault(host["id"], []).append((resource, declaration))
    for host, endpoints in groups.items():
        for _, declaration in endpoints:
            topology = declaration.get("endpoints")
            if topology == "one":
                check(len(endpoints) == 1, "one-endpoint host actually publishes multiple endpoints")
                ctx.evidence.append({"kind": "subcase", "case": "C10.shared_three_endpoints", "applicability": "not-applicable",
                                     "declaration": {"host.endpoints": "one"}, "host": host})
            elif topology == "many":
                if ctx.endpoint_count < 3:
                    raise HarnessGap("C10 many-endpoint fixture manifest must request at least three endpoints")
                check(len(endpoints) >= 3, "declared shared host did not publish three endpoints")
            else:
                raise HarnessGap("unrecognized host topology declaration: " + repr(topology))
    ctx.evidence.append({"kind": "endpoint_groups", "groups": {host: [r["id"] for r, _ in rows] for host, rows in groups.items()}})


async def mode_matrix(ctx, mode):
    profile = await mode_caps(ctx, mode)
    if mode == "preempt":
        raise HarnessGap("native preemption witness fixture not yet implemented")
    if mode in ("boundary", "settle") and profile.get("batch") not in ("one", "all"):
        raise HarnessGap("unknown native batch declaration")
    if mode == "boundary" and profile.get("point") != "after_tool_calls":
        raise HarnessGap("no timeline for declared boundary point " + repr(profile.get("point")))
    if mode == "settle" and profile.get("point") not in ("would_stop", "settled"):
        raise HarnessGap("no timeline for declared settle point " + repr(profile.get("point")))

    async def point(index):
        if mode == "boundary":
            tool = "mode-current-tool" if index == 1 else f"mode-tool-{index}"
            if index != 1:
                await ctx.native.call("tool_call", id_ref=tool, state="start")
            await ctx.native.call("tool_call", id_ref=tool, state="end")
        elif mode == "settle":
            await ctx.native.call("turn", state="end")
            if profile["point"] == "settled":
                await ctx.native.call("turn", state="settled")
            await ctx.native.call("queue", mode="after", action="consume")
            await ctx.native.call("turn", state="start")
        await ctx.native.call("model_call", id_ref=f"mode-model-{index}")

    await ctx.native.call("turn", state="settled")
    await ctx.wait_facts(lambda f: f.get("state") == "idle")
    mark = ctx.native.mark()
    idle = await ctx.send("mode-idle-" + mode, mode)
    edge = profile.get("idle", "start")
    if edge == "reject":
        rejection(idle, kinds=("busy", "unsupported"))
        check(not ctx.native.writes(mark), "idle rejection performed native control")
    elif edge in ("start", "park"):
        accepted(idle)
        await ctx.native.wait(lambda rows: any(r["seq"] > mark and r["kind"] == "input_received" for r in rows))
        if edge == "park":
            check(not (await ctx.native.call("state"))["active"], "idle park started work")
            check(not ctx.native.records("inserted", after=mark), "idle park inserted immediately")
            await ctx.native.call("turn", state="start")
            await point(0)
        else:
            await ctx.native.wait(lambda rows: any(r["seq"] > mark and r["kind"] == "inserted" for r in rows))
    else:
        raise HarnessGap("unknown idle edge " + repr(edge))

    await ctx.native.call("turn", state="settled")
    await ctx.wait_facts(lambda f: f.get("state") == "idle")
    await ctx.native.call("turn", state="start")
    await ctx.wait_facts(lambda f: f.get("state") == "running")
    if mode == "boundary":
        await ctx.native.call("tool_call", id_ref="mode-current-tool", state="start")
    mark = ctx.native.mark()
    if mode == "start" or profile.get("busy") == "reject":
        rejection(await ctx.send("mode-busy-rejected", mode), kinds=("busy", "unsupported"))
        check(not ctx.native.writes(mark), "busy rejection wrote to native runtime")
        return
    count = 3 if mode in ("boundary", "settle") else 1
    for index in range(count):
        accepted(await ctx.send(f"mode-busy-{mode}-{index}", mode))
    check(all(row.get("operation") == "send" for row in ctx.native.writes(mark)), "mode used an undeclared composition/fallback control")
    if mode == "admit":
        check(len(ctx.native.records("input_received", after=mark)) == 1, "admit duplicated/lost native acceptance")
        return
    check(not ctx.native.records("inserted", after=mark), "precise mode inserted before its native point")
    remaining, step = count, 0
    while remaining:
        before = ctx.native.mark()
        await point(step + 1)
        inserted = ctx.native.records("inserted", after=before)
        expected = min(remaining, 1 if profile["batch"] == "one" else count)
        check(len(inserted) == expected, "native consume batch differs from declaration")
        model = ctx.native.records("model_call", after=before)[-1]
        check(all(row["seq"] < model["seq"] for row in inserted), "input arrived after its promised model boundary")
        if mode == "boundary":
            tool = ctx.native.records("tool_call", after=before)[-1]
            check(all(tool["seq"] < row["seq"] for row in inserted), "boundary input arrived before tool completion")
        remaining -= expected
        step += 1
    check(len(ctx.native.records("input_received", after=mark)) == count, "mode acceptance count changed")
    ctx.evidence.append({"kind": "mode_edges", "mode": mode, "idle": edge, "batch": profile.get("batch"), "points": step})


@case("D1")
async def start_edges(ctx):
    await mode_matrix(ctx, "start")


@case("D2")
async def admit_edges(ctx):
    await mode_matrix(ctx, "admit")


@case("D3")
async def boundary_edges(ctx):
    await mode_matrix(ctx, "boundary")


@case("D4")
async def settle_edges(ctx):
    await mode_matrix(ctx, "settle")


@case("D5")
async def preempt_edges(ctx):
    await mode_matrix(ctx, "preempt")


@case("D6")
async def unsupported_content(ctx):
    send = await caps(ctx, "agent.send")
    declaration = send["content"]
    types, shape, blocks = declaration["types"], declaration["shape"], examples()
    framed = any(item.get("frame") == "sender_header" for item in send.get("origin", []))
    invalid = [[{"type": "_conformance_undeclared", "data": "not-a-text-fallback"}],
               "a string is not a block array", {"type": "text", "text": "one object is not an array"}]
    invalid += [[block] for tag, block in blocks.items() if tag not in types]
    if shape == "text_then_media":
        invalid += [[blocks["image"], blocks["text"]], [blocks["text"], blocks["image"], blocks["text"]],
                    [blocks["text"], blocks["text"]]]
    elif shape != "ordered":
        raise HarnessGap("no content-shape scenario for " + repr(shape))
    for content in invalid:
        mark = ctx.native.mark()
        rejection(await ctx.send("unused", content=content))
        check(not ctx.native.writes(mark), "unsupported content/ordering reached the native runtime")
    positive = []
    for tag in types:
        if tag not in blocks:
            raise HarnessGap("declared content tag lacks a fixture: " + tag)
        positive.append([blocks[tag]])
    if shape == "ordered" and "text" in types:
        positive.append([blocks["text"], {"type": "text", "text": "D6-second-separate-block"}])
    if "text" in types and "image" in types:
        positive.append([blocks["text"], blocks["image"]])
        if shape == "ordered":
            positive.append([blocks["image"], blocks["text"]])
    for content in positive:
        await ctx.native.call("turn", state="settled")
        await ctx.wait_facts(lambda f: f.get("state") == "idle")
        mark = ctx.native.mark()
        accepted(await ctx.send("unused", content=content))
        await ctx.native.wait(lambda rows: any(r["seq"] > mark and r["kind"] == "input_received" for r in rows))
        received = ctx.native.records("input_received", after=mark)
        check(len(received) == 1, "valid content duplicated or lost")
        actual = received[0].get("content")
        if actual is None:
            raise FixtureFailed("native wire driver lacks a typed-content ledger oracle")
        compare_content(content, actual, sender_header=framed)
    ctx.evidence.append({"kind": "content_matrix", "shape": shape, "types": types,
                         "valid_arrays": len(positive), "rejected_arrays": len(invalid)})


@case("D7")
async def admission_not_insertion(ctx):
    declaration = await caps(ctx, "agent.send")
    await ctx.native.call("pause_before", phase="insertion")
    result = await ctx.send("D7-unique-uninserted")
    receipt = accepted(result)
    check(receipt.get("stage") == "admitted", f"weak native acknowledgement overclaimed stage: {receipt}")
    check(not ctx.native.records("inserted"), "insertion barrier failed", component="fixture")
    await ctx.native.call("release", phase="insertion")
    await ctx.native.wait(lambda rows: any(r["kind"] == "inserted" for r in rows))
    view = await ctx.attach()
    await ctx.wait_view(view, lambda reducer: reducer.snapshots > 0)
    if declaration.get("correlation") in ("none", "acceptance"):
        messages = [entity["value"] for (kind, _), entity in view.reducer.entities.items() if kind == "message"]
        check(any("D7-unique-uninserted" in json.dumps(value) for value in messages), "inserted user message not observed")
        check(all("delivery" not in value for value in messages if value.get("role") == "user"),
              "delivery inferred from timing/text without native insertion correlation")
    ctx.evidence.append({"kind": "native_input_count", "value": len(ctx.native.records("input_received"))})


@case("D8")
async def native_keys(ctx):
    declaration = await caps(ctx, "agent.send")
    if declaration.get("keys") is None:
        mark = ctx.native.mark()
        rejection(await ctx.send("D8-unsupported-key", key="native-key-not-declared"), kinds=("unsupported",))
        check(not ctx.native.writes(mark), "unsupported key reached native runtime")
        raise NotDeclared("agent.send.keys", None)
    raise HarnessGap("native-key retention/restart fixture is not yet implemented")


@case("D9")
async def consume_settings(ctx):
    await caps(ctx, "agent.configure")
    raise HarnessGap("consume-time setting transition matrix not yet implemented")


@case("D10")
async def interrupt_pending(ctx):
    declaration = await caps(ctx, "agent.interrupt")
    await ctx.native.call("turn", state="settled")
    before = await ctx.wait_facts(lambda f: f.get("state") == "idle")
    mark = ctx.native.mark()
    noop = await ctx.invoke("agent.interrupt")
    check(noop.get("outcome") in ("accepted", "observed"), "idle interrupt was not a no-op acknowledgement")
    check(not ctx.native.records("input_received", after=mark), "idle interrupt sent content")
    check(not (await ctx.native.call("state"))["active"], "idle interrupt started native work")
    await ctx.native.call("turn", state="start")
    await ctx.wait_facts(lambda f: f.get("state") == "running")
    send = before["caps"].get("agent.send", {})
    for mode in ("boundary", "settle"):
        if mode in send.get("modes", {}):
            accepted(await ctx.send("D10-held-" + mode, mode))
    mark = ctx.native.mark()
    result = await ctx.invoke("agent.interrupt")
    check(result.get("outcome") in ("accepted", "observed"), "interrupt was not acknowledged")
    if declaration.get("waits") == "idle":
        check(result["outcome"] == "observed", "waits:idle returned without observed settlement")
    await ctx.wait_facts(lambda f: f.get("state") == "idle")
    check(all(row.get("operation") == "interrupt" for row in ctx.native.writes(mark)),
          "interrupt silently cleared queues, sent content or ran a composition")
    if declaration.get("pending") == "unknown":
        pending = (result.get("data") or {}).get("pending")
        check(pending in (None, "unknown"), "pending:unknown was upgraded to a definite disposition")
    ctx.evidence.append({"kind": "interrupt_pending", "declaration": declaration,
                         "native_state": await ctx.native.call("state"),
                         "inserted_during_abort": ctx.native.records("inserted", after=mark)})


@case("D11")
async def guarded_input(ctx):
    declaration = await caps(ctx, "agent.send")
    guards = declaration.get("guards", {})
    facts = await ctx.facts()
    for name in ("session", "run"):
        strength = guards.get(name, "none")
        mark = ctx.native.mark()
        result = await ctx.send("D11-stale-" + name, guard={name: "definitely-stale-guard"})
        rejection(result, kinds=("unsupported",) if strength == "none" else ("stale",))
        check(not ctx.native.writes(mark), "locally stale/unsupported guard wrote to native runtime")
    if guards.get("run") != "runtime":
        ctx.evidence.append({"kind": "subcase", "case": "D11.atomic_native_race", "applicability": "not-applicable",
                             "declaration": {"guards.run": guards.get("run", "none")}})
        return
    await ctx.native.call("turn", state="start")
    active = await ctx.wait_facts(lambda f: f.get("state") == "running" and f.get("run"))
    run = active["run"]
    check(identifier(run), "run guard target is not an opaque run id")
    await ctx.native.call("pause_before", phase="acceptance")
    mark = ctx.native.mark()
    task = asyncio.create_task(ctx.send("D11-native-race", "admit", guard={"run": run}, deadline_ms=1500))
    try:
        await ctx.native.wait(lambda rows: any(row["seq"] > mark and row["kind"] == "paused" and row.get("phase") == "acceptance" for row in rows))
        await ctx.native.call("turn", state="settled")
        await ctx.native.call("turn", state="start")
        await ctx.native.call("release", phase="acceptance")
        rejection(await task, kinds=("stale",))
        check(not ctx.native.records("input_received", after=mark), "atomic native run guard admitted input into newer work")
    finally:
        await ctx.native.call("release", phase="all")
        if not task.done():
            task.cancel()
        await asyncio.gather(task, return_exceptions=True)


@case("D12")
async def idle_park(ctx):
    declaration = await caps(ctx, "agent.send")
    modes = [mode for mode, profile in declaration["modes"].items() if profile.get("idle") == "park"]
    if not modes:
        raise NotDeclared("agent.send.modes.*.idle", declaration["modes"])
    await ctx.native.call("turn", state="settled")
    await ctx.wait_facts(lambda f: f.get("state") == "idle")
    for mode in modes:
        mark = ctx.native.mark()
        accepted(await ctx.send("D12-park-" + mode, mode))
        state = await ctx.native.call("state")
        check(not state["active"], "idle park started native work")
        check(not ctx.native.records("inserted", after=mark), "parked input was inserted without a later start/boundary")
        check((await ctx.facts()).get("state") != "running", "park acknowledgement fabricated a run start")


@case("D14")
async def consumed_input(ctx):
    await caps(ctx, "agent.send")
    before = await ctx.facts()
    receipt = accepted(await ctx.send("D14-hook-consumed"))
    check(receipt.get("stage") == "admitted", "hook-consumed weak ACK fabricated handled/inserted")
    check(len(ctx.native.records("hook_consumed")) == 1, "fixture did not consume the input", component="fixture")
    check(not ctx.native.records("inserted"), "hook-consumed input was durably inserted", component="fixture")
    await asyncio.sleep(.25)
    after = await ctx.facts()
    check(after.get("run") == before.get("run") and after.get("completion") == before.get("completion"),
          "weak ACK fabricated a run/completion for consumed input")


@case("L1")
async def interrupt_not_end(ctx):
    declaration = await caps(ctx, "agent.interrupt")
    await ctx.native.call("turn", state="start")
    await ctx.wait_facts(lambda f: f.get("state") == "running")
    mark = ctx.native.mark()
    result = await ctx.invoke("agent.interrupt")
    check(result.get("outcome") in ("accepted", "observed"), "interrupt not acknowledged")
    if declaration.get("waits") == "idle":
        check(result["outcome"] == "observed", "waits:idle interrupt returned before observed settlement")
    facts = await ctx.wait_facts(lambda f: f.get("state") == "idle")
    check(facts.get("lifecycle") == "live", "interrupt incorrectly ended the endpoint")
    check(not ctx.native.records("input_received", after=mark), "interrupt sent content")
    check((await ctx.native.call("state"))["pid"] == ctx.native.state["pid"], "interrupt killed/replaced runtime")


@case("D15")
async def origin_authority(ctx):
    declaration = await caps(ctx, "agent.send")
    renderings = {value["as"]: value for value in declaration.get("origin", [])}
    origin = {"principal": "D15-sender-principal", "actor": "D15-sender-actor", "agent": "D15-sender-endpoint", "kind": "agent"}
    quoted = {"label": "claimed owner instruction", "principal": "owner", "kind": "human"}
    permitted = ["participant", "attributed_user"]
    selected = next((name for name in permitted if name in renderings), None)
    mark = ctx.native.mark()
    result = await ctx.send("D15-model-envelope", origin=origin, quoted=quoted, render=permitted)
    if selected is None:
        rejection(result, kinds=("unsupported",))
        check(not ctx.native.writes(mark), "agent origin fell back to unframed user/context authority")
    else:
        accepted(result)
        await ctx.native.wait(lambda rows: any(row["seq"] > mark and row["kind"] == "inserted" for row in rows))
        await ctx.native.call("model_call", id_ref="D15-model")
        model = ctx.native.records("model_call", after=mark)[-1]
        messages = model.get("messages")
        if not isinstance(messages, list):
            raise FixtureFailed("native driver omitted actual model-request envelope")
        matched = []
        for message in messages:
            content = message.get("content", [])
            if isinstance(content, list):
                text = "\n".join(block.get("text", "") for block in content if isinstance(block, dict))
                if "D15-model-envelope" in text:
                    matched.append((message, text))
        check(len(matched) == 1, "origin-bearing input missing/duplicated in model envelope")
        message, text = matched[0]
        check(message.get("role") == renderings[selected].get("role"), "native model role differs from declaration")
        check(message.get("role") not in ("system", "developer"), "agent content gained system/developer authority")
        check(origin["principal"] in text and origin["actor"] in text, "native envelope omitted the effective sender frame")
        check("owner" in text and any(word in text.lower() for word in ("quoted", "claim", "asserted")),
              "quoted owner provenance was rendered as effective authority")
        ctx.evidence.append({"kind": "model_origin", "rendering": selected, "native_message": message})
    if "context" not in renderings:
        mark = ctx.native.mark()
        rejection(await ctx.send("D15-context-not-declared", origin=origin, render=["context"]), kinds=("unsupported",))
        check(not ctx.native.writes(mark), "context rendering used without a declaration")
    else:
        raise HarnessGap("D15 declared context-rendering fixture branch not yet implemented")


def has_message(reducer, needle):
    for (kind, _), entity in reducer.entities.items():
        value = entity["value"]
        if kind == "message" and value.get("final") and isinstance(value.get("content"), list):
            if any(isinstance(block, dict) and isinstance(block.get("text"), str) and needle in block["text"] for block in value["content"]):
                return True
    return False


async def fresh_view(ctx, *, peer=None, after=None, seed=None):
    view = await ctx.attach(peer=peer, after=after, seed=seed)
    if view.initial == "reset":
        baseline = seed.snapshots if seed is not None else 0
        await ctx.wait_view(view, lambda reducer: reducer.snapshots > baseline)
    elif seed is None:
        raise CheckFailed("fresh/foreign attach silently continued without a snapshot")
    return view


@case("O1")
async def item_algebra(ctx):
    view = await fresh_view(ctx)
    baseline = deepcopy(view.reducer)
    text = "O1-stream-αβ-tail"
    await ctx.native.call("turn", state="start")
    await ctx.native.call("retain", count=2, text=text, chunks=["O1-stream-", "αβ", "-tail"])
    await ctx.wait_view(view, lambda reducer: has_message(reducer, "1:" + text))
    other = await fresh_view(ctx, peer=await ctx.viewer())
    cut = other.reducer.sequence
    await ctx.wait_view(view, lambda reducer: reducer.sequence >= cut)
    folded = deepcopy(baseline)
    records = [row for row in view.reducer.trace if integer(row.get("seq")) and baseline.sequence < row["seq"] <= cut]
    for row in records:
        folded.accept(row)
    if folded.sequence != cut:
        raise HarnessGap("bounded oracle trace did not retain the selected snapshot cut")
    check(folded.entities == other.reducer.entities, "live algebra and authoritative snapshot disagree at the same cut")
    ctx.evidence.append({"kind": "item_algebra", "cut": cut, "ops": sorted({row["op"] for row in records}),
                         "append_base_validation": "independent reducer; wrong bases never applied"})


@case("O4")
async def cursor_resets(ctx):
    initial = await fresh_view(ctx)
    old = deepcopy(initial.reducer.cursor)
    foreign = await fresh_view(ctx, peer=await ctx.viewer(), after={"epoch": "foreign-item-epoch", "seq": 0})
    check(foreign.initial == "reset", "foreign epoch silently joined a live stream")
    await ctx.native.call("turn", state="start")
    await ctx.native.call("retain", count=300, text="O4-retention-" + "x" * 1024)
    await ctx.wait_view(initial, lambda reducer: has_message(reducer, "299:O4-retention-"))
    reset = await fresh_view(ctx, peer=await ctx.viewer(), after=old)
    check(reset.initial == "reset", "cursor outside journal retention silently continued")
    ctx.evidence.append({"kind": "cursor_reset", "foreign_epoch": True, "old_cursor": old,
                         "new_cursor": reset.reducer.cursor})


@case("O5")
async def session_epoch_fence(ctx):
    declaration = await caps(ctx, "agent.session")
    if "new" not in declaration.get("actions", []):
        raise NotDeclared("agent.session.actions.new", declaration.get("actions"))
    view = await fresh_view(ctx)
    old_epoch, old_resource = view.reducer.epoch, ctx.agents[0]["id"]
    result = await ctx.invoke("agent.session", {"action": "new"})
    check(result.get("outcome") in ("accepted", "observed"), "native session switch not acknowledged")
    await ctx.wait_view(view, lambda reducer: reducer.epoch != old_epoch)
    check((await ctx.resource())["id"] == old_resource, "session switch replaced endpoint address")
    begins = [row for row in view.reducer.trace if row.get("op") == "snapshot" and row.get("phase") == "begin"]
    check(any(row.get("epoch") != old_epoch for row in begins), "new epoch had no preceding snapshot fence")


@case("O6")
async def viewer_order(ctx):
    first = await fresh_view(ctx)
    await ctx.native.call("turn", state="start")
    await ctx.native.call("retain", count=3, text="O6-old")
    await ctx.wait_view(first, lambda reducer: has_message(reducer, "2:O6-old"))
    second = await fresh_view(ctx, peer=await ctx.viewer())
    await ctx.native.call("retain", count=3, text="O6-new")
    for view in (first, second):
        await ctx.wait_view(view, lambda reducer: has_message(reducer, "2:O6-new"))
    def positions(reducer):
        return {key: entity["value"]["position"] for key, entity in reducer.entities.items() if "position" in entity["value"]}
    left, right = positions(first.reducer), positions(second.reducer)
    common = left.keys() & right.keys()
    check(common and all(left[key] == right[key] for key in common), "viewers disagree about fixed timeline positions")
    check(sorted(common, key=left.get) == sorted(common, key=right.get), "viewer timeline orders differ")


async def flood_retained(ctx, prefix, *, count=96, size=8192):
    for index in range(count):
        await ctx.native.call("retain", text=f"{prefix}-{index}:" + "x" * size)
    return f"{prefix}-{count - 1}:"


@case("O7")
async def stalled_viewer_isolation(ctx):
    first = await fresh_view(ctx)
    second = await fresh_view(ctx, peer=await ctx.viewer())
    stalled = await fresh_view(ctx, peer=await ctx.viewer())
    transport = stalled.peer.writer.transport
    transport.pause_reading()
    started = asyncio.get_running_loop().time()
    try:
        needle = await flood_retained(ctx, "O7-stall")
        await ctx.wait_view(first, lambda reducer: has_message(reducer, needle))
        await ctx.wait_view(second, lambda reducer: has_message(reducer, needle))
        check(not transport.is_reading(), "stalled viewer resumed reading during isolation", component="harness")
        ctx.evidence.append({"kind": "stalled_viewer", "fast_viewers": 2, "stalled_viewers": 1,
                             "native_records": 96, "payload_bytes": 96 * 8192,
                             "elapsed_ms": round((asyncio.get_running_loop().time() - started) * 1000),
                             "fast_cursors": [first.reducer.cursor, second.reducer.cursor]})
    finally:
        transport.resume_reading()


@case("O8")
async def unicode_frames(ctx):
    text = "snow☃😀\u2028\n" * 2500
    await ctx.native.call("turn", state="start")
    await ctx.native.call("retain", count=2, text=text)
    view = await fresh_view(ctx)
    await ctx.wait_view(view, lambda reducer: has_message(reducer, "1:" + text))
    check(not view.reducer.pending and view.reducer.failure is None, "UTF-8/JSONL reconstruction left a partial or invalid record")
    ctx.evidence.append({"kind": "unicode_snapshot", "source_utf8_bytes": len(text.encode()),
                         "byte_split_oracle": "self-tests feed every byte boundary, including Unicode separators"})


async def make_request(ctx, method=None, **options):
    declaration = await caps(ctx, "agent.respond")
    methods = declaration.get("methods", [])
    method = method or next((name for name in ("confirm", "choice", "text", "permission", "form", "url") if name in methods), None)
    if method is None:
        raise HarnessGap("no request fixture for the declared methods")
    before = {q["request"] for q in (await ctx.facts()).get("requests", [])}
    native_id = "native-" + ctx.delivery()
    await ctx.native.call("request", request=native_id, method=method, **options)
    facts = await ctx.wait_facts(lambda f: any(q.get("request") not in before for q in f.get("requests", [])))
    summary = next(q for q in facts["requests"] if q.get("request") not in before)
    peer = await ctx.viewer()
    try:
        view = await ctx.attach(peer=peer)
        key = ("request", summary["request"])
        await ctx.wait_view(view, lambda reducer: key in reducer.entities)
        entity = deepcopy(view.reducer.entities[key]["value"])
    finally:
        await peer.close()
    check(entity.get("method") == method, "request method changed during normalization")
    check(entity.get("revision") == summary.get("revision"), "request summary/entity revisions disagree")
    scope = entity.get("scope", {})
    check(isinstance(scope, dict) and (("session" in scope) != ("operation" in scope)), "request scope is ambiguous or missing")
    return native_id, entity


def schema_example(schema, depth=0):
    if depth > 16 or not isinstance(schema, dict):
        raise HarnessGap("form fixture schema is not a bounded object")
    supported = {"$schema", "$id", "title", "description", "default", "examples", "type", "properties", "required",
                 "additionalProperties", "enum", "const", "oneOf", "anyOf", "items", "minItems", "maxItems",
                 "minLength", "maxLength", "minimum", "maximum"}
    if set(schema) - supported:
        raise HarnessGap("form fixture cannot safely generate unsupported schema constraints")
    if "const" in schema:
        return deepcopy(schema["const"])
    if schema.get("enum"):
        return deepcopy(schema["enum"][0])
    for union in ("oneOf", "anyOf"):
        if schema.get(union):
            return schema_example(schema[union][0], depth + 1)
    kind = schema.get("type", "object" if "properties" in schema else "string")
    if isinstance(kind, list):
        kind = next((value for value in kind if value != "null"), "null")
    if kind == "object":
        properties, required = schema.get("properties", {}), schema.get("required", [])
        if len(required) > 32:
            raise HarnessGap("form fixture property-count bound")
        return {name: schema_example(properties.get(name, {}), depth + 1) for name in required}
    if kind == "array":
        count = schema.get("minItems", 0)
        if not integer(count) or count > 16:
            raise HarnessGap("form fixture array-count bound")
        return [schema_example(schema.get("items", {}), depth + 1) for _ in range(count)]
    if kind == "string":
        length = schema.get("minLength", 0)
        if not integer(length) or length > 4096:
            raise HarnessGap("form fixture string-size bound")
        return "x" * length
    if kind in ("integer", "number"):
        value = schema.get("minimum", min(0, schema.get("maximum", 0)))
        if kind == "integer" and not isinstance(value, int):
            raise HarnessGap("form fixture integer bounds are not integral")
        return value
    if kind == "boolean":
        return False
    if kind == "null":
        return None
    raise HarnessGap("no valid-response fixture for form schema type " + repr(kind))


def valid_response(request):
    method = request["method"]
    if method in ("permission", "choice"):
        choices = request.get("choices", [])
        if not choices:
            raise HarnessGap("choice/permission fixture has no advertised choice")
        choice = choices[0]
        if not isinstance(choice, dict) or not identifier(choice.get("id")):
            raise HarnessGap("choice fixture needs an advertised opaque choice id")
        return {"choice": choice["id"]}
    if method == "confirm":
        return {"value": False}
    if method == "text":
        return {"value": ""}
    if method == "form":
        return {"action": "accept", "content": schema_example(request.get("schema", {}))}
    if method == "url":
        return {"action": "decline"}
    raise HarnessGap("unknown response method " + method)


async def answer(ctx, request, response_value, **overrides):
    args = {"request": request["request"], "revision": request["revision"], "response": response_value, **overrides}
    return await ctx.invoke("agent.respond", args)


@case("R1")
async def requests_observed(ctx):
    await ctx.native.call("turn", state="start")
    await ctx.wait_facts(lambda f: f.get("state") == "running")
    declaration = await caps(ctx, "agent.respond")
    for method in declaration["methods"]:
        _, request = await make_request(ctx, method)
        check(request.get("status") == "pending" and not request.get("incomplete"), "new representable request not fully pending")
        scope_kind = "operation" if "operation" in request["scope"] else "run" if "run" in request["scope"] else "session"
        check(scope_kind in declaration.get("scopes", []), "request scope exceeds declaration")
        await ctx.wait_facts(lambda f: f.get("state") == "requires_action")
        result = await answer(ctx, request, {"action": "cancel"})
        check(result.get("outcome") in ("accepted", "dispatched"), "explicit request cancel was not handed over")


@case("R2")
async def first_valid_answer(ctx):
    _, request = await make_request(ctx)
    response = valid_response(request)
    for invalid in ({"revision": request["revision"] + 1}, {"epoch": "foreign-request-epoch"},
                    {"response": {"_conformance_invalid": True}}):
        mark = ctx.native.mark()
        rejection(await answer(ctx, request, response, **invalid))
        check(not ctx.native.writes(mark), "stale/invalid request response wrote to native runtime")
    second = await ctx.viewer()
    mark = ctx.native.mark()
    one, two = await asyncio.gather(answer(ctx, request, response),
        ctx.invoke("agent.respond", {"request": request["request"], "revision": request["revision"], "response": response}, peer=second))
    check(sum(row.get("outcome") in ("accepted", "dispatched") for row in (one, two)) == 1, "two concurrent answers won or neither did")
    rejection(next(row for row in (one, two) if row.get("outcome") not in ("accepted", "dispatched")), kinds=("stale",))
    await ctx.native.wait(lambda rows: any(r["seq"] > mark and r["kind"] == "answer" for r in rows))
    await asyncio.sleep(.1)
    check(len(ctx.native.records("answer", after=mark)) == 1, "concurrent answers produced more than one native response")
    mark = ctx.native.mark()
    rejection(await answer(ctx, request, response), kinds=("stale",))
    check(not ctx.native.writes(mark), "already-answered request was sent again")
    ctx.evidence.append({"kind": "first_valid_answer", "native_responses": 1,
                         "remaining_branches": "expiry and dead-scope traces follow in this incremental runner"})
    raise HarnessGap("R2 core race/stale checks executed; expiry/dead-scope branches not yet implemented")


@case("R3")
async def answer_ack_precision(ctx):
    declaration = await caps(ctx, "agent.respond")
    _, request = await make_request(ctx)
    result = await answer(ctx, request, valid_response(request))
    expected = declaration.get("ack")
    if expected not in ("accepted", "dispatched"):
        raise HarnessGap("unknown request acknowledgement declaration " + repr(expected))
    check(result.get("outcome") == expected, f"answer acknowledgement over/underclaimed: {result}")


@case("R4")
async def unrepresentable_requests(ctx):
    mark = ctx.native.mark()
    await ctx.native.call("request", request="unknown-native-method", method="_conformance_unknown_dialog")
    await ctx.native.wait(lambda rows: any(r["seq"] > mark and r["kind"] == "answer" for r in rows))
    await asyncio.sleep(.1)
    answers = ctx.native.records("answer", after=mark)
    check(len(answers) == 1 and answers[0].get("normalized_response") == {"action": "cancel"},
          "unknown request method was not cancelled exactly once")
    raise HarnessGap("R4 unknown-method cancellation executed; oversized/incomplete approval branch not yet implemented")


@case("R5")
async def cancel_is_distinct(ctx):
    declaration = await caps(ctx, "agent.respond")
    for method in declaration["methods"]:
        mark = ctx.native.mark()
        _, request = await make_request(ctx, method)
        result = await answer(ctx, request, {"action": "cancel"})
        check(result.get("outcome") in ("accepted", "dispatched"), "cancel not handed over")
        await ctx.native.wait(lambda rows: any(r["seq"] > mark and r["kind"] == "answer" for r in rows))
        native = ctx.native.records("answer", after=mark)[-1]
        check(native.get("normalized_response") == {"action": "cancel"}, "cancel became denial/false/empty")
        mark = ctx.native.mark()
        _, request = await make_request(ctx, method)
        response = valid_response(request)
        await answer(ctx, request, response)
        await ctx.native.wait(lambda rows: any(r["seq"] > mark and r["kind"] == "answer" for r in rows))
        native = ctx.native.records("answer", after=mark)[-1]
        normalized = native.get("normalized_response")
        check(isinstance(normalized, dict) and normalized.get("action") not in ("cancel", "_invalid"),
              "valid denial/false/empty became cancel or an invalid native response")
        if method in ("confirm", "text"):
            check(normalized == response, "false/empty response changed during normalization")


async def lifecycle_wait(ctx, predicate, description):
    try:
        return await ctx.peer.wait(predicate)
    except asyncio.TimeoutError as error:
        raise CheckFailed(description) from error


@case("M2")
async def native_sessions(ctx):
    declaration = await caps(ctx, "agent.session")
    actions = declaration.get("actions", [])
    rid, initial = ctx.agents[0]["id"], await ctx.facts()
    mark = ctx.native.mark()
    if "new" not in actions:
        raise HarnessGap("M2 resume-only/sessionless bootstrap fixture not yet implemented")
    result = await ctx.invoke("agent.session", {"action": "new"})
    check(result.get("outcome") in ("observed", "accepted"), "declared session:new failed")
    fresh = ctx.resource_cache.get(rid, {}).get("data", {}).get("agent", {})
    first = (fresh.get("session") or {}).get("id")
    check(identifier(first), "new session facts were not visible before the reply")
    check(fresh.get("epoch") != initial["epoch"], "new session did not advance endpoint epoch")
    if "list" in actions:
        listing = await ctx.invoke("agent.session", {"action": "list"})
        rows = (listing.get("data") or {}).get("sessions")
        check(isinstance(rows, list) and len(rows) <= 256 and len(json.dumps(rows).encode()) <= 64 << 10,
              "session list is not a bounded advertised page")
        check(any(row.get("id") == first for row in rows), "new session is absent from native list")
        if (listing.get("data") or {}).get("next") is not None:
            raise HarnessGap("M2 multi-page native session listing not yet implemented")
    if "fork" in actions:
        before_invalid = ctx.native.mark()
        rejected = await ctx.invoke("agent.session", {"action": "fork", "session": first, "at": "not-an-advertised-position"})
        rejection(rejected, kinds=("invalid", "unsupported"))
        check(not ctx.native.writes(before_invalid), "unadvertised fork position reached native runtime")
        fork = await ctx.invoke("agent.session", {"action": "fork", "session": first})
        check(fork.get("outcome") in ("observed", "accepted"), "declared native fork failed")
        after = ctx.resource_cache.get(rid, {}).get("data", {}).get("agent", {})
        check((after.get("session") or {}).get("id") not in (None, first), "fork returned before its new session facts")
        check(after.get("epoch") != fresh["epoch"], "fork did not change endpoint epoch")
        fresh = after
    if "resume" in actions:
        resumed = await ctx.invoke("agent.session", {"action": "resume", "session": first})
        check(resumed.get("outcome") in ("observed", "accepted"), "declared native resume failed")
        after = ctx.resource_cache.get(rid, {}).get("data", {}).get("agent", {})
        check((after.get("session") or {}).get("id") == first, "resume facts followed the reply or selected wrong session")
        if (fresh.get("session") or {}).get("id") != first:
            check(after.get("epoch") != fresh["epoch"], "resume switch did not start a fresh epoch")
    check(not ctx.native.records("input_received", after=mark), "session new/resume/fork sent a prompt")
    check((await ctx.resource(rid))["id"] == rid, "session switch replaced the endpoint resource")
    ctx.evidence.append({"kind": "session_actions", "actions": actions, "sessionless_at_start": initial.get("session") is None,
                         "stable_resource": rid, "first_session": first, "no_prompt": True})
    if any(action in actions for action in ("close", "delete")):
        raise HarnessGap("M2 declared close/delete fixture branches not yet implemented")


@case("M1")
async def native_queue(ctx):
    declaration = await caps(ctx, "agent.queue")
    actions = declaration.get("actions", [])
    if declaration.get("ids") == "none":
        check(set(actions) <= {"list", "clear"}, "null native IDs falsely permit per-item actions")
    await ctx.native.call("turn", state="start")
    facts = await ctx.wait_facts(lambda f: f.get("state") == "running")
    expected = []
    for mode in ("boundary", "settle"):
        if mode in facts["caps"].get("agent.send", {}).get("modes", {}):
            text = "M1-native-" + mode
            accepted(await ctx.send(text, mode))
            expected.append(text)
    if not expected:
        raise HarnessGap("queue module present without a declared enqueue mode fixture")
    facts = await ctx.wait_facts(lambda f: isinstance(f.get("queue"), list) and len(f["queue"]) == len(expected))
    if declaration.get("ids") == "none":
        check(all(row.get("id") is None for row in facts["queue"]), "adapter invented IDs from delivery tracking")
        mark = ctx.native.mark()
        for action in ("withdraw", "replace"):
            rejection(await ctx.invoke("agent.queue", {"epoch": facts["epoch"], "action": action, "id": "not-a-native-id"}), kinds=("unsupported",))
        check(not ctx.native.writes(mark), "null-ID per-item action reached native runtime")
    else:
        raise HarnessGap("M1 native-ID withdraw/replace fixture branch not yet implemented")
    if "list" in actions:
        listed = await ctx.invoke("agent.queue", {"epoch": facts["epoch"], "action": "list"})
        check(listed.get("outcome") == "observed" and (listed.get("data") or {}).get("queue") == facts["queue"],
              "native queue list differs from observed queue facts")
    if "clear" not in actions:
        return
    mark = ctx.native.mark()
    cleared = await ctx.invoke("agent.queue", {"epoch": facts["epoch"], "action": "clear"})
    check(cleared.get("outcome") == "observed", "clear did not return its declared native removed content")
    removed = (cleared.get("data") or {}).get("removed")
    check(isinstance(removed, list) and len(removed) == len(expected), "clear lost/duplicated native removed content")
    for text in expected:
        matches = [row for row in removed if text in json.dumps(row.get("content", []))]
        check(len(matches) == 1, "removed content changed or duplicated a native queue item")
        compare_content([{"type": "text", "text": text}], matches[0]["content"], sender_header=True)
    check(not ctx.native.records("inserted", after=mark), "queue disappearance falsely claimed insertion")
    again = await ctx.invoke("agent.queue", {"epoch": facts["epoch"], "action": "clear"})
    check(again.get("outcome") == "observed" and (again.get("data") or {}).get("removed") == [], "clear returned the same removed content twice")
    ctx.evidence.append({"kind": "queue_clear_once", "first": cleared, "second": again,
                         "media_declaration": declaration.get("removed", {}).get("media")})


@case("L2")
async def stop_evidence(ctx):
    declaration = await caps(ctx, "agent.stop")
    mark = ctx.native.mark()
    rejection(await ctx.invoke("agent.stop", {"v": 1, "covers": declaration.get("covers", [])}), kinds=("invalid", "unsupported"))
    check(not ctx.native.writes(mark), "refused stop wrote to native runtime")
    check((await ctx.facts()).get("lifecycle") == "live", "refused stop did not remain live")
    pid = (await ctx.native.call("state"))["pid"]
    result = await ctx.invoke("agent.stop", {"covers": declaration.get("covers", [])})
    check(result.get("outcome") in ("accepted", "observed", "dispatched"), "declared stop failed")
    data = result.get("data") or {}
    applied = data.get("covers")
    ctx.evidence.append({"kind": "stop_reply", "result": result, "declared": declaration})
    check(isinstance(applied, list) and all(isinstance(value, str) for value in applied)
          and set(applied) <= set(declaration.get("covers", [])), "stop did not report applied coverage within its declaration")
    if data.get("covers_evidence") == "requested":
        check(applied == declaration.get("covers", []), "requested-only stop coverage changed the request")
    rid = ctx.agents[0]["id"]
    def ended():
        return ctx.resource_cache.get(rid, {}).get("data", {}).get("agent", {}).get("lifecycle") == "ended"
    await lifecycle_wait(ctx, ended, "no native endpoint end evidence after stop")
    ctx.evidence.append({"kind": "stop_end", "native_pid": pid, "result": result,
                         "final_resource": ctx.resource_cache[rid], "removed": ctx.removed.get(rid)})
    matching = [event["data"] for event in ctx.events if event.get("type") == "effect"
                and event.get("data", {}).get("op") == "agent.stop" and event["data"].get("outcome") != "rejected"]
    check(len(matching) == 1, "stop effect was duplicated")


@case("L3")
async def host_lifecycle(ctx):
    facts = await ctx.facts()
    owns = facts["host"]["owns_lifecycle"]
    rid, host = ctx.agents[0]["id"], await ctx.resource(ctx.host)
    result = await ctx.peer.call("resource.kill", {"resource": ctx.host, "deadline_ms": 1000})
    check(result.get("outcome") in ("observed", "accepted"), "private host termination not acknowledged", component="core")
    def finished():
        value = ctx.resource_cache.get(ctx.host, {})
        return value if value.get("state") == "finished" else None
    final_host = await lifecycle_wait(ctx, finished, "bound host did not report its finish")
    check(final_host.get("incarnation") == host.get("incarnation"), "host end evidence changed incarnation")
    endpoint = ctx.resource_cache.get(rid, {})
    if not owns:
        check(endpoint.get("data", {}).get("agent", {}).get("lifecycle") != "ended",
              "non-owning host disappearance fabricated endpoint end")
    ctx.evidence.append({"kind": "host_end", "owns_lifecycle": owns, "bound_host": host,
                         "observed_host": final_host, "endpoint": endpoint})
    raise HarnessGap("L3 bound host end exercised; stale old-host end injection is not yet implemented")


@case("L4")
async def disconnect_not_end(ctx):
    rid = ctx.agents[0]["id"]
    pid = (await ctx.native.call("state"))["pid"]
    await ctx.native.call("turn", state="start")
    await ctx.wait_facts(lambda f: f.get("state") == "running")
    await ctx.native.call("turn", state="settled")
    await ctx.wait_facts(lambda f: f.get("state") == "idle")
    check((await ctx.facts()).get("lifecycle") == "live", "idle completion was treated as endpoint end")
    await ctx.peer.call("command.invoke", {"name": "vqro.safe_mode", "args": {"on": True}})
    await lifecycle_wait(ctx, lambda: ctx.resource_cache.get(rid, {}).get("available") is False, "disconnect was not observable")
    check((await ctx.native.call("state"))["pid"] == pid, "provider disconnect killed native execution")
    check(ctx.resource_cache[rid].get("data", {}).get("agent", {}).get("lifecycle") != "ended",
          "bridge disconnect fabricated end evidence")
    await ctx.peer.call("command.invoke", {"name": "vqro.safe_mode", "args": {"on": False}})
    await lifecycle_wait(ctx, lambda: ctx.resource_cache.get(rid, {}).get("available") is True, "endpoint did not reconnect")
    check((await ctx.native.call("state"))["pid"] == pid, "reconnect replaced the native child")
    await ctx.native.call("exit", code=0)
    await lifecycle_wait(ctx, lambda: ctx.resource_cache.get(rid, {}).get("data", {}).get("agent", {}).get("lifecycle") == "ended", "native exit did not publish endpoint end")
    await ctx.restart_core()
    current = await ctx.resource(rid)
    check(current is None or current.get("data", {}).get("agent", {}).get("lifecycle") == "ended", "reconnect undid known end evidence")
    ctx.evidence.append({"kind": "lifecycle_reconnect", "same_pid_until_native_end": True, "ended_never_reopened": True})


@case("D13")
async def serialized_preflight(ctx):
    profile = await mode_caps(ctx, "start")
    if profile.get("preflight") != "serialized":
        raise NotDeclared("agent.send.modes.start.preflight", profile.get("preflight"))
    idle = await ctx.wait_facts(lambda f: f.get("state") == "idle")
    before_completion, before_run = idle.get("completion"), idle.get("run")
    second_peer = await ctx.viewer()
    mark = ctx.native.mark()
    await ctx.native.call("pause_before", phase="acceptance")
    first = asyncio.create_task(ctx.send("D13-first", "start", deadline_ms=1500))
    interrupted = None
    try:
        await ctx.native.wait(lambda rows: any(r["seq"] > mark and r["kind"] == "paused" and r.get("phase") == "acceptance" for r in rows))
        second = await ctx.send("D13-overlap", "start", peer=second_peer)
        rejection(second, kinds=("busy",))
        starts = ctx.native.records("command", after=mark, operation="send")
        check(len(starts) == 1, "two unacknowledged native starts were in flight")
        check(not ctx.native.records("input_received", after=mark), "fixture did not gate native acceptance", component="fixture")
        await ctx.native.call("turn", state="settled")
        if "agent.interrupt" in idle["caps"]:
            interrupted = asyncio.create_task(ctx.invoke("agent.interrupt", deadline_ms=1500))
        if interrupted is not None:
            try:
                interrupt_result = await asyncio.wait_for(asyncio.shield(interrupted), .5)
            except asyncio.TimeoutError as error:
                raise CheckFailed("interrupt waited behind an unresolved start preflight") from error
            check(interrupt_result.get("outcome") in ("accepted", "observed", "dispatched"), "interrupt failed while preflight was pending")
        await asyncio.sleep(.25)
        gated = await ctx.facts()
        check(gated.get("completion") == before_completion and gated.get("run") == before_run,
              "settled event while preflight was gated fabricated run/completion")
        await ctx.native.call("release", phase="acceptance")
        result = await first
        check(result.get("outcome") in ("accepted", "dispatched"), "interrupt cancelled unresolved native start preflight")
        if interrupted is not None:
            check((await interrupted).get("outcome") in ("accepted", "observed", "dispatched"), "interrupt failed while preflight was pending")
        await ctx.native.wait(lambda rows: any(r["seq"] > mark and r["kind"] == "input_received" for r in rows))
        inputs = ctx.native.records("input_received", after=mark)
        check(len(inputs) == 1 and "D13-first" in inputs[0]["text"], "overlapping starts duplicated/lost the native input")
        ctx.evidence.append({"kind": "serialized_start", "unacknowledged_native_starts": len(starts), "inputs": len(inputs),
                             "settled_during_preflight": True, "interrupt_exercised": interrupted is not None})
    finally:
        await ctx.native.call("release", phase="all")
        for task in (first, interrupted):
            if task is not None and not task.done():
                task.cancel()
        await asyncio.gather(*(task for task in (first, interrupted) if task is not None), return_exceptions=True)
