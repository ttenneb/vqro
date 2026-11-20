# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Part B capacity, quota and scale probes (I9, I18, I19, L8, S1). I23/L11 live in archive_cases.py.

Only public primitives and the owned native oracle. Every pass names its observed witness; limits come from the
target's declared `coord.status` limits, never from constants copied out of an implementation.
"""
import asyncio
import hashlib
import json
import os
import time
from pathlib import Path

from .cases import case, invocations, settled, ordinary
from ..wire import RpcError


def canonical(value):
    return json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode()


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


@case("I9")
async def budgets_are_atomic_and_outcomes_compact(ctx):
    """Budgets refuse atomically without blocking layout or gates; every attempt and recipe step reserves a compact
    1 KiB outcome at admission; a large native result is stored as size/sha256/incomplete while the live caller
    gets it unchanged."""
    target, other = ctx.agents[0]["id"], ctx.agents[1]["id"]
    expiry = lambda: int(time.time() * 1000) + 600000
    # Fill one sender/target byte quota with large records.
    filled = []
    while True:
        try: filled.append((await ctx.send(target=target, mode="preempt", expires_at_ms=expiry(), send_new=True,
                                           text="f" * 30000))["delivery"])
        except RpcError as error:
            ctx.check(error.kind == "quota", "the fill ends at a quota refusal", refusal=str(error)); break
        ctx.check(len(filled) < 64, "the byte quota binds before 64 records")
    # Descend in 512-byte steps (< the 2 KiB difference between 3 and 1 reserved steps): at the first size where any
    # send fits, the 1-step send must fit and the identical 3-step recipe must be refused quota.
    before = len((await ctx.command("inbox.list", {"target": target}))["items"])
    witness = None
    for size in range(30000, -1, -512):
        text = "r" * max(size, 1)
        try:
            await ctx.send(target=target, mode="preempt", strategy="interrupt_then_start", expires_at_ms=expiry(),
                           send_new=True, text=text)
            ctx.check(False, "a 3-step recipe fitted where its 1-step send had not", size=size)
        except RpcError as error:
            ctx.check(error.kind == "quota", "a recipe that does not fit is refused quota", size=size, refusal=str(error))
            after = len((await ctx.command("inbox.list", {"target": target}))["items"])
            ctx.check(after == before, "the refused recipe wrote nothing", before=before, after=after)
        try: one = await ctx.send(target=target, mode="preempt", expires_at_ms=expiry(), send_new=True, text=text)
        except RpcError as error:
            ctx.check(error.kind == "quota", "only quota refuses the fill probe", refusal=str(error)); continue
        witness = {"content_bytes": len(text), "accepted_one_step": one["delivery"], "filled": len(filled)}
        break
    ctx.check(witness is not None, "a size exists where 1 reserved step fits and 3 do not", **(witness or {}))
    # Saturation blocks neither layout writes nor gates.
    await ordinary(ctx, [{"op": "set", "key": "doc/part-b/i9-layout", "value": {"saturated": True}, "expect": 0}])
    entry = (await ctx.peer.call("workspace.snapshot"))["entries"].get("doc/part-b/i9-layout") or {}
    ctx.check(entry.get("value") == {"saturated": True}, "an ordinary layout write commits while the quota is saturated (read back)", entry=entry)
    paused = await ctx.scope("pause", "agent:" + target)
    ctx.check(paused.get("state") not in (None, "rejected", "unknown"), "a gate still closes while saturated", reply=paused)
    # A large native result: compact stored evidence, unchanged live reply.
    blob = {"outcome": "accepted", "data": {"receipt": {"stage": "admitted"}, "blob": "b" * 4096}}
    await ctx.native.call("plan", ref=ctx.agents[1]["backend_ref"], op="agent.send", plan={"result": blob})
    sent = await ctx.send(target=other, mode="admit", text="large native result")
    await ctx.scope("resume", "agent:" + other)
    record = await settled(ctx, sent["delivery"])
    native = (record.get("result") or {}).get("native") or {}
    ctx.check(native.get("incomplete") is True and native.get("bytes", 0) > 1024 and len(canonical(native)) <= 1024,
              "a send's large native result is stored compactly (size, incomplete) within 1 KiB", stored=native)
    ctx.check(native["bytes"] >= len(canonical(blob)), "the stored size covers the whole native result",
              stored=native["bytes"], planned=len(canonical(blob)))
    steps = (record.get("attempt") or {}).get("steps") or []
    # A settled record may already be compacted to its summary (attempt id only); its stored outcome is result.native.
    ctx.check(all(len(canonical(step)) <= 1024 for step in steps), "every stored attempt step is at most 1 KiB",
              steps=steps, compacted=not steps)
    await ctx.native.call("plan", ref=ctx.agents[1]["backend_ref"], op="agent.interrupt", plan={"result": blob})
    op = await ctx.operation("interrupt")
    live = await ctx.command("control", {"op": op, "action": "interrupt", "target": other, "args": {"v": 0}})
    reply = live.get("native") or {}
    ctx.check(reply.get("outcome") == blob["outcome"] and (reply.get("data") or {}).get("blob") == blob["data"]["blob"],
              "the live caller gets the large native result whole", live_bytes=len(canonical(reply)),
              live_keys=sorted(reply), live_data_keys=sorted(reply.get("data") or {}))
    stored = await ctx.command("inbox.op", {"op": op})
    evidence = stored.get("native") or stored.get("coordination") or {}
    ctx.check(evidence.get("incomplete") is True and evidence.get("sha256") == hashlib.sha256(canonical(reply)).hexdigest()
              and evidence.get("bytes") == len(canonical(reply)) and len(canonical(evidence)) <= 1024,
              "the stored control outcome is compact: size and sha256 of exactly what the live caller got", stored=evidence)
    # A compact-outcome write failure leaves the attempt unknown and is never resent (real committer fault at the
    # outcome write of the same attempt).
    await ctx.restart(fault="outcome-error")
    await ctx.native.call("plan", ref=ctx.agents[1]["backend_ref"], op="agent.send", plan={})
    failing = await ctx.send(target=other, mode="admit", text="outcome write fails", send_new=True)
    await scope(ctx, "resume", "agent:" + other)
    await ctx.wait_cut("before-outcome-write")
    record = await ctx.wait_command("inbox.get", lambda row: row.get("state") not in ("recorded", "held", "claimed"),
                                    args={"delivery": failing["delivery"]})
    ctx.check(record.get("state") == "unknown" and len(invocations(ctx, delivery=failing["delivery"])) == 1,
              "a failed outcome write leaves the attempt unknown after exactly one native input",
              state=record.get("state"), reason=record.get("reason"), invocations=len(invocations(ctx, delivery=failing["delivery"])))
    await ctx.restart(crash=True)
    await scope(ctx, "resume", "agent:" + other)
    await asyncio.sleep(.5)
    restored = await ctx.get(failing["delivery"])
    ctx.check(restored.get("state") == "unknown" and len(invocations(ctx, delivery=failing["delivery"])) == 1,
              "after restart and resume the unknown attempt is never resent", state=restored.get("state"),
              invocations=len(invocations(ctx, delivery=failing["delivery"])))
    # A recipe stores one compact outcome per step.
    await ctx.native.call("plan", ref=ctx.agents[1]["backend_ref"], op="agent.send", plan={})
    recipe = await ctx.send(target=other, mode="preempt", strategy="interrupt_then_start", expires_at_ms=expiry(),
                            text="recipe", send_new=True)
    record = await ctx.wait_command("inbox.get", lambda row: row.get("state") not in ("recorded", "claimed"),
                                    args={"delivery": recipe["delivery"]})
    steps = (record.get("attempt") or {}).get("steps") or []
    ctx.check(all(len(canonical(step)) <= 1024 for step in steps), "every stored recipe step is at most 1 KiB", steps=steps)
    ctx.check(len(steps) == 3, "the 3-step recipe ran all its steps, each with a compact outcome "
              "(agent.queue needs `epoch`, agent-v0 §6)", state=record.get("state"), reason=record.get("reason"),
              steps=steps, queue_invocations=[row["args"] for row in invocations(ctx, op="agent.queue")])


async def sub_coordination(ctx, name, agents):
    """A private sub-fixture with more native agents (the manifest default is 2); the caller closes it."""
    from copy import deepcopy
    from .context import PublicCoordination
    manifest = deepcopy(ctx.manifest); manifest["agents"] = agents
    sub = PublicCoordination(ctx.root / name, manifest, ctx.vqro)
    await sub.start()
    return sub


async def saturate_domain(ctx, sub, targets, *, senders=16, size=30000):
    """Fill the normal budget with open records from many senders over many targets; returns accepted deliveries
    per target and the proof refusal: a fresh sender's 1-byte send to the least-loaded target refused `quota`."""
    peers = []
    for index in range(senders):
        name = f"sat-s{index}"
        await sub.grant("sat-" + name, name, ["send:admit"], "*")
        peers.append(await sub.connect(name))
    accepted = {row["id"]: [] for row in targets}
    refusals = {}
    for chunk in (size, size // 4, size // 16, size // 64, 1):
      for row in targets:
        for peer in peers:
            while True:
                try: accepted[row["id"]].append((await sub.send(peer=peer, target=row["id"], mode="admit",
                                                                 send_new=True, text="s" * chunk))["delivery"])
                except RpcError as error:
                    if error.kind not in ("quota", "busy"): raise
                    refusals[error.kind] = refusals.get(error.kind, 0) + 1
                    break
    await sub.grant("sat-probe", "sat-probe", ["send:admit"], "*")
    probe = await sub.connect("sat-probe")
    lightest = min(targets, key=lambda row: len(accepted[row["id"]]))
    try:
        sent = await sub.send(peer=probe, target=lightest["id"], mode="admit", text="p")
        ctx.check(False, "a fresh sender's 1-byte send is refused at the durable budget", admitted=sent, fill_refusals=refusals,
                  per_target={k: len(v) for k, v in accepted.items()}, limits=(await sub.command("status")).get("limits"))
    except RpcError as error:
        refusal = {"kind": error.kind, "message": str(error), "open_records": sum(map(len, accepted.values())),
                   "per_target": {k: len(v) for k, v in accepted.items()}, "fill_refusals": refusals}
        ctx.evidence.append({"kind": "durable_budget_refusal", **refusal})
    return accepted, probe, refusal


@case("L8")
async def full_durable_budget(ctx):
    """At the durable budget, after GC, operation-window eviction and alias growth: a new send is refused quota; a late
    native reply to an admitted record still records its outcome; agent registration still succeeds; an explicit
    archive exports and frees space."""
    sub = await sub_coordination(ctx, "l8", 12)
    try:
        late_agent, spacer_agent = sub.agents[-1], sub.agents[0]
        await sub.native.call("plan", ref=late_agent["backend_ref"], op="agent.send", plan={"before": "l8-late"})
        late = await sub.send(target=late_agent["id"], mode="admit", text="admitted before the budget filled")
        await sub.grant("l8-spacer", "l8-spacer", ["send:admit"], "*")
        spacer_peer = await sub.connect("l8-spacer")
        spacers = [(await sub.send(peer=spacer_peer, target=spacer_agent["id"], mode="admit", send_new=True,
                                   text="z" * 30000))["delivery"] for _ in range(2)]
        for index, row in enumerate(sub.agents[:4]):
            await sub.command("names", {"op": await sub.operation(), "handle": f"@l8-{index}", "agent": row["id"], "expect": 0})
        # GC witness (prim-final-review): settle more records than K2 keeps in full on one target, staying below the
        # sender/target open quota, so history GC compacts the oldest before the fill.
        gc_target = sub.agents[1]
        await scope(sub, "resume", "agent:" + gc_target["id"])
        gc_sent = []
        for index in range(300):
            gc_sent.append((await send_same_op(sub, target=gc_target["id"], text=f"settle {index}", send_new=True, mode="admit"))["delivery"])
            if len(gc_sent) >= 6: await settled_within(sub, gc_sent[-6], 60)
        for delivery in gc_sent[-6:]: await settled_within(sub, delivery, 60)
        await scope(sub, "pause", "agent:" + gc_target["id"])
        oldest = await sub.get(gc_sent[0])
        ctx.check(oldest.get("incomplete") or not oldest.get("content"), "history GC compacted the oldest settled record before the fill",
                  settled=len(gc_sent), oldest={k: oldest.get(k) for k in ("state", "incomplete")})
        fill_targets = sub.agents[1:-1]
        accepted, probe, refusal = await saturate_domain(ctx, sub, fill_targets)
        # Operation-window eviction under saturation: refusals also take window slots.
        first = (await sub.command("inbox.window"))["epoch"]
        for count in range(1, 2100):
            try: await sub.send(peer=probe, target=sub.agents[1]["id"], mode="admit", text="p")
            except RpcError as error:
                if error.kind not in ("quota", "busy"): raise
            if count % 64 == 0 and (await sub.command("inbox.window"))["epoch"] != first: break
        ctx.check((await sub.command("inbox.window"))["epoch"] != first, "the operation window rolled while saturated", refusals=count)
        try:
            await sub.command("names", {"op": await sub.operation(), "handle": "@l8-grown", "agent": sub.agents[4]["id"], "expect": 0})
            alias = "accepted"
        except RpcError as error: alias = error.kind
        sub.evidence.append({"kind": "alias_growth_at_budget", "result": alias})
        try:
            await sub.send(peer=probe, target=sub.agents[1]["id"], mode="admit", text="p", send_new=True)
            ctx.check(False, "still refused after the window rolled")
        except RpcError as error: after_roll = {"kind": error.kind, "message": str(error)}
        # New sends at the budget from a never-used sender, rotating targets so per-pair quotas never mask the budget
        # (prim-final-review): count only refusals that name the budget; every one must be quota.
        await sub.grant("l8-fresh", "l8-fresh", ["send:admit"], "*")
        fresh = await sub.connect("l8-fresh")
        at_budget = []
        for index in range(40):
            target = fill_targets[1 + index % (len(fill_targets) - 1)]  # never the GC target
            try:
                reply = await sub.send(peer=fresh, target=target["id"], text="fresh sender at budget", send_new=True, mode="admit")
                at_budget.append({"admitted": reply.get("delivery")})
            except RpcError as error:
                at_budget.append({"kind": error.kind, "message": str(error)[:120],
                                  "budget": any(word in str(error) for word in ("reserve", "domain", "budget", "slot"))})
            if sum(bool(row.get("budget")) for row in at_budget) >= 4: break
        # Observation (not graded by L8): an admitted, never-claimed record is not dispatched at the budget.
        await sub.scope("resume", "agent:" + late_agent["id"])
        await asyncio.sleep(.5)
        sub.evidence.append({"kind": "unclaimed_admitted_at_budget", "record": await sub.get(late["delivery"]),
                             "invocations": len(invocations(sub, delivery=late["delivery"]))})
        # Registration still succeeds at the budget.
        await sub.native.call("add", ref="l8-registered")
        async def registered():
            rows = await sub.resources()
            return next((row for row in rows if row["backend_ref"] == "l8-registered" and row.get("available")), None)
        row = await sub.wait(registered, label="new native agent registered")
        preview = await sub.command("scope", {"action": "inspect", "scope": "agent:" + row["id"]})
        ctx.check(preview.get("expect") is not None, "a newly registered agent is admitted by coordination at the budget",
                  preview=preview)
        # An explicit archive exports and frees space: exactly the two spacers.
        destination = sub.root / "archive"; destination.mkdir()
        frozen = await sub.command("inbox.archive", {"op": await sub.operation(), "action": "freeze",
                                                     "agents": [spacer_agent["id"]], "deliveries": spacers,
                                                     "destination": str(destination)})
        exported = await sub.command("inbox.archive", {"op": await sub.operation(), "action": "export", "archive": frozen["archive"]})
        freed = await sub.command("inbox.archive", {"op": await sub.operation(), "action": "free", "archive": frozen["archive"]})
        ctx.check(freed.get("state") == "completed" and sorted(freed.get("freed", [])) == sorted("message/" + d for d in spacers)
                  and (destination / (frozen["archive"] + ".json")).is_file(),
                  "an explicit archive exports to the destination and frees exactly the frozen records",
                  frozen=frozen.get("records"), exported_state=exported.get("state"), freed=freed.get("freed"))
        # The late record: claimed and invoked with room, then the budget refilled while its reply is held.
        rewake = await sub.scope("resume", "agent:" + late_agent["id"])
        sub.evidence.append({"kind": "resume_after_free", "reply": rewake})
        try: await sub.wait(lambda: invocations(sub, delivery=late["delivery"]), label="late record invoked once room exists")
        except Exception:
            ctx.check(False, "the admitted record is dispatched once space is freed", record=await sub.get(late["delivery"]),
                      status=(await sub.command("status")).get("attention"))
        # Refill concurrently (about one round trip): the claimed reply must be released within K2's native deadline.
        chunks = (30000, 30000, 8000, 8000, 2000, 2000, 500, 500, 100, 100, 1, 1, 1, 1)
        async def refill_one(index, chunk):
            try:
                await concurrent_send(sub, peer=(probe, spacer_peer)[index % 2], target=spacer_agent["id"], send_new=True, text="y" * chunk)
                return chunk
            except RpcError as error:
                if error.kind not in ("quota", "busy"): raise
                return None
        started = time.monotonic()
        refill = [chunk for chunk in await asyncio.gather(*(refill_one(i, c) for i, c in enumerate(chunks))) if chunk]
        sub.evidence.append({"kind": "refill", "admitted": refill, "seconds": round(time.monotonic() - started, 2)})
        try:
            await sub.send(peer=spacer_peer, target=sub.agents[2]["id"], mode="admit", text="p")
            refilled = False
        except RpcError: refilled = True
        ctx.check(refilled and (await sub.get(late["delivery"]))["state"] == "claimed",
                  "the budget is full again while the late record is claimed with its reply held", refill=refill)
        await sub.native.call("release", barrier="l8-late")
        record = await settled(sub, late["delivery"])
        late_outcome = {"state": record["state"], "reason": record.get("reason"), "result": record.get("result")}
        ctx.check(late_outcome["state"] in ("accepted", "observed", "dispatched") and (late_outcome["result"] or {}).get("native"),
                  "a late native reply to an admitted record records its outcome at the budget", late=late_outcome)
        budget_refusals = [row for row in at_budget if row.get("budget")]
        ctx.check(len(budget_refusals) >= 4 and all(row["kind"] == "quota" for row in budget_refusals),
                  "at the durable budget a new send is refused quota (never-used sender, rotating targets)",
                  fresh_at_budget=at_budget, first_probe=refusal, after_roll=after_roll)
    finally:
        ctx.evidence.append({"kind": "sub_fixture", "name": "l8", "evidence": sub.evidence})
        await sub.close()




async def scope(ctx, action, scope_name):
    """A scope action; a stale preview is refused before any write, so re-inspect and retry (bounded)."""
    for _ in range(5):
        try: return await ctx.scope(action, scope_name)
        except RpcError as error:
            if error.kind != "conflict" or "stale" not in str(error): raise
            ctx.evidence.append({"kind": "stale_scope_preview_retried", "action": action, "scope": scope_name})
    return await ctx.scope(action, scope_name)


def declared_latency(status):
    """A declared control/answer latency bound (ms), wherever the target publishes it in coord.status; None if absent."""
    found = {}
    def walk(value, path):
        if isinstance(value, dict):
            for key, item in value.items():
                if "latency" in key and isinstance(item, (int, float)): found[path + key] = item
                walk(item, path + key + ".")
    walk(status, "")
    return found


@case("I19")
async def control_headroom_under_reconnect_burst_and_flood(ctx):
    """During a burst of 200 item-stream opens and an inbox flood, interrupts and answers are admitted within the
    declared latency bound; stream opens use their own budget and never exhaust the control reservation."""
    await ctx.native.call("add-many", count=200, prefix="a")
    await ctx.wait_agents(200)
    agents = ctx.agents[:200]
    bound = declared_latency(await ctx.command("status"))
    ctx.evidence.append({"kind": "declared_latency_bound", "found": bound})
    request = lambda n: {"request": f"q{n}", "revision": 1, "scope": {"session": "s1"}, "status": "pending",
                         "method": "confirm", "title": "continue?", "params": {}, "incomplete": False}
    answer_index = 0
    await ctx.native_set(0, requests=[request(0)], state="running")
    viewers = [await ctx.connect("owner") for _ in range(4)]
    for index in range(4):
        await ctx.grant(f"i19-f{index}", f"i19-f{index}", ["send:admit"], "*")
    flooders = [await ctx.connect(f"i19-f{index}") for index in range(4)]
    async def burst():
        async def open_all(peer, rows):
            return await asyncio.gather(*(ctx.attach(rid=row["id"], peer=peer) for row in rows), return_exceptions=True)
        groups = [agents[index::4] for index in range(4)]
        results = await asyncio.gather(*(open_all(peer, rows) for peer, rows in zip(viewers, groups)))
        return [item for group in results for item in group]
    async def flood():
        sent = refused = 0
        for round_ in range(40):
            for peer in flooders:
                try: await ctx.send(peer=peer, target=agents[2 + round_ % 190]["id"], mode="admit", send_new=True, text="f" * 8192); sent += 1
                except RpcError: refused += 1
        return sent, refused
    async def controls():
        nonlocal answer_index
        samples = []
        for n in range(6):
            started = time.monotonic()
            reply = await ctx.command("control", {"op": await ctx.operation(), "action": "interrupt", "target": agents[1]["id"], "args": {"v": 0}})
            samples.append({"kind": "interrupt", "ms": (time.monotonic() - started) * 1000, "state": reply.get("state")})
            started = time.monotonic()
            reply = await ctx.command("control", {"op": await ctx.operation(), "action": "respond", "target": agents[0]["id"],
                "args": {"v": 0, "request": f"q{answer_index}", "epoch": "e1", "revision": 1, "response": {"value": True}}})
            samples.append({"kind": "answer", "ms": (time.monotonic() - started) * 1000, "state": reply.get("state")})
            answer_index += 1
            await ctx.native_set(0, requests=[request(answer_index)])
            await asyncio.sleep(.05)
        return samples
    opened, flooded, samples = await asyncio.gather(burst(), flood(), controls())
    failures = [repr(item) for item in opened if isinstance(item, BaseException)]
    ctx.evidence.append({"kind": "i19_load", "opens": len(opened), "open_failures": failures[:8], "flood": flooded, "samples": samples})
    ctx.check(not failures and len(opened) == 200, "200 item-stream opens in one burst all succeed on their own budget",
              failures=len(failures))
    ctx.check(flooded[0] > 0, "the inbox flood ran concurrently", sent=flooded[0], refused=flooded[1])
    ctx.check(all(row["state"] in ("accepted", "observed", "dispatched") for row in samples),
              "every interrupt and answer is admitted during the burst and flood", samples=samples)
    ctx.check(len(ctx.native.records("native_response")) == 6, "each answer reached the native request exactly once")
    worst = max(row["ms"] for row in samples)
    if not bound:
        ctx.check(False, "interrupts and answers meet the declared latency bound: the target declares none "
                  "(coordination-v0 §9 item 4, pending C1)", worst_ms=round(worst, 1), samples=samples)
    limit = min(bound.values())
    ctx.check(worst <= limit, "interrupts and answers meet the declared latency bound", worst_ms=worst, bound=bound)


async def operation(ctx, suffix="op"):
    """ctx.operation() reads its serial after an await, so concurrent callers can share an op id; take it first."""
    ctx.serial += 1
    serial = ctx.serial
    window = await ctx.command("inbox.window")
    return window["epoch"] + f":b{serial}:{suffix}"


async def send_same_op(ctx, *, target, text, peer=None, attempts=3, **fields):
    """A send that survives a slow reply: the window read is read-only, and repeating the same op id observes the
    original result (K2 caches by op), so a timeout is retried without ever creating a second intent."""
    for _ in range(attempts):
        try: op = await operation(ctx, "send"); break
        except (asyncio.TimeoutError, TimeoutError): continue
    else: raise TimeoutError("operation window read timed out")
    args = {"op": op, "submission": op, "to": target, "mode": fields.pop("mode", "admit"),
            "content": [{"type": "text", "text": text}], **fields}
    for attempt in range(attempts):
        try: return await ctx.command("inbox.send", args, peer=peer)
        except (asyncio.TimeoutError, TimeoutError):
            ctx.evidence.append({"kind": "same_op_retry", "op": op, "attempt": attempt + 1})
    raise TimeoutError("inbox.send timed out on every same-op attempt")


async def settled_within(ctx, delivery, timeout):
    """settled() with an explicit bound: the GC witness needs history, not speed, so a slow dispatcher under load is not
    a failure, while a record that never settles still is."""
    async def done():
        row = await ctx.command("inbox.get", {"delivery": delivery})
        return row if row.get("state") not in ("held", "recorded", "claimed") else None
    return await ctx.wait(done, label="settled " + delivery, timeout=timeout)


async def concurrent_send(ctx, *, peer, target, text, **fields):
    op = await operation(ctx, "send")
    return await ctx.command("inbox.send", {"op": op, "submission": op, "to": target, "mode": fields.pop("mode", "admit"),
                                           "content": [{"type": "text", "text": text}], **fields}, peer=peer)


def percentile(values, fraction):
    values = sorted(values)
    return round(values[min(len(values) - 1, int(fraction * len(values)))], 1) if values else None


class CoreSampler:
    """CPU seconds and RSS of the private core, sampled every 0.5 s (evidence, not a pass criterion by itself)."""
    def __init__(self, pid):
        self.pid, self.samples, self.task = pid, [], None
    def sample(self):
        try:
            fields = Path(f"/proc/{self.pid}/stat").read_text().rsplit(")", 1)[1].split()
            cpu = (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK")
            rss = next(int(line.split()[1]) for line in Path(f"/proc/{self.pid}/status").read_text().splitlines() if line.startswith("VmRSS:"))
            self.samples.append({"t": time.monotonic(), "cpu_s": cpu, "rss_kib": rss})
        except (OSError, StopIteration, IndexError, ValueError): pass
    async def run(self):
        while True:
            self.sample(); await asyncio.sleep(.5)
    def summary(self):
        if len(self.samples) < 2: return {}
        first, last = self.samples[0], self.samples[-1]
        return {"duration_s": round(last["t"] - first["t"], 1), "cpu_s": round(last["cpu_s"] - first["cpu_s"], 1),
                "cpu_mean_percent": round(100 * (last["cpu_s"] - first["cpu_s"]) / max(.001, last["t"] - first["t"]), 1),
                "rss_max_kib": max(row["rss_kib"] for row in self.samples), "samples": len(self.samples)}


async def stalled_viewer(ctx, agents):
    """A raw connection that attaches every agent's item stream and never reads."""
    reader, writer = await asyncio.open_unix_connection(ctx.backend.env["VQRO_SOCKET"])
    lines = [{"jsonrpc": "2.0", "id": 1, "method": "hello", "params": {"client": "s1-stalled", "identity": {"kind": "owner"}}}]
    lines += [{"jsonrpc": "2.0", "id": 2 + index, "method": "resource.attach", "params": {"resource": row["id"], "items": {}}}
              for index, row in enumerate(agents)]
    for line in lines: writer.write(json.dumps(line).encode() + b"\n")
    await writer.drain()
    return reader, writer


@case("S1")
async def combined_scale_gate(ctx):
    """600 terminals / 200 agents, 1 then 3 viewers per agent, a stalled viewer, a 200-attach burst, steady and burst
    inbox traffic, an inbox flood and a reconnect storm. Passes when layout, control and recovery keep working, excess
    is refused predictably and the stalled viewer harms no other viewer; CPU, RSS, commit p50/p99, events/missed,
    occupancy and control/answer latency are recorded."""
    sampler = CoreSampler(ctx.backend.core.pid); sampling = asyncio.ensure_future(sampler.run())
    metrics = {"inbox_ms": [], "layout_ms": [], "control_ms": [], "answer_ms": [], "refusals": {}, "events": 0, "missed": []}
    monitor = await ctx.connect("owner")
    monitor.on("event", lambda value: metrics.__setitem__("events", metrics["events"] + 1))
    monitor.on("missed", lambda value: metrics["missed"].append(value))
    await monitor.call("events.subscribe", {})
    try:
        await ctx.native.call("add-many", count=200, prefix="a", timeout=60)
        await ctx.wait_agents(200)
        agents = ctx.agents[:200]
        terminals = []
        async def spawn(index):
            terminals.append((await ctx.peer.call("resource.spawn", {"argv": ["sh"]}))["resource"]["id"])
        for start in range(0, 600, 50): await asyncio.gather(*(spawn(index) for index in range(start, start + 50)))
        ctx.check(len(terminals) == 600, "600 terminals spawned", terminals=len(terminals))
        stalled = [await stalled_viewer(ctx, agents[:100]), await stalled_viewer(ctx, agents[100:])]
        await asyncio.sleep(2)  # the stalled viewer's opens settle first; the burst below is one 200-attach burst
        # Phase 1: one viewer per agent, opened as one 200-attach burst.
        viewers = [await ctx.connect("owner") for _ in range(3)]
        window = asyncio.Semaphore(64)  # viewers window attaches under the 128-request connection limit
        async def windowed(peer, row):
            async with window: return await ctx.attach(rid=row["id"], peer=peer)
        async def attach_all(peer):
            results = await asyncio.gather(*(windowed(peer, row) for row in agents), return_exceptions=True)
            return [item for item in results if not isinstance(item, BaseException)], [repr(item) for item in results if isinstance(item, BaseException)]
        views, failures = await attach_all(viewers[0])
        ctx.check(len(views) == 200 and not failures, "a 200-attach burst stays within the effect limit", failures=failures[:4])
        for index in range(4): await ctx.grant(f"s1-s{index}", f"s1-s{index}", ["send:admit"], "*")
        senders = [await ctx.connect(f"s1-s{index}") for index in range(4)]
        await ctx.grant("s1-flood", "s1-flood", ["send:admit"], "*")
        flooder = await ctx.connect("s1-flood")
        await scope(ctx, "resume", "workspace")
        request = lambda n: {"request": f"s1q{n}", "revision": 1, "scope": {"session": "s1"}, "status": "pending",
                             "method": "confirm", "title": "continue?", "params": {}, "incomplete": False}
        await ctx.native_set(0, requests=[request(0)], state="running")
        answered = 0
        async def timed(bucket, call):
            started = time.monotonic()
            try: return await call
            except RpcError as error:
                metrics["refusals"][f"{bucket}:{error.kind}"] = metrics["refusals"].get(f"{bucket}:{error.kind}", 0) + 1
                metrics.setdefault("refusal_examples", {}).setdefault(f"{bucket}:{error.kind}", str(error)[:240])
                return None
            finally: metrics[bucket].append((time.monotonic() - started) * 1000)
        async def steady(seconds, rate):
            nonlocal answered
            deadline, n = time.monotonic() + seconds, 0
            while time.monotonic() < deadline:
                n += 1
                await asyncio.gather(*(timed("inbox_ms", concurrent_send(ctx, peer=senders[k], target=agents[(n * 4 + k) % 200]["id"], send_new=True, text=f"steady {n}")) for k in range(4)),
                                     timed("layout_ms", ctx.peer.call("workspace.apply", {"ops": [{"op": "set", "key": f"doc/part-b/s1/{n % 64}", "value": n}]})))
                if n % 10 == 0:
                    await timed("control_ms", ctx.command("control", {"op": await operation(ctx), "action": "interrupt", "target": agents[1]["id"], "args": {"v": 0}}))
                    reply = await timed("answer_ms", ctx.command("control", {"op": await operation(ctx), "action": "respond", "target": agents[0]["id"],
                        "args": {"v": 0, "request": f"s1q{answered}", "epoch": "e1", "revision": 1, "response": {"value": True}}}))
                    if reply is not None:
                        answered += 1; await ctx.native_set(0, requests=[request(answered)], state="running")
                await asyncio.sleep(1 / rate)
        await steady(15, 10)
        # Phase 2: three viewers per agent.
        for peer in viewers[1:]:
            more, failures = await attach_all(peer)
            ctx.check(len(more) == 200 and not failures, "the second and third viewers attach every agent", failures=failures[:4])
            views += more
        await asyncio.gather(steady(15, 10), *(ctx.native.call("items-flood", ref=row["backend_ref"], count=200, size=1024, timeout=60) for row in agents[:20]))
        # Phase 3: inbox burst and flood; excess is refused predictably.
        burst_window = asyncio.Semaphore(48)  # the owner connection also carries the op-window reads
        async def burst_send(k):
            async with burst_window:
                return await timed("inbox_ms", concurrent_send(ctx, peer=senders[k % 4], target=agents[k]["id"], send_new=True, text="burst"))
        burst = await asyncio.gather(*(burst_send(k) for k in range(200)))
        flood_kinds = {}
        for n in range(200):
            try: await ctx.send(peer=flooder, target=agents[5]["id"], mode="admit", send_new=True, text="f" * 16384)
            except RpcError as error: flood_kinds[error.kind] = flood_kinds.get(error.kind, 0) + 1
        ctx.check(set(flood_kinds) <= {"quota"} and flood_kinds, "an inbox flood is refused predictably (quota)", kinds=flood_kinds)
        extra = await ctx.connect("owner")
        over = await asyncio.gather(*(windowed(extra, row) for row in agents), return_exceptions=True)
        over_kinds = sorted({str(item) for item in over if isinstance(item, BaseException)})[:3]
        opened_extra = sum(not isinstance(item, BaseException) for item in over)
        ctx.evidence.append({"kind": "s1_excess_attach", "opened": opened_extra, "refused": len(over) - opened_extra, "examples": over_kinds})
        ctx.check(opened_extra < 200 and all("overloaded" in kind or "item stream bound" in kind for kind in over_kinds),
                  "attaches beyond the item-stream budget are refused predictably (overloaded)", opened=opened_extra, examples=over_kinds)
        await extra.close()
        # Phase 4: reconnect storm: every viewer connection drops and reattaches.
        for index, peer in enumerate(viewers):
            await peer.close()
            viewers[index] = await ctx.connect("owner")
        storm = []
        for peer in viewers:
            reopened, failures = await attach_all(peer)
            storm.append(len(reopened))
            ctx.check(not failures, "every viewer reattaches after the reconnect storm", failures=failures[:4])
        await steady(10, 10)
        # Healthy viewers: no reconstruction failure on any non-stalled stream after the storm.
        fresh = [view for view in ctx.views.values() if view.peer in viewers]
        broken = [view.reducer.failure for view in fresh if view.reducer.failure]
        ctx.check(len(fresh) == 600 and not broken, "the stalled viewer harms no other viewer (600 live streams reconstruct)",
                  live=len(fresh), broken=broken[:4])
        # Layout, control and recovery headroom under the load.
        ctx.check(not any(key.startswith("layout_ms") for key in metrics["refusals"]), "every layout commit succeeded", refusals=metrics["refusals"])
        ctx.check(not any(key.startswith(("control_ms", "answer_ms")) for key in metrics["refusals"]) and metrics["answer_ms"],
                  "every interrupt and answer was admitted", refusals=metrics["refusals"])
        examples = metrics.get("refusal_examples", {})
        ctx.check(all(key.endswith(":quota") or key.endswith(":overloaded") and "not dispatched" in examples.get(key, "")
                      for key in metrics["refusals"]),
                  "every inbox refusal under burst load is predictable (quota, or overloaded before dispatch)",
                  refusals=metrics["refusals"], examples=examples)
        paused = await scope(ctx, "pause", "workspace"); resumed = await scope(ctx, "resume", "workspace")
        ctx.check(paused.get("state") == "accepted" and resumed.get("state") == "accepted", "recovery (pause/resume workspace) works under load")
        status = await ctx.command("status")
        ctx.evidence.append({"kind": "s1_metrics", "core": sampler.summary(),
            "commit_ms": {name: {"p50": percentile(metrics[name], .5), "p99": percentile(metrics[name], .99), "n": len(metrics[name])}
                          for name in ("inbox_ms", "layout_ms", "control_ms", "answer_ms")},
            "raw_ms": {name: [round(value, 1) for value in metrics[name]] for name in ("control_ms", "answer_ms")},
            "percentile": "nearest rank, no interpolation: sorted[min(n-1, int(q*n))]",
            "timed": "client round trip of the coord.control call only (op id fetched before the timer starts)",
            "events": metrics["events"], "missed": metrics["missed"][:8], "missed_count": len(metrics["missed"]),
            "occupancy": {"window": status.get("window"), "attention": status.get("attention")},
            "terminals": len(terminals), "item_streams_live": len(fresh), "stalled_streams": len(agents),
            "storm_reattached": storm, "refusals": metrics["refusals"], "refusal_examples": metrics.get("refusal_examples"), "burst_admitted": sum(row is not None for row in burst)})
    finally:
        sampling.cancel()
        for _, writer in locals().get("stalled", []): writer.close()
