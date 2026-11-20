# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""I13 over public transactions, actual scheduling and controlled native outcomes.

A later ordinary message actually reaches the same destination/lane between the
protected move and a failed configuration stage. This is a positive scheduling
witness, not a sleep, private store mutation or target-engine import.
"""
from copy import deepcopy
import time

from ..wire import RpcError
from .cases import case, held, settled, invocations

HANDLE = "@i13-worker"
NAME_KEY = "doc/agents/names/" + HANDLE


async def entries(ctx):
    return (await ctx.peer.call("workspace.snapshot"))["entries"]


async def record(ctx, delivery):
    # Public read-only primitive, not a store import or mutation. inbox.get is an
    # intentionally compact projection and cannot witness retained content/guard.
    value = await ctx.peer.call("operational.get", {"domain": "coord", "key": "operational/coord/message/" + delivery})
    ctx.check(isinstance(value.get("entry"), dict), "actual protected record exists for the public handover witness")
    return value["entry"]["value"]


async def gate_cut(ctx, expected):
    control = await ctx.peer.call("operational.get", {"domain": "coord", "key": "operational/coord/control"})
    gates = control["entry"]["value"]["gates"]
    ctx.check(all(gates.get(agent) is value for agent, value in expected.items()),
        "handover preserves actual source and destination pause gates", expected=expected, observed=gates)


async def name(ctx, agent):
    current = (await entries(ctx)).get(NAME_KEY, {})
    return await ctx.command("names", {"op": await ctx.operation("i13-name"), "handle": HANDLE,
        "agent": agent, "expect": current.get("version", 0)})


async def membership(ctx, agent, project):
    key = "doc/agents/members/" + agent
    current = (await entries(ctx)).get(key, {})
    return await ctx.command("membership.set", {"op": await ctx.operation("i13-member"), "agent": agent,
        "project": project, "reports_to": None, "expect": current.get("version", 0)})


async def stages(ctx, destination):
    rows = await entries(ctx)
    return {"bind": {"handle": HANDLE, "agent": destination, "expect": rows[NAME_KEY]["version"]},
        "membership": {"agent": destination, "project": "i13-final", "reports_to": None,
            "expect": rows["doc/agents/members/" + destination]["version"]}}


def exact(agent):
    return {"agent": agent["id"], "incarnation": agent["incarnation"]}


async def handover(ctx, **fields):
    return await ctx.command("handover", {"op": await ctx.operation("i13-handover"), **fields})


async def pending_cut(ctx, deliveries, destination, *, reason="handover_pending"):
    records = [await record(ctx, delivery) for delivery in deliveries]
    ctx.check(all(row["state"] == "held" and row.get("reason") == reason and
        row["to"]["agent"] == destination and not row.get("attempt") for row in records),
        "moved records are held without any claim at the observed cut", records=records, reason=reason)
    ctx.check(all(not invocations(ctx, delivery=delivery) for delivery in deliveries),
        "no moved delivery has reached native invocation at this cut")
    return records


async def scheduling_witness(ctx, deliveries, destination, plan, label):
    # The move RPC has already committed. This later message uses the same admit
    # lane/endpoint; its real claim+native invocation proves a scheduler pass before
    # the caller is allowed to issue the next (possibly failing) stage.
    observed = deepcopy(next(row for row in ctx.agents if row["id"] == destination))
    sentinel = await ctx.send(target=destination, mode="admit", send_new=True, text="I13 scheduling witness: " + label)
    result = await settled(ctx, sentinel["delivery"])
    calls = invocations(ctx, delivery=sentinel["delivery"], op="agent.send")
    ctx.check(result["state"] in ("accepted", "admitted", "dispatched", "observed", "inserted") and len(calls) == 1,
        "a real destination scheduling pass completes before the next handover stage", result=result, calls=calls)
    call = calls[0]
    args = call.get("args") or {}
    ctx.check(call.get("ref") == observed["backend_ref"] and args.get("mode") == "admit" and
        args.get("delivery") == sentinel["delivery"],
        "actual native scheduling invocation uses the inspected destination and admit lane", call=call, destination=observed)
    claim = ((call.get("witness") or {}).get("entry") or {}).get("value", {})
    target = claim.get("to") or {}
    ctx.check(claim.get("state") == "claimed" and claim.get("delivery") == sentinel["delivery"] and
        target.get("agent") == observed["id"] and target.get("incarnation") == observed["incarnation"] and
        claim.get("mode") == "admit" and (claim.get("attempt") or {}).get("strategy") == "admit",
        "positive scheduling witness saw a real claim for the inspected destination/incarnation and admit lane", claim=claim)
    await pending_cut(ctx, deliveries, destination)
    ctx.evidence.append({"kind": "handover_scheduling_barrier", "plan": plan, "label": label,
        "sentinel": sentinel["delivery"], "native_invoke_seq": calls[0]["seq"], "moved": list(deliveries)})


async def rejected_stage(ctx, plan, index):
    try: result = await handover(ctx, action="stage", plan=plan, stage=index)
    except RpcError as error:
        ctx.check(error.kind == "conflict", "stale stage has a definite CAS refusal", refusal=error.kind)
    else:
        ctx.check(result.get("state") == "rejected", "stale ordinary stage is not accepted or unknown", reply=result)


async def refuses_complete(ctx, plan):
    await ctx.rejected("handover", {"op": await ctx.operation("i13-early-complete"), "action": "complete", "plan": plan}, {"conflict"})


async def refusal_preserves(ctx, source, destination, deliveries, expected_state):
    before = [await record(ctx, delivery) for delivery in deliveries]
    ctx.check(before[-1]["state"] == expected_state, "protected non-pending state really exists before move", record=before[-1])
    ctx.check(all(row["state"] in ("held", "recorded") and not row.get("attempt") for row in before[:-1]),
        "mixed-list prefix really is pending before the refusal", records=before[:-1])
    await ctx.rejected("handover", {"op": await ctx.operation("i13-nonpending"), "from": exact(source),
        "to": exact(destination), "deliveries": deliveries}, {"too_late"})
    after = [await record(ctx, delivery) for delivery in deliveries]
    # Both native barriers keep this cut stable. No concurrent evidence is allowed
    # to explain away any sibling's state, content, guard, identity or intent loss.
    ctx.check(after == before, "mixed-list refusal preserves every complete protected record at the held stable cut",
        before=before, after=after)


@case("I13")
async def staged_successor_handover(ctx):
    await ctx.native.call("add", ref="a2")
    await ctx.wait_agents(3)
    source, destination, other = ctx.agents[:3]
    sid, did = source["id"], destination["id"]
    await name(ctx, sid)
    await membership(ctx, did, "i13-initial")
    # Materialize the requested paused source through its public control action;
    # an omitted default-closed gate is not an explicit persisted pause witness.
    await ctx.scope("pause", "agent:" + sid)
    expiry = int(time.time() * 1000) + 300000
    moved = [(await ctx.send(mode="admit", send_new=True, guard={"session": "s1"}, expires_at_ms=expiry,
        text="I13 held intent " + str(index)))["delivery"] for index in range(2)]
    for delivery in moved: await held(ctx, delivery, "paused")
    original = [await record(ctx, delivery) for delivery in moved]
    ctx.check(all(row.get("guard") == {"session": "s1"} and row.get("expires_at_ms") == expiry and row.get("content")
        and row.get("origin") for row in original), "preservation probes begin with real nonempty source constraints", records=original)
    resumed = await ctx.scope("resume", "agent:" + did)
    ctx.check(resumed.get("targets") == [did], "destination is explicitly open while source stays paused", reply=resumed)
    await gate_cut(ctx, {sid: False, did: True})
    await ctx.native_set(0, lifecycle="ended")

    for failure in ("names", "membership"):
        arguments = await stages(ctx, did)
        move = await handover(ctx, **{"from": exact(source), "to": exact(destination), "deliveries": moved, **arguments})
        plan = move["plan"]
        await gate_cut(ctx, {sid: False, did: True})
        await pending_cut(ctx, moved, did)
        await scheduling_witness(ctx, moved, did, plan, "after-move-before-failed-" + failure)
        await refuses_complete(ctx, plan)
        if failure == "membership":
            first = await handover(ctx, action="stage", plan=plan, stage=0)
            ctx.check(first.get("state") == "accepted", "first ordinary stage actually succeeded", reply=first)
            await scheduling_witness(ctx, moved, did, plan, "after-name-before-failed-membership")
            await membership(ctx, did, "i13-newer-membership")
            key, index = "doc/agents/members/" + did, 1
        else:
            await name(ctx, other["id"])
            key, index = NAME_KEY, 0
        newer = deepcopy((await entries(ctx))[key])
        await rejected_stage(ctx, plan, index)
        await refuses_complete(ctx, plan)
        await pending_cut(ctx, moved, did)
        await rejected_stage(ctx, plan, index)  # a fresh late retry still has the old expected revision
        ctx.check((await entries(ctx))[key] == newer, "late failed stage cannot overwrite the newer ordinary binding", kind=failure, entry=newer)
        if failure == "membership":
            await name(ctx, other["id"])
            newer_name = deepcopy((await entries(ctx))[NAME_KEY])
            retry = await handover(ctx, action="stage", plan=plan, stage=0)
            ctx.check(retry.get("state") == "accepted" and retry.get("historical") is True and
                (await entries(ctx))[NAME_KEY] == newer_name,
                "late accepted-stage retry is historical, never overwrites a newer name", reply=retry)
        aborted = await handover(ctx, action="abort", plan=plan)
        ctx.check(aborted.get("state") == "aborted", "failed plan is explicitly aborted, not silently completed", reply=aborted)
        await pending_cut(ctx, moved, sid, reason="handover_aborted")
        await gate_cut(ctx, {sid: False, did: True})

    final = await handover(ctx, **{"from": exact(source), "to": exact(destination), "deliveries": moved, **(await stages(ctx, did))})
    plan = final["plan"]
    await gate_cut(ctx, {sid: False, did: True})
    await scheduling_witness(ctx, moved, did, plan, "successful-plan-before-stages")
    for index in (0, 1):
        stage = await handover(ctx, action="stage", plan=plan, stage=index)
        ctx.check(stage.get("state") == "accepted", "ordinary handover stage commits before release", index=index, reply=stage)
        if index == 0: await refuses_complete(ctx, plan)
    await scheduling_witness(ctx, moved, did, plan, "all-stages-accepted-before-explicit-complete")
    await ctx.scope("pause", "agent:" + did)
    complete = await handover(ctx, action="complete", plan=plan)
    ctx.check(complete.get("state") == "completed", "only an explicit all-stage complete releases the plan", reply=complete)
    delivered = await pending_cut(ctx, moved, did, reason="paused")
    await gate_cut(ctx, {sid: False, did: False})
    for before, after in zip(original, delivered):
        ctx.check(all(after.get(key) == before.get(key) for key in ("origin", "content", "guard", "expires_at_ms")),
            "handover preserves sender, content, guard and expiry", before=before, after=after)
    await ctx.scope("resume", "agent:" + did)
    for delivery in moved:
        await settled(ctx, delivery)
        ctx.check(len(invocations(ctx, delivery=delivery)) == 1, "later explicit resume dispatches moved intent exactly once")

    # Genuine claimed cut, then actual native application with its reply held.
    # Later native end makes the outstanding claim unknown without disconnecting
    # the shared provider (which would also make our destination unavailable).
    await ctx.native.call("plan", ref=other["backend_ref"], op="agent.send",
        plan={"before": "i13-claimed", "after": "i13-after-apply"})
    claimed = await ctx.send(target=other["id"], mode="admit", send_new=True, text="I13 claimed then unknown")
    sibling = await ctx.send(target=other["id"], mode="admit", send_new=True, text="I13 pending sibling must not partially move")
    await ctx.scope("resume", "agent:" + other["id"])
    await ctx.wait(lambda: invocations(ctx, delivery=claimed["delivery"]), label="actual claimed native barrier")
    retired = await ctx.command("control", {"op": await ctx.operation("i13-retire"), "target": other["id"], "action": "retire", "args": {}})
    ctx.check(retired.get("state") == "retired" and retired.get("execution") == "unknown" and retired.get("native") is None,
        "predecessor routing is actually retired without inventing native end", reply=retired)
    released = False
    try:
        await refusal_preserves(ctx, other, destination, [sibling["delivery"], claimed["delivery"]], "claimed")
        await ctx.native.call("release", barrier="i13-claimed")
        released = True
        await ctx.wait(lambda: [row for row in ctx.native.records("applied") if row.get("args", {}).get("delivery") == claimed["delivery"]],
            label="actual native application before end with reply held")
        await ctx.native_set(2, lifecycle="ended")
        unknown = await ctx.wait_command("inbox.get", lambda row: row.get("state") == "unknown", args={"delivery": claimed["delivery"]})
        await ctx.wait_agents(3)
        ctx.check(all(any(row["id"] == old["id"] and row["incarnation"] == old["incarnation"] for row in ctx.agents)
            for old in (source, destination, other)), "available endpoints retain the exact inspected identities at unknown refusal")
        await refusal_preserves(ctx, other, destination, [sibling["delivery"], claimed["delivery"]], "unknown")
        ctx.check(len(invocations(ctx, delivery=claimed["delivery"])) == 1 and not invocations(ctx, delivery=sibling["delivery"]),
            "claimed/unknown handover refusal never replays input or dispatches its pending sibling", unknown=unknown)
    finally:
        if not released: await ctx.native.call("release", barrier="i13-claimed")
        await ctx.native.call("release", barrier="i13-after-apply")
