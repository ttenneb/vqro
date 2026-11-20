# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Inbox history and race probes; only public primitives and the owned oracle."""
import asyncio
from .cases import case, invocations, native_idle, settled, terminal, user_pull, consumer, take, insert_pull
from ..results import HarnessGap
from ..wire import RpcError


@case("I5")
async def pending_mutation_claim_races(ctx,priority=False):
    editor=await ctx.connect("owner");winners=set()
    for withdraw in (False,True):
        for ordering in ("edit-first","claim-first","concurrent"):
            await ctx.scope("pause");await native_idle(ctx)
            barrier="claim-race-"+str(withdraw)
            await ctx.native.call("plan",ref=ctx.agents[0]["backend_ref"],op="agent.send",plan={"before":barrier} if ordering=="claim-first" else {})
            sent=await ctx.send(text="old payload",priority="low" if priority else "normal");record=await ctx.get(sent["delivery"])
            command="inbox.withdraw" if withdraw else "inbox.edit"
            change={"op":await ctx.operation(),"delivery":sent["delivery"],"expect":record["revision"]}
            if not withdraw:
                change["content"]=[{"type":"text","text":"new payload"}]
                if priority: change["priority"]="high"
            async def update():
                try: await ctx.command(command,change,peer=editor);return True
                except RpcError as error:
                    ctx.check(error.kind in ("too_late","conflict"),"claim winner refuses stale pending mutation")
                    return False
            if ordering=="edit-first":
                edited=await update();await ctx.scope("resume")
            elif ordering=="claim-first":
                await ctx.scope("resume")
                await ctx.wait(lambda:invocations(ctx,delivery=sent["delivery"]),label="durable claim before competing mutation")
                ctx.check((await ctx.get(sent["delivery"]))["state"]=="claimed","claim-first cut is before native reply, not settled history")
                edited=await update();await ctx.native.call("release",barrier=barrier)
            else: edited,_=await asyncio.gather(update(),ctx.scope("resume"))
            winners.add(edited)
            if withdraw and edited:
                ctx.check((await ctx.get(sent["delivery"]))["state"]=="withdrawn" and not invocations(ctx,delivery=sent["delivery"]),"winning pre-claim withdraw sends nothing")
            else:
                await settled(ctx,sent["delivery"]);calls=invocations(ctx,delivery=sent["delivery"])
                expected="new payload" if edited and not withdraw else "old payload"
                ctx.check(len(calls)==1 and calls[0]["args"]["content"]==[{"type":"text","text":expected}],"one linearization winner determines the sole native payload")
                if priority:
                    expected_priority="high" if edited and not withdraw else "low"
                    ctx.check(calls[0]["disk_witness"]["entry"]["value"]["priority"]==expected_priority,"priority edit and claim have the same durable winner")
    ctx.check(winners=={True,False},"both winners exercised, not only a lucky race outcome")


async def age_out_history(ctx,delivery,operation=None,*,settle_timeout=8):
    """Discover actual eviction; no smaller retained-count substitute can pass."""
    await ctx.scope("resume")
    for index in range(4096):
        sent=await ctx.send(mode="admit",text="retention-"+str(index));await settled(ctx,sent["delivery"],timeout=settle_timeout)
        if index%32==31:
            old=await ctx.get(delivery)
            lookup=await ctx.command("inbox.op",{"op":operation}) if operation else {"state":"unknown"}
            if old.get("state")=="unknown" and lookup.get("state")=="unknown":
                ctx.evidence.append({"kind":"observed_retention_eviction","delivered":index+1,"old_delivery":delivery,"old_operation":operation})
                return index+1
    raise HarnessGap("retention exceeds4096 real deliveries; eviction not reached, no reduced fixture qualifies")


@case("I6")
async def retained_settled_input_never_requalifies(ctx):
    await ctx.scope("resume")
    first=await ctx.send(mode="admit",text="oldest settled delivery");await settled(ctx,first["delivery"])
    count=await age_out_history(ctx,first["delivery"]);before=len(invocations(ctx))
    ctx.check(before==count+1,"every retention input reached native exactly once")
    await ctx.restart();await ctx.scope("resume");await asyncio.sleep(.15)
    calls=invocations(ctx)
    ctx.check(len(calls)==before and len({row["args"]["delivery"] for row in calls})==before,
              "eviction and restart never make a prior delivery eligible again")


@case("I10")
async def evicted_operation_replay_is_not_new_input(ctx):
    row=await terminal(ctx,pull=user_pull());await consumer(ctx,row)
    send_op=await ctx.operation();send_args={"op":send_op,"submission":send_op,"to":row["id"],"mode":"boundary","content":[{"type":"text","text":"old operation"}]}
    sent=await ctx.command("inbox.send",send_args);await ctx.scope("resume","agent:"+row["id"])
    batch=await take(ctx,row);await ctx.native.call("entry",ref=row["backend_ref"],records=batch["records"])
    await ctx.command("inbox.report",{"op":await ctx.operation(),"consumer":"consumer-1","stage":"inserted",**batch["claims"][0]})
    await ctx.native.call("model",ref=row["backend_ref"])
    await ctx.native.call("point",ref=row["backend_ref"],point="idle")
    report_op=await ctx.operation();report_args={"op":report_op,"consumer":"consumer-1","stage":"read",**batch["claims"][0]}
    await ctx.command("inbox.report",report_args)
    # History construction observes settlement, not a dispatch-latency bound (same60s observation window as L8).
    await age_out_history(ctx,sent["delivery"],report_op,settle_timeout=60)
    before=len(invocations(ctx));entries=len((await ctx.native.call("session",ref=row["backend_ref"]))["entries"])
    async def replay_old():
        for command,args in (("inbox.send",send_args),("inbox.report",report_args)):
            try: reply=await ctx.command(command,args)
            except RpcError as error:
                ctx.check(error.kind in ("expired_operation","too_late","not_found","conflict"),"old operation refuses without claiming absence means no effect")
            else: ctx.check(not reply.get("records") and reply.get("delivery") in (None,sent["delivery"]),"replay observes history, not new content",reply=reply)
        ctx.check((await ctx.command("inbox.op",{"op":report_op}))["state"]=="unknown" and (await ctx.get(sent["delivery"]))["state"]=="unknown",
                  "evicted operation and delivery stay explicitly unknown, not resurrected")
    await replay_old();await ctx.restart();await replay_old()
    ctx.check(len(invocations(ctx))==before and len((await ctx.native.call("session",ref=row["backend_ref"]))["entries"])==entries,
              "replays after eviction and a new control epoch create no native insertion")
    fresh=await ctx.operation();args={"op":fresh,"submission":fresh,"to":ctx.agents[1]["id"],"mode":"start","content":[{"type":"text","text":"one intent"}]}
    await ctx.command("inbox.send",args)
    await ctx.rejected("inbox.send",dict(args,content=[{"type":"text","text":"different intent"}]),{"conflict"})
