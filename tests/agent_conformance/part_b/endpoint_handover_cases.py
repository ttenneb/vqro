# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""T9: a plugin's structured endpoint supersedes a terminal marker by a recorded public handover.

Only public coordination commands drive it, and only the fake native ledger is witness: the native payload each
endpoint actually received, target identity at invoke time, and no typed terminal input or second invocation
(no replay). No coordination engine or store module is imported or asserted.
"""

from .cases import case, held, invocations, settled, terminal


def plan_matches(recorded, source, destination, deliveries, *, state, stage):
    """T9: the recorded plan names exactly this source/destination (ids and incarnations), delivery set and states."""
    return (recorded.get("state") == state and recorded.get("from") == source
            and {k: (recorded.get("to") or {}).get(k) for k in ("agent", "incarnation")} == destination
            and sorted(recorded.get("deliveries") or []) == sorted(deliveries)
            and [(st.get("kind"), st.get("state")) for st in recorded.get("stages") or []] == [("names", stage)])


def lookup_names_plan(inspected, plan_id):
    """T9: the handover op lookup is historical and returns exactly the recorded plan (never unknown or another plan)."""
    return inspected.get("plan") == plan_id and inspected.get("historical") is True and inspected.get("state") in ("held", "completed")


async def stored_plan(ctx, plan_id):
    """The recorded handover plan, read through the public operational API (never a store import)."""
    entry = (await ctx.peer.call("operational.get", {"domain": "coord", "key": "operational/coord/plan/" + plan_id})).get("entry")
    return (entry or {}).get("value") or {}


async def scheduling_barrier(ctx, endpoint, text):
    """A harmless later settle through the same open destination: its durable claim and native invocation prove the
    scheduler ran over that destination (settle is the last lane it considers); the endpoint is then made idle again so
    the protected records keep their native eligibility. Returns the barrier delivery, which callers exclude."""
    barrier = (await ctx.send(target=endpoint["id"], mode="settle", text=text, send_new=True))["delivery"]
    await settled(ctx, barrier)
    calls = invocations(ctx, delivery=barrier)
    claim = (((calls[0].get("witness") or {}).get("entry") or {}).get("value") or {}) if calls else {}
    ctx.check(len(calls) == 1 and calls[0]["ref"] == endpoint["backend_ref"] and claim.get("state") == "claimed"
              and (claim.get("to") or {}).get("agent") == endpoint["id"],
              "a later input through the same open destination was durably claimed and invoked (scheduler ran)",
              barrier=barrier, claim=claim.get("to"))
    await ctx.native.call("point", ref=endpoint["backend_ref"], point="idle")
    return barrier


async def structured_endpoint(ctx, ref):
    """The plugin's own structured agent endpoint, published by the native backend (not a marker)."""
    await ctx.native.call("add", ref=ref, fields={"kind": "agent"})
    async def found():
        return next((row for row in await ctx.resources()
                     if row["backend_ref"] == ref and row.get("available") and row.get("kind") == "agent"), None)
    return await ctx.wait(found, label="public structured plugin endpoint")


@case("T9")
async def plugin_endpoint_supersedes_marker_by_recorded_handover(ctx):
    # A marked terminal (OSC-shaped app status, no plugin): mail to it is held, never typed.
    marker = await terminal(ctx)
    await ctx.command("names", {"op": await ctx.operation(), "handle": "@t9-worker", "agent": marker["id"], "expect": 0})
    texts = {"start": "T9-first-held-at-the-marker", "settle": "T9-second-held-at-the-marker"}
    sent = {mode: (await ctx.send(target="@t9-worker", mode=mode, text=text, send_new=True))["delivery"]
            for mode, text in texts.items()}
    await ctx.scope("resume", "agent:" + marker["id"])
    for delivery in sent.values():
        record = await held(ctx, delivery, "integration_unavailable")
        ctx.check(record["to"]["agent"] == marker["id"], "the handle resolved to the marked terminal", record=record)
    ctx.check(not ctx.native.records("terminal_input") and not invocations(ctx),
              "a marked terminal without a plugin is never typed into or invoked")

    # The plugin's structured endpoint appears; the marker target is retired administratively (it stays a usable
    # terminal), then a recorded handover moves the exact held list and rebinds the handle.
    plugin = await structured_endpoint(ctx, "t9-plugin")
    await ctx.scope("resume", "agent:" + plugin["id"])  # open destination: pending handover must still not deliver
    retired = await ctx.command("control", {"op": await ctx.operation(), "action": "retire", "target": marker["id"],
                                            "args": {"reason": "superseded by its plugin's structured endpoint"}})
    ctx.check(retired.get("state") == "retired", "the marker target is retired administratively", reply=retired)
    handle = (await ctx.peer.call("workspace.snapshot"))["entries"]["doc/agents/names/@t9-worker"]["version"]
    handover_op = await ctx.operation("handover")
    plan = await ctx.command("handover", {"op": handover_op,
        "from": {"agent": marker["id"], "incarnation": marker["incarnation"]},
        "to": {"agent": plugin["id"], "incarnation": plugin["incarnation"]},
        "deliveries": list(sent.values()), "bind": [{"handle": "@t9-worker", "agent": plugin["id"], "expect": handle}]})
    ctx.check(plan.get("state") == "held" and plan.get("reason") == "handover_pending" and isinstance(plan.get("plan"), str),
              "the handover is recorded as a pending plan", reply=plan)
    recorded = await stored_plan(ctx, plan["plan"])
    source = {"agent": marker["id"], "incarnation": marker["incarnation"]}
    destination = {"agent": plugin["id"], "incarnation": plugin["incarnation"]}
    ctx.check(plan_matches(recorded, source, destination, sent.values(), state="pending", stage="pending"),
              "the recorded plan names the exact source and destination ids and incarnations, delivery set and pending bind stage",
              recorded=recorded)
    for delivery in sent.values():
        record = await held(ctx, delivery, "handover_pending")
        came_from = record.get("handed_over_from") or {}
        ctx.check(record["to"]["agent"] == plugin["id"] and record["to"]["incarnation"] == plugin["incarnation"]
                  and came_from.get("agent") == marker["id"] and came_from.get("incarnation") == marker["incarnation"]
                  and record.get("handover_plan") == plan["plan"],
                  "the record names the exact plugin endpoint, the exact marker id and incarnation it came from, and the plan",
                  record=record)
    barriers = [await scheduling_barrier(ctx, plugin, "T9-barrier-while-pending")]
    reasons = [(await ctx.get(delivery))["reason"] for delivery in sent.values()]
    ctx.check(all(not invocations(ctx, delivery=delivery) for delivery in sent.values()) and not ctx.native.records("terminal_input")
              and reasons == ["handover_pending"] * len(sent),
              "a pending handover delivers nothing although the open destination's scheduler ran", reasons=reasons)

    stage = await ctx.command("handover", {"op": await ctx.operation(), "action": "stage", "plan": plan["plan"], "stage": 0})
    ctx.check(stage.get("state") == "accepted", "the handle rebind stage is accepted", reply=stage)
    staged = await stored_plan(ctx, plan["plan"])
    ctx.check(plan_matches(staged, source, destination, sent.values(), state="pending", stage="accepted"),
              "the recorded bind stage is accepted while the plan is still pending", recorded=staged)
    done = await ctx.command("handover", {"op": await ctx.operation(), "action": "complete", "plan": plan["plan"]})
    ctx.check(done.get("state") == "completed", "the handover completes after every stage", reply=done)
    completed = await stored_plan(ctx, plan["plan"])
    ctx.check(plan_matches(completed, source, destination, sent.values(), state="completed", stage="accepted"),
              "the recorded plan is completed for the same source, destination and delivery set", recorded=completed)

    # Delivery now reaches the plugin endpoint natively: exact payload, exact target, once each.
    for mode, delivery in sent.items():
        record = await settled(ctx, delivery)
        calls = invocations(ctx, delivery=delivery)
        ctx.check(len(calls) == 1, "each handed-over record is invoked exactly once", delivery=delivery, calls=len(calls))
        call = calls[0]
        claim = ((call.get("witness") or {}).get("entry") or {}).get("value") or {}
        ctx.check(call["op"] == "agent.send" and call["ref"] == plugin["backend_ref"]
                  and call["args"].get("mode") == mode and call["args"].get("delivery") == delivery
                  and call["args"].get("content") == [{"type": "text", "text": texts[mode]}],
                  "the plugin endpoint received the original payload for that delivery", call=call["args"])
        ctx.check(claim.get("to", {}).get("agent") == plugin["id"] and claim.get("to", {}).get("incarnation") == plugin["incarnation"],
                  "the durable claim seen at invoke time targets the plugin endpoint", claim=claim.get("to"))
    # The handle now supersedes the marker: new mail by handle goes to the plugin endpoint.
    # (settle: the endpoint is running its handed-over start, so a start would be held busy)
    later = (await ctx.send(target="@t9-worker", mode="settle", text="T9-after-supersede", send_new=True))["delivery"]
    await settled(ctx, later)
    after = invocations(ctx, delivery=later)
    ctx.check(len(after) == 1 and after[0]["ref"] == plugin["backend_ref"]
              and (await ctx.get(later))["to"]["agent"] == plugin["id"], "the rebound handle addresses the plugin endpoint")
    # No replay: a further resume, then a later input the scheduler actually claims and invokes; still exactly one
    # invocation per handed-over record and nothing typed into the terminal.
    await ctx.native.call("point", ref=plugin["backend_ref"], point="idle")
    await ctx.scope("resume", "agent:" + plugin["id"])
    barriers.append(await scheduling_barrier(ctx, plugin, "T9-barrier-after-resume"))
    counts = {delivery: len(invocations(ctx, delivery=delivery)) for delivery in [*sent.values(), later]}
    ctx.check(all(count == 1 for count in counts.values()) and not ctx.native.records("terminal_input"),
              "no typed terminal input and no second invocation after the handover, although the scheduler ran again",
              counts=counts, barriers=barriers)
    inspected = await ctx.command("inbox.op", {"op": handover_op})
    ctx.check(lookup_names_plan(inspected, plan["plan"]),
              "the handover operation lookup returns the exact recorded plan", lookup=inspected)
    ctx.evidence.append({"kind": "t9_handover", "marker": marker["id"], "plugin": plugin["id"], "plan": plan["plan"],
                         "deliveries": list(sent.values()), "handover_op": inspected,
                         "native_invokes": [{"delivery": c["args"]["delivery"], "ref": c["ref"], "seq": c["seq"]}
                                            for c in invocations(ctx)]})
