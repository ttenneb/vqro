# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Public pull followups; wake hints are observed live, never manufactured."""
import asyncio
import json

from .cases import case, terminal, user_pull, consumer, take, insert_pull, invocations


def target_wakes(events, target):
    rows = []
    for event in events:
        data = event.get("data") or {}
        if event.get("type") != "view.content" or data.get("view") != "coord-status":
            continue
        content = data.get("content") or {}
        wake = content.get("wake")
        if isinstance(wake, dict) and target in (wake.get("pending") or {}):
            rows.append(content)
    return rows


@case("T7")
async def paused_take_and_same_count_edit_wake(ctx):
    row = await terminal(ctx, pull=user_pull())
    target = row["id"]
    await consumer(ctx, row)
    events = []
    monitor = await ctx.connect("owner")
    monitor.on("event", events.append)
    await monitor.call("events.subscribe", {"view_content": {"names": ["coord-status"]}})
    first_text = "T7-private-content-never-in-a-wake"
    sent = await ctx.send(target=target, mode="boundary", text=first_text)
    before = await ctx.get(sent["delivery"])
    paused = await take(ctx, row)
    ctx.check(paused.get("held") == "paused" and not paused.get("records") and not paused.get("claims"),
              "a paused target's actual take returns held paused, without a claim", reply=paused)
    paused_text = "T7-edited-while-paused"
    await ctx.command("inbox.edit", {"op": await ctx.operation(), "delivery": sent["delivery"],
        "expect": before["revision"], "content": [{"type": "text", "text": paused_text}]})
    edited = await ctx.get(sent["delivery"])
    await asyncio.sleep(.3)
    ctx.check(edited["revision"] > before["revision"] and not target_wakes(events, target),
              "editing held mail advances its record revision but a paused target receives no wake",
              before=before["revision"], after=edited["revision"])
    ctx.check(not invocations(ctx, delivery=sent["delivery"]) and not ctx.native.records("terminal_input")
              and not ctx.native.records("model_request"), "paused take/edit performs no native input")
    await ctx.scope("resume", "agent:" + target)
    initial = await ctx.wait(lambda: target_wakes(events, target), label="actual resume count-only wake")
    first = initial[-1]
    wake = first["wake"]
    ctx.check(set(wake) == {"seq", "pending"} and type(wake["seq"]) is int and wake["pending"] == {target: 1}
              and type(first.get("control_revision")) is int and isinstance(first.get("control_epoch"), str),
              "wake contains only the pending count and actual revisions, never authority or content", wake=first)
    mark = len(events)
    final_text = "T7-open-edit-with-the-same-count"
    await ctx.command("inbox.edit", {"op": await ctx.operation(), "delivery": sent["delivery"],
        "expect": edited["revision"], "content": [{"type": "text", "text": final_text}]})
    async def changed_wake():
        candidates = target_wakes(events[mark:], target)
        return next((item for item in candidates if item["wake"].get("seq", -1) > wake["seq"]), None)
    later = await ctx.wait(changed_wake, label="same-count edit produces a newer wake")
    ctx.check(later["wake"]["pending"] == wake["pending"] and later["wake"]["seq"] > wake["seq"],
              "an edit wakes the open plugin with a newer revision even when counts are unchanged", before=first, after=later)
    for item in target_wakes(events, target):
        encoded = json.dumps(item)
        ctx.check(all(text not in encoded for text in (first_text, paused_text, final_text)) and
                  set(item["wake"]) == {"seq", "pending"}, "every observed wake is a count-only hint", wake=item)
    batch = await take(ctx, row)
    ctx.check(len(batch.get("records", [])) == 1 and batch["records"][0]["delivery"] == sent["delivery"]
              and batch["records"][0]["content"] == [{"type": "text", "text": final_text}],
              "the plugin still needs its own gated take and receives only the final edited content", take=batch)
    await insert_pull(ctx, row, batch)
    await ctx.native.call("model", ref=row["backend_ref"])
    requests = ctx.native.records("model_request")
    ctx.check(len(requests) == 1 and requests[0]["deliveries"] == [sent["delivery"]],
              "the three count hints produce only one actual model input, after the explicit take")
