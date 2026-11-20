# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Part B composition, owner-epoch and pause-race probes (I11, I14, H4, H11).

Public primitives only: `coord.*` commands, owner workspace configuration, the native fixture's control verbs
(plan/release barriers, set, message_start) and the fixture's native ledger. No engine or store imports. Every
pass names an observed witness; a subclause the implementation does not provide fails with its evidence.
"""
import asyncio
from copy import deepcopy

from .cases import case, invocations, settled
from ..wire import RpcError


def pi_like_caps(agent):
    """The Pi declaration shape: start/admit/boundary/settle but no native preempt; queue clear + interrupt."""
    caps = deepcopy(agent["data"]["agent"]["caps"])
    modes = caps["agent.send"]["modes"]
    modes.pop("preempt", None)
    caps["rev"] = caps.get("rev", 1) + 1
    return caps


async def pi_agent(ctx, index=0):
    caps = pi_like_caps(ctx.agents[index])
    await ctx.native_set(index, caps=caps)
    return ctx.agents[index]


async def recipe(ctx, text, **fields):
    return await ctx.send(mode="preempt", strategy="interrupt_then_start", text=text, send_new=True, **fields)


def ops_for(ctx, delivery, after=0):
    """Native composition steps for one attempt, in ledger order (queue/interrupt carry no delivery id)."""
    rows = ctx.native.records("invoke", after=after)
    return [row["op"] for row in rows if row.get("args", {}).get("delivery") in (None, delivery)
            and row["op"] in ("agent.queue", "agent.interrupt", "agent.send")]


def steps_of(record):
    return [(step.get("op"), step.get("outcome")) for step in (record.get("attempt") or {}).get("steps", [])]


@case("I11")
async def composition_interrupt_then_start(ctx):
    """Pi gets only interrupt_then_start (clear, abort, start), never preempt; exclusive writers succeed;
    interference is partial with step outcomes; pause or deadline after the interrupt stops the input step;
    a definite prerequisite failure stops the recipe; a restart never continues it."""
    failed = []
    def sub(condition, message, **evidence):
        ctx.evidence.append({"kind": "subclause", "subclause": message, "passed": bool(condition), **evidence})
        if not condition: failed.append(message)
    pi = await pi_agent(ctx)
    ref = pi["backend_ref"]

    # 1. Native preempt is never used for Pi: a plain preempt is held, never invoked as preempt.
    mark = (await ctx.native.call("mark"))["seq"]
    plain = await ctx.send(mode="preempt", text="plain preempt to Pi", send_new=True)
    await ctx.scope("resume")
    await asyncio.sleep(1)
    record = await ctx.get(plain["delivery"])
    sends = [row for row in invocations(ctx, after=mark, op="agent.send") if row["args"].get("mode") == "preempt"]
    sub(not sends and record["state"] == "held", "a plain preempt to Pi is not composed or sent as native preempt",
        record={k: record.get(k) for k in ("state", "reason")})
    await ctx.scope("pause")
    record = await ctx.get(plain["delivery"])
    op = await ctx.operation("withdraw")
    await ctx.command("inbox.withdraw", {"op": op, "delivery": plain["delivery"], "expect": record.get("revision")})

    # 2. Exclusive writers: clear, interrupt, start, in that order; native sees start, never preempt.
    mark = (await ctx.native.call("mark"))["seq"]
    good = await recipe(ctx, "exclusive recipe")
    await ctx.scope("resume")
    done = await settled(ctx, good["delivery"])
    ops = ops_for(ctx, good["delivery"], mark)
    modes = [row["args"].get("mode") for row in invocations(ctx, after=mark, op="agent.send")]
    sub(done["state"] in ("accepted", "observed", "dispatched") and ops == ["agent.queue", "agent.interrupt", "agent.send"]
        and modes == ["start"], "exclusive writers: clear, abort, start succeeds with start (never preempt)",
        state=done["state"], ops=ops, modes=modes, steps=steps_of(done))
    await ctx.scope("pause")
    await ctx.native.call("point", ref=ref, point="idle")

    # 3. A competing start between the interrupt and the input step: partial with step outcomes.
    await ctx.native.call("plan", ref=ref, op="agent.interrupt", plan={"after": "i11-steer"})
    mark = (await ctx.native.call("mark"))["seq"]
    steer = await recipe(ctx, "recipe raced by a competing start")
    await ctx.scope("resume")
    await ctx.wait(lambda: invocations(ctx, after=mark, op="agent.interrupt"), label="interrupt in flight")
    await ctx.native.call("message_start", ref=ref)  # another writer starts a run after the abort
    await ctx.native.call("release", barrier="i11-steer")
    raced = await ctx.wait_command("inbox.get", lambda row: row.get("state") not in ("held", "recorded", "claimed"),
                                   args={"delivery": steer["delivery"]})
    sub(raced["state"] == "partial" and len(steps_of(raced)) == 3 and steps_of(raced)[-1][1] == "rejected",
        "a competing start after the interrupt is reported partial with per-step outcomes",
        state=raced["state"], reason=raced.get("reason"), steps=steps_of(raced))
    # coordination-v0 §5.7: each step is stored as a compact outcome reserved at admission. Let maintenance run
    # (any accepted mutation runs it) and read the stored record again.
    for _ in range(3):
        await ctx.scope("pause"); await ctx.scope("resume")
    stored = await ctx.get(steer["delivery"])
    sub(len(steps_of(stored)) == 3, "the per-step outcomes stay stored after maintenance (coordination-v0 §5.7)",
        state=stored["state"], steps=steps_of(stored), attempt=stored.get("attempt"), result=stored.get("result"),
        code="maintenance compaction keeps attempt {id} only (coordination_engine.py:1732-1738 at b9142ce)")
    await ctx.native.call("plan", ref=ref, op="agent.interrupt", plan={})
    await ctx.scope("pause")
    await ctx.native.call("point", ref=ref, point="idle")

    # 4. Pause after the interrupt stops the input step.
    await ctx.native.call("plan", ref=ref, op="agent.interrupt", plan={"after": "i11-pause"})
    mark = (await ctx.native.call("mark"))["seq"]
    paused = await recipe(ctx, "recipe paused after interrupt")
    await ctx.scope("resume")
    await ctx.wait(lambda: invocations(ctx, after=mark, op="agent.interrupt"), label="interrupt in flight")
    pause_task = asyncio.create_task(ctx.scope("pause"))
    await asyncio.sleep(.3)
    await ctx.native.call("release", barrier="i11-pause")
    await pause_task
    stopped = await settled(ctx, paused["delivery"])
    sub(not invocations(ctx, after=mark, op="agent.send") and stopped["state"] != "accepted",
        "pause after the interrupt stops the input step (no native start)",
        state=stopped["state"], reason=stopped.get("reason"), steps=steps_of(stopped))
    await ctx.native.call("plan", ref=ref, op="agent.interrupt", plan={})
    await ctx.native.call("point", ref=ref, point="idle")

    # 5. Deadline after the interrupt: the interrupt outlives the recipe deadline; no input step follows.
    await ctx.native.call("plan", ref=ref, op="agent.interrupt", plan={"after": "i11-deadline"})
    mark = (await ctx.native.call("mark"))["seq"]
    late = await recipe(ctx, "recipe past its deadline")
    await ctx.scope("resume")
    await ctx.wait(lambda: invocations(ctx, after=mark, op="agent.interrupt"), label="interrupt in flight")
    await asyncio.sleep(6)
    await ctx.native.call("release", barrier="i11-deadline")
    expired = await settled(ctx, late["delivery"])
    sub(not invocations(ctx, after=mark, op="agent.send") and expired["state"] != "accepted",
        "a deadline reached during the interrupt stops the input step",
        state=expired["state"], reason=expired.get("reason"), steps=steps_of(expired))
    await ctx.native.call("plan", ref=ref, op="agent.interrupt", plan={})
    await ctx.scope("pause")
    await ctx.native.call("point", ref=ref, point="idle")
    if expired["state"] == "unknown":
        # An unknown predecessor blocks this sender's lane (I22); resolve it explicitly so later steps can run.
        await ctx.command("inbox.resolve", {"op": await ctx.operation("resolve"), "delivery": late["delivery"], "as": "not_delivered"})

    # 6. A definite prerequisite failure (queue clear rejected) stops the recipe before interrupt/start.
    await ctx.native.call("plan", ref=ref, op="agent.queue",
                          plan={"result": {"outcome": "rejected", "error": {"kind": "invalid", "message": "fixture refuses clear"}}})
    mark = (await ctx.native.call("mark"))["seq"]
    refused = await recipe(ctx, "recipe with refused clear")
    await ctx.scope("resume")
    try: stopped = await settled(ctx, refused["delivery"])
    except Exception:
        stopped = await ctx.get(refused["delivery"])
        ctx.evidence.append({"kind": "unsettled_prerequisite_record", "record": stopped,
                             "ops": ops_for(ctx, refused["delivery"], mark)})
    sub(ops_for(ctx, refused["delivery"], mark) == ["agent.queue"] and stopped["state"] in ("rejected", "held"),
        "a definite prerequisite failure stops the recipe", state=stopped["state"], reason=stopped.get("reason"),
        ops=ops_for(ctx, refused["delivery"], mark), steps=steps_of(stopped))
    await ctx.native.call("plan", ref=ref, op="agent.queue", plan={})
    await ctx.scope("pause")

    # 7. Restart while the interrupt runs: the recipe is never continued.
    await ctx.native.call("plan", ref=ref, op="agent.interrupt", plan={"after": "i11-restart"})
    mark = (await ctx.native.call("mark"))["seq"]
    cut = await recipe(ctx, "recipe cut by restart")
    await ctx.scope("resume")
    await ctx.wait(lambda: invocations(ctx, after=mark, op="agent.interrupt"), label="interrupt in flight")
    await ctx.restart()
    await ctx.native.call("release", barrier="i11-restart")
    await ctx.scope("resume")
    await asyncio.sleep(1.5)
    after = await ctx.get(cut["delivery"])
    sub(not invocations(ctx, after=mark, op="agent.send") and after["state"] == "unknown",
        "a restart never continues a recipe (no start; the attempt is unknown)",
        state=after["state"], reason=after.get("reason"), steps=steps_of(after))
    await ctx.native.call("plan", ref=ref, op="agent.interrupt", plan={})
    ctx.check(not failed, "I11 subclauses", failed=failed)


@case("I14")
async def stale_owner_callback_cannot_invoke(ctx):
    """The inbox owner is replaced (extension restarted: new control_epoch) while a recipe's native callback runs.
    The old attempt never invokes its next step, and the new owner does not continue it."""
    pi = await pi_agent(ctx)
    ref = pi["backend_ref"]
    key = "extension/coord"
    entry = (await ctx.peer.call("workspace.snapshot"))["entries"].get(key)
    if not entry:
        raise RuntimeError("no extension/coord entry to replace the owner through public configuration")
    before = await ctx.command("status")
    await ctx.native.call("plan", ref=ref, op="agent.interrupt", plan={"after": "i14-callback"})
    mark = (await ctx.native.call("mark"))["seq"]
    sent = await recipe(ctx, "recipe whose owner is replaced")
    await ctx.scope("resume")
    await ctx.wait(lambda: invocations(ctx, after=mark, op="agent.interrupt"), label="callback (interrupt) in flight")
    claimed = await ctx.get(sent["delivery"])
    old_epoch = (claimed.get("attempt") or {}).get("control_epoch")
    # Replace the owner through ordinary configuration: disable, then re-enable the managed extension.
    current = (await ctx.peer.call("workspace.get", {"key": key}))
    await ctx.peer.call("workspace.apply", {"ops": [{"op": "set", "key": key, "expect": current["version"],
                                                     "value": dict(current["value"], enabled=False)}]})
    await asyncio.sleep(1)
    current = (await ctx.peer.call("workspace.get", {"key": key}))
    await ctx.peer.call("workspace.apply", {"ops": [{"op": "set", "key": key, "expect": current["version"],
                                                     "value": dict(current["value"], enabled=True)}]})
    refusals = []
    async def ready():
        try: row = await ctx.command("status")
        except RpcError as error:
            refusals.append(f"{error.kind}: {str(error)[:80]}"); return None
        return row if row.get("state") == "ready" else None
    status = await ctx.wait(ready, label="replacement owner ready", timeout=30)
    ctx.evidence.append({"kind": "owner_switch_refusals", "distinct": sorted(set(refusals)), "count": len(refusals)})
    await ctx.native.call("release", barrier="i14-callback")
    await ctx.scope("resume")
    await asyncio.sleep(2)
    record = await ctx.get(sent["delivery"])
    ctx.evidence.append({"kind": "owner_replacement", "old_attempt_epoch": old_epoch,
                         "status_before": {k: before.get(k) for k in ("state", "window")},
                         "status_after": {k: status.get(k) for k in ("state", "window")},
                         "record": {k: record.get(k) for k in ("state", "reason")}, "steps": steps_of(record)})
    ctx.check(not invocations(ctx, after=mark, op="agent.send"),
              "the stale owner's callback never invokes the next step, and the new owner does not continue it")
    ctx.check(record["state"] == "unknown", "the interrupted attempt is unknown, never pending or resent",
              state=record["state"], reason=record.get("reason"))
    ctx.check(len(invocations(ctx, after=mark, op="agent.interrupt")) == 1, "the native interrupt ran exactly once")
    await ctx.native.call("plan", ref=ref, op="agent.interrupt", plan={})


@case("H4")
async def pause_claim_race(ctx):
    """Pause wins: no claim and no native write. Claim wins: the pause reply reports the outstanding attempt."""
    failed = []
    # Pause wins (deterministic): the claim waits for native idle (start mode, runtime busy); pause commits first,
    # then the runtime goes idle. No claim and no native write may follow.
    await ctx.native_set(0, state="running", run="h4-busy-run")
    sent = await ctx.send(text="pause wins", send_new=True)
    await ctx.scope("resume")
    await ctx.wait_command("inbox.get", lambda row: row.get("reason") == "agent_busy", args={"delivery": sent["delivery"]})
    pause = await ctx.scope("pause")
    await ctx.native.call("point", ref=ctx.agents[0]["backend_ref"], point="idle")
    await asyncio.sleep(1.5)
    record = await ctx.get(sent["delivery"])
    won = not record.get("attempt") and not invocations(ctx, delivery=sent["delivery"]) and record["state"] == "held"
    ctx.evidence.append({"kind": "subclause", "subclause": "pause wins: no claim and no native write", "passed": won,
                         "record": {k: record.get(k) for k in ("state", "reason", "attempt")}, "pause_reply": pause})
    if not won:
        failed.append("pause wins: no claim and no native write")
    op = await ctx.operation("wd")
    await ctx.command("inbox.withdraw", {"op": op, "delivery": sent["delivery"], "expect": record.get("revision")})

    # Claim wins (deterministic): the native call is in flight when pause arrives.
    await ctx.native.call("plan", ref=ctx.agents[0]["backend_ref"], op="agent.send", plan={"before": "h4-claim"})
    sent = await ctx.send(text="claim wins", send_new=True)
    await ctx.scope("resume")
    await ctx.wait(lambda: invocations(ctx, delivery=sent["delivery"]), label="claimed attempt in flight")
    claimed = await ctx.get(sent["delivery"])
    attempt = (claimed.get("attempt") or {}).get("id")
    pause = await ctx.scope("pause")
    reported = attempt is not None and (attempt in str(pause) or sent["delivery"] in str(pause))
    ctx.evidence.append({"kind": "subclause", "subclause": "claim wins: the pause reply reports the outstanding attempt",
                         "passed": reported, "attempt": attempt, "pause_reply": pause})
    if not reported:
        failed.append("claim wins: the pause reply reports the outstanding attempt")
    await ctx.native.call("release", barrier="h4-claim")
    done = await settled(ctx, sent["delivery"])
    ctx.check(len(invocations(ctx, delivery=sent["delivery"])) == 1 and done["state"] in ("accepted", "observed", "dispatched"),
              "the claimed attempt completes once; pause neither cancels nor repeats it", state=done["state"])
    await ctx.native.call("plan", ref=ctx.agents[0]["backend_ref"], op="agent.send", plan={})
    ctx.check(not failed, "H4 subclauses", failed=failed)


@case("H11")
async def owner_restart_during_resume(ctx):
    """Resume is accepted, the claim it enables is cut at the real committer, and the owner restarts: the new owner
    starts closed, the lookup of the resume op reports its historical result, and no permit is created."""
    sent = await ctx.send()
    await ctx.restart(fault="claim-before")
    preview = await ctx.command("scope", {"action": "inspect", "scope": "agent:" + ctx.agents[0]["id"]})
    resume_op = await ctx.operation("resume-cut")
    accepted = await ctx.command("scope", {"op": resume_op, "action": "resume", "scope": "agent:" + ctx.agents[0]["id"],
                                           "expect": preview["expect"]})
    await ctx.wait_cut("before-claim-write")
    await ctx.restart(crash=True)
    lookup = None
    try:
        lookup = await ctx.command("inbox.op", {"op": resume_op})
    except RpcError as error:
        lookup = {"error": error.kind, "message": str(error)}
    await asyncio.sleep(1.5)
    record = await ctx.get(sent["delivery"])
    fresh = await ctx.send(text="sent after restart", send_new=True)
    await asyncio.sleep(1)
    fresh_record = await ctx.get(fresh["delivery"])
    ctx.evidence.append({"kind": "h11", "accepted": {k: accepted.get(k) for k in ("state", "receipt", "control_revision")},
                         "lookup": lookup, "record": {k: record.get(k) for k in ("state", "reason")},
                         "fresh": {k: fresh_record.get(k) for k in ("state", "reason")}})
    ctx.check(record["state"] == "held" and record.get("reason") == "paused" and fresh_record["state"] == "held"
              and fresh_record.get("reason") == "paused", "the restarted owner starts closed",
              record={k: record.get(k) for k in ("state", "reason")}, fresh={k: fresh_record.get(k) for k in ("state", "reason")})
    ctx.check(isinstance(lookup, dict) and lookup.get("state") == "accepted" and lookup.get("historical") is True,
              "the lookup of the resume op reports its historical result", lookup=lookup)
    ctx.check(not invocations(ctx, delivery=sent["delivery"]) and not invocations(ctx, delivery=fresh["delivery"]),
              "the historical resume creates no permit (no native write)")
