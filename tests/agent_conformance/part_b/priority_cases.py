# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Priority ordering never grants admission, execution or reserved capacity."""
import asyncio
from .cases import case, invocations, settled, held
from .inbox_cases import pending_mutation_claim_races
from ..wire import RpcError
from ..results import HarnessGap


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
    exhausted=False
    for index in range(64):
        principal="worker:priority-fill-"+str(index);peer=await ctx.connect(principal)
        await ctx.grant("priority-fill-"+str(index),principal,["send:boundary"])
        for agent in ctx.agents:
            try: await ctx.send(peer=peer,target=agent["id"],mode="boundary",priority="high",text="x"*60000,send_new=True)
            except RpcError as error:
                ctx.check(error.kind=="quota" and "reserve" in str(error),"normal high-input saturation stops at control-reserve fence",error=error.error)
                exhausted=True;break
        if exhausted: break
    if not exhausted: raise HarnessGap("declared control reserve not reached by128 real high-priority inputs; no reduced budget fixture qualifies")
    before=len(invocations(ctx,op="agent.send"))
    reply=await ctx.command("control",{"op":await ctx.operation(),"action":"interrupt","target":ctx.agents[0]["id"],"args":{"v":0}})
    ctx.check(reply["state"] in ("accepted","observed","dispatched") and invocations(ctx,op="agent.interrupt"),"high-input saturation leaves the reserved control path usable")
    await asyncio.sleep(.1)
    ctx.check(len(invocations(ctx,op="agent.send"))==before,"saturated high input stays behind closed gates even while control succeeds")
