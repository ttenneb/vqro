# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Priority ordering never grants admission, execution or reserved capacity."""
import asyncio
import json
from .cases import case, invocations, settled, held
from .inbox_cases import pending_mutation_claim_races
from ..wire import RpcError
from ..results import HarnessGap


def reserve_pressure_plan(limits):
    """Legal encoded content and fresh targets; never shrink the real domain budget."""
    quotas = limits.get("quotas") or {}
    names = ("content_bytes", "domain_bytes", "metadata_bytes", "outcome_bytes",
             "sender_target_bytes", "sender_bytes", "target_bytes")
    if any(type(quotas.get(name)) is not int or quotas[name] <= 0 for name in names):
        raise HarnessGap("I21 needs actual declared content/domain/sender/target byte quotas")
    percent = limits.get("control_reserve_percent")
    if type(percent) is not int or not 0 < percent < 100:
        raise HarnessGap("I21 needs the actual control reserve percentage")
    overhead = len(json.dumps([{"type": "text", "text": ""}], separators=(",", ":")).encode())
    charge_extra = quotas["metadata_bytes"] + quotas["outcome_bytes"]
    content = min(quotas["content_bytes"], quotas["sender_target_bytes"] - charge_extra,
                  quotas["sender_bytes"] // 2 - charge_extra, quotas["target_bytes"] - charge_extra)
    if content <= overhead:
        raise HarnessGap("declared quotas cannot fit a legal I21 pressure text record")
    normal = quotas["domain_bytes"] - (quotas["domain_bytes"] * percent + 99) // 100
    inputs = (normal + content - 1) // content + 1
    if inputs > 128:
        raise HarnessGap("declared reserve cannot be reached by the bounded128-input I21 fixture")
    return {"text": "x" * (content - overhead), "encoded_content_bytes": content,
            "targets": inputs, "normal_bytes": normal, "quotas": quotas}


def reserve_refusal(error):
    return error.kind == "quota" and any(message in str(error) for message in (
        "normal byte budget would consume control reserve", "normal record budget would consume control reserve"))


@case("I21")
async def priority_obeys_all_other_guards(ctx):
    a=await ctx.connect("worker:priority-a");b=await ctx.connect("worker:priority-b")
    for name in ("a","b"): await ctx.grant("priority-"+name,"worker:priority-"+name,["send:boundary"])
    await ctx.native_set(0,state="running",run="existing-run")
    low=await ctx.send(peer=a,mode="boundary",priority="low",text="A-low")
    ah=[await ctx.send(peer=a,mode="boundary",priority="high",text="A-high-"+str(i),send_new=True) for i in range(2)]
    bh=[await ctx.send(peer=b,mode="boundary",priority="high",text="B-high-"+str(i),send_new=True) for i in range(2)]
    normal=await ctx.send(mode="boundary",text="default normal")
    ctx.check((await ctx.get(normal["delivery"]))["priority"]=="normal" and not invocations(ctx),"default priority is normal and high never bypasses paused gate")
    await ctx.scope("resume")
    for row in [low,*ah,*bh,normal]: await settled(ctx,row["delivery"])
    order=[row["args"]["delivery"] for row in invocations(ctx)]
    highs={row["delivery"] for row in ah+bh}
    ctx.check(set(order[:4])==highs and order[-2:]==[normal["delivery"],low["delivery"]],"eligible high overtakes normal/earlier low, then lower priorities progress")
    senders=["a" if delivery in {r["delivery"] for r in ah} else "b" for delivery in order[:4]]
    ctx.check(senders[0]!=senders[1] and senders[2]!=senders[3] and all(order.index(rows[0]["delivery"])<order.index(rows[1]["delivery"]) for rows in (ah,bh)),"equal priority uses sender round-robin and per-sender FIFO")
    ctx.evidence.append({"kind":"spec_scope","source":"coordination-v0.md priority rule","rule":"There is no aging in v0"})
    # The fake's native queue advances one accepted boundary input per safe point.
    for _ in range(6): drained=await ctx.native.call("point",ref=ctx.agents[0]["backend_ref"],point="idle")
    ctx.check(drained["pending"]==0 and [row["delivery"] for row in ctx.native.records("model_request")]==order,
              "actual native model requests retain the observed priority selection order")
    await pending_mutation_claim_races(ctx,priority=True)
    denied=await ctx.connect("worker:priority-no-grant");before_denied=len(invocations(ctx))
    try: await ctx.send(peer=denied,mode="boundary",priority="high")
    except RpcError as error: ctx.check(error.kind in ("forbidden","no_grant"),"priority conveys no standing")
    else: ctx.check(False,"high priority bypassed grant admission")
    ctx.check(len(invocations(ctx))==before_denied,"denied high-priority admission dispatches no native input")
    await ctx.scope("pause");await ctx.native_set(0,state="running",run="existing-run")
    blocked=await ctx.send(priority="high",mode="start",text="busy high start")
    await ctx.scope("resume");await held(ctx,blocked["delivery"],"agent_busy")
    ctx.check(not invocations(ctx,delivery=blocked["delivery"]),"high cannot bypass native eligibility or promote a start lane")
    await ctx.scope("pause");await ctx.scope("pause","agent:"+ctx.agents[1]["id"])
    for index in range(8): await ctx.send(target=ctx.agents[1]["id"],mode="boundary",priority="high",text="quota-"+str(index),send_new=True)
    try: await ctx.send(target=ctx.agents[1]["id"],mode="boundary",priority="high",send_new=True)
    except RpcError as error: ctx.check(error.kind=="quota","high cannot bypass sender-target count quota")
    else: ctx.check(False,"ninth high message bypassed eight-record quota")
    plan = reserve_pressure_plan((await ctx.command("status")).get("limits") or {})
    control_target = ctx.agents[0]["id"]
    original_count = len(ctx.agents)
    await ctx.native.call("add-many", count=plan["targets"], prefix="i21-pressure-")
    await ctx.wait_agents(original_count + plan["targets"])
    targets = [agent for agent in ctx.agents if agent["backend_ref"].startswith("i21-pressure-")]
    ctx.check(len(targets) == plan["targets"], "fresh pressure targets prevent per-target quotas hiding the domain reserve")
    peers = []
    for index in range((len(targets) + 1) // 2):
        principal = "worker:priority-fill-" + str(index)
        await ctx.grant("priority-fill-" + str(index), principal, ["send:boundary"])
        peers.append(await ctx.connect(principal))
    ctx.evidence.append({"kind": "priority_pressure_plan", **{key: value for key, value in plan.items() if key != "text"},
                         "principals_pregranted": len(peers), "one_input_per_fresh_target": True})
    exhausted = False
    for index, agent in enumerate(targets):
        try:
            await ctx.send(peer=peers[index // 2], target=agent["id"], mode="boundary", priority="high", text=plan["text"], send_new=True)
        except RpcError as error:
            ctx.evidence.append({"kind": "priority_pressure_refusal", "error": error.error, "accepted_before": index})
            if not reserve_refusal(error):
                raise HarnessGap("legal I21 workload hit a non-reserve refusal: " + str(error)) from error
            exhausted = True
            ctx.check(True, "normal high-input saturation stops at the declared control-reserve fence", error=error.error)
            break
    if not exhausted: raise HarnessGap("declared control reserve not reached by legal128-input pressure; no reduced budget fixture qualifies")
    before=len(invocations(ctx,op="agent.send"))
    reply=await ctx.command("control",{"op":await ctx.operation(),"action":"interrupt","target":control_target,"args":{"v":0}})
    ctx.check(reply["state"] in ("accepted","observed","dispatched") and invocations(ctx,op="agent.interrupt"),"high-input saturation leaves the reserved control path usable")
    await asyncio.sleep(.1)
    ctx.check(len(invocations(ctx,op="agent.send"))==before,"saturated high input stays behind closed gates even while control succeeds")
