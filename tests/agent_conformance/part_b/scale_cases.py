# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Part B capacity, quota, funding, archive and scale probes (I9, I18, I19, I23, L7, L8, L11, S1).

Only public primitives and the owned native oracle. Every pass names its observed witness; limits come from the
target's declared `coord.status` limits, never from constants copied out of an implementation.
"""
from .cases import case, invocations, settled
from ..wire import RpcError


async def sub_coordination(ctx, name, agents):
    """Private sub-fixture; callers own bounded close and retain its evidence."""
    from copy import deepcopy
    from .context import PublicCoordination
    manifest=deepcopy(ctx.manifest);manifest["agents"]=agents
    sub=PublicCoordination(ctx.root/name,manifest,ctx.vqro)
    await sub.start()
    return sub


async def declared_limits(ctx):
    status = await ctx.command("status")
    limits = status.get("limits") or {}
    ctx.evidence.append({"kind": "declared_limits", "limits": limits})
    return limits


async def flood_until_quota(ctx, peer, target, *, text, cap=4096, **fields):
    """Send distinct records until the target refuses `quota`; returns (accepted deliveries, refusal)."""
    accepted = []
    for index in range(cap):
        try: sent = await ctx.send(peer=peer, target=target, text=f"{text}-{index}", send_new=True, **fields)
        except RpcError as error:
            if error.kind == "quota": return accepted, error
            raise
        accepted.append(sent["delivery"])
    ctx.check(False, f"no quota refusal within {cap} sends", accepted=len(accepted))


@case("I18")
async def sender_fairness_and_quota(ctx):
    """One sender floods a target up to quota; another sender's equal-priority record is served within one round;
    over-quota sends, including `send_new`, are refused `quota` and write nothing."""
    target = ctx.agents[0]["id"]
    flooder, other = await ctx.connect("i18-flooder"), await ctx.connect("i18-other")
    for name in ("i18-flooder", "i18-other"):
        await ctx.grant("i18-" + name, name, ["send:admit"], "agent:" + target)
    flood, refusal = await flood_until_quota(ctx, flooder, target, text="flood", mode="admit")
    ctx.check(len(flood) >= 2, "the flood builds a real backlog before the quota", flood=len(flood))
    ctx.check(refusal.kind == "quota", "over-quota send_new is refused quota", refusal=str(refusal))
    rows = (await ctx.command("inbox.list", {"target": target, "sender": "i18-flooder"}))["items"]
    ctx.check(len(rows) == len(flood), "the refused send wrote no record", listed=len(rows), accepted=len(flood))
    fair = await ctx.send(peer=other, target=target, text="other sender, equal priority", mode="admit")
    await ctx.scope("resume", "agent:" + target)
    await settled(ctx, fair["delivery"])
    for delivery in flood: await settled(ctx, delivery)
    order = [row["args"]["delivery"] for row in invocations(ctx, op="agent.send")
             if row["args"].get("delivery") in set(flood) | {fair["delivery"]}]
    position = order.index(fair["delivery"])
    ctx.check(position <= 1, "the second sender is served within one round of the flood (2 senders)",
              position=position, served=len(order), flood=len(flood))
    ctx.check(len(order) == len(flood) + 1 and len(set(order)) == len(order), "every admitted record reached native exactly once")
