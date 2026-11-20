"""Part A public-wire cases. No runtime names or native command spellings here."""
import asyncio
from copy import deepcopy
import json

from .content import examples
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
    check(isinstance(agent, dict) and agent.get("v") == 0, "provider did not declare agent {v:0,modules}")
    check(isinstance(agent.get("modules"), dict), "provider agent.modules is not an object")
    check("items.stream" in upper, "structured adapter did not declare mandatory item observation")
    check(ctx.provider.get("host") == ctx.host, "provider not bound to its actual private execution host")
    for resource in ctx.agents:
        facts = resource.get("data", {}).get("agent", {})
        check(facts.get("v") == 0, "resource lacks data.agent.v:0")
        check(len(json.dumps(facts, ensure_ascii=False, separators=(",", ":")).encode()) <= 64 << 10, "agent facts exceed 64 KiB")
        check(identifier(facts.get("epoch")), "missing/oversized observation epoch")
        check(isinstance(facts.get("state"), str) and isinstance(facts.get("lifecycle"), str), "missing state/lifecycle")
        state = facts["state"]
        coarse = "error" if state == "idle" and (facts.get("completion") or {}).get("reason") == "error" else {
            "idle": "idle", "running": "working", "requires_action": "blocked"}.get(state, "working")
        check((resource.get("status") or {}).get("state") == coarse, "coarse status disagrees with native-evidence facts")
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
    invalids = [{"v": 1}, {"epoch": "foreign-conformance-epoch"}, {"caps_rev": fact["caps"]["rev"] + 1}]
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


@case("D6")
async def unsupported_content(ctx):
    declaration = (await caps(ctx, "agent.send"))["content"]
    types, shape, blocks = declaration["types"], declaration["shape"], examples()
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
        check([block.get("type") for block in actual] == [block["type"] for block in content],
              "typed content was reordered, dropped, flattened or concatenated")
        for before, after in zip(content, actual):
            for field, value in before.items():
                if field == "text" and before["type"] == "text":
                    check(value in after.get(field, ""), "literal text changed inside its declared origin frame")
                else:
                    check(after.get(field) == value, f"native {before['type']}.{field} changed")
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
