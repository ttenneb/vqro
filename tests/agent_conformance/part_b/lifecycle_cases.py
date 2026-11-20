# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Part B identity-cache and tombstone probes (L7). L8 (durable budget) lives in scale_cases.py.

Public primitives only: owner/principal connections, `coord.*` commands, `operational.snapshot`, core
`workspace.snapshot.identity_reservations`, and extra provider connections that publish agent facts exactly as a
backend would. Every pass names its observed witness; limits come from declarations (`coord.status` limits and the
core snapshot), never from constants copied out of an implementation. A subclause the declaration cannot support
is recorded as a gap and fails the row; it is never converted into a pass.
"""
import asyncio
import json
import time

from .cases import case, invocations, settled
from ..results import CheckFailed
from ..wire import Peer


def encoded(value):
    return len(json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode())


async def declared_limits(ctx):
    status = await ctx.command("status")
    limits = dict(status.get("limits") or {})
    limits["identity_cache"] = status.get("identity_cache")  # declared beside the limits, not inside them
    ctx.evidence.append({"kind": "declared_limits", "limits": limits})
    return limits


async def tombstones(ctx):
    """Every tombstone summary in the protected domain, keyed by agent (read-only public snapshot)."""
    snapshot = await ctx.peer.call("operational.snapshot", {"domain": "coord"})
    rows = {}
    for key, entry in snapshot["entries"].items():
        value = entry.get("value")
        if "/agents/" in key and isinstance(value, dict):
            for agent, stone in value.items():
                if isinstance(stone, dict) and stone.get("agent") == agent:
                    rows[agent] = stone
    return rows


async def reservations(ctx):
    stats = (await ctx.peer.call("workspace.snapshot")).get("identity_reservations")
    if not isinstance(stats, dict):
        raise CheckFailed("core snapshot exposes no identity_reservations bookkeeping", component="core")
    return stats


def agent_fact(ref, *, incarnation="i1", app="l7-fixture"):
    return {"ref": ref, "incarnation": incarnation, "kind": "agent", "state": "live",
            "status": {"state": "idle", "app": app},
            "data": {"agent": {"v": 0, "state": "idle", "lifecycle": "live", "session": {"id": "s1"}, "epoch": "e1",
                               "host": {"id": "l7-fixture-process", "kind": "process", "owns_lifecycle": False},
                               "caps": {"rev": 1}}}}


class Backend:
    """A second, test-owned provider connection publishing plain agent facts (no effects are served)."""
    def __init__(self, ctx, name, instance):
        self.ctx, self.name, self.instance, self.peer, self.generation = ctx, name, instance, None, None

    async def connect(self, facts):
        self.peer = await Peer.connect(self.ctx.backend.env["VQRO_SOCKET"], max_record=8 << 20, max_pending=64)
        await self.peer.call("hello", {"client": "part-b-l7-provider"})
        registration = await self.peer.call("provider.register", {
            "name": self.name, "role": "backend", "instance": self.instance,
            "software": {"name": "part-b-l7", "version": "0"}, "provider_version": "0",
            "capabilities": {"agent": {}}})
        self.generation = registration["generation"]
        chunks = [facts[i:i + 256] for i in range(0, len(facts), 256)] or [[]]
        for index, chunk in enumerate(chunks):
            await self.peer.call("provider.sync", {"generation": self.generation, "resources": chunk,
                                                   "final": index == len(chunks) - 1}, timeout=30)
        async def published():
            rows = (await self.ctx.peer.call("resource.list", {"provider": self.name}))["resources"]
            live = [row for row in rows if row.get("available") and row.get("kind") == "agent"]
            return live if len(live) >= len(facts) else None
        rows = await self.ctx.wait(published, label=f"{len(facts)} agents from {self.name}", timeout=60)
        return {row["backend_ref"]: row for row in rows}

    async def disconnect(self):
        if self.peer is not None:
            await asyncio.wait_for(self.peer.close(), 4)
            self.peer = None


@case("L7")
async def identity_cache_and_tombstones(ctx):
    """~900 unavailable agents per hour: K2-driven forgetting stays within the cache; durable evidence is kept;
    evidence-free tombstones are pruned oldest first; maximum legal identities and summaries fit; a large native
    result is stored as size/sha256/incomplete."""
    gaps = []
    limits = await declared_limits(ctx)
    cache = limits.get("identity_cache")
    summary_bound = limits.get("identity_summary_bytes")
    # 1. Declaration: L7 needs the extension's identity-cache size N.
    if not isinstance(cache, dict) or not isinstance(cache.get("capacity"), int) or cache.get("incomplete"):
        gaps.append({"subclause": "identity cache size N declared by the extension",
                     "declared": cache, "component": "coordination"})
    core = await reservations(ctx)
    ctx.evidence.append({"kind": "core_identity_reservations_before", "stats": core})
    n_core = core.get("max_entries")

    # 2. Churn at the measured soak rate: one test-owned backend with 300 agents in two waves (old, new); every
    # agent gets a tombstone (scope pause), five carry durable evidence (held mail), then all become unavailable.
    churn = Backend(ctx, "part-b-l7-churn", "l7-instance")
    refs_old = [f"old{i:03d}" for i in range(150)]
    refs_new = [f"new{i:03d}" for i in range(150)]
    rows = await churn.connect([agent_fact(ref) for ref in refs_old + refs_new])
    for wave in (refs_old, refs_new):
        for ref in wave:
            await ctx.scope("pause", "agent:" + rows[ref]["id"])
        await asyncio.sleep(0.05)  # distinct last_seen_ms between waves
    evidence = [rows[ref]["id"] for ref in refs_old[:5]]
    kept_mail = []
    for agent in evidence:
        kept_mail.append((await ctx.send(target=agent, text="durable evidence " + agent, send_new=True))["delivery"])
    stones = await tombstones(ctx)
    ctx.check(all(rows[ref]["id"] in stones for ref in refs_old + refs_new), "every churn agent has a tombstone summary",
              missing=[ref for ref in refs_old + refs_new if rows[ref]["id"] not in stones][:8])
    before_count = len(stones)

    # 2b. Eviction pressure beyond the core's declared cache bound: these identities are forgotten FIRST (the oldest
    # in the cache). One flood backend at a time stays below the extension's resource traversal bound. The first
    # flood agent also carries a tombstone and held mail.
    flood_size = n_core + 8 if isinstance(n_core, int) and n_core <= 8192 else 0
    floods, first, first_mail, forget_failures = [], None, None, []
    flood_started = time.monotonic()
    per = 2048
    for index in range(0, flood_size, per):
        refs = [f"f{index + i:05d}" for i in range(min(per, flood_size - index))]
        backend = Backend(ctx, f"part-b-l7-flood-{index // per}", "l7-flood")
        rows_by_ref = await backend.connect([agent_fact(ref) for ref in refs])
        floods.append((backend, refs, rows_by_ref))
        if first is None:
            first = rows_by_ref[refs[0]]
            await ctx.scope("pause", "agent:" + first["id"])
            first_mail = (await ctx.send(target=first["id"], text="evidence before eviction", send_new=True))["delivery"]
        await backend.disconnect()
        async def unavailable(backend=backend, rows_by_ref=rows_by_ref, refs=refs):
            listed = {r["id"]: r for r in (await ctx.peer.call("resource.list", {"provider": backend.name}))["resources"]}
            return all(listed.get(rows_by_ref[ref]["id"], {}).get("available") is False for ref in refs[:4])
        await ctx.wait(unavailable, label="flood agents unavailable", timeout=30)
        for i in range(0, len(refs), 32):
            batch = refs[i:i + 32]
            results = await asyncio.gather(*(ctx.peer.call("resource.forget_cached", {"resource": rows_by_ref[ref]["id"]}, timeout=30)
                                             for ref in batch), return_exceptions=True)
            forget_failures.extend(str(r)[:120] for r in results if isinstance(r, BaseException))
    ctx.evidence.append({"kind": "direct_forget_flood", "forgotten": flood_size, "failures": forget_failures[:8],
                         "seconds": round(time.monotonic() - flood_started, 1)})
    ctx.check(not forget_failures, "core accepts forget_cached for every unavailable flood identity", failures=forget_failures[:8])
    started = time.monotonic()
    await churn.disconnect()

    async def k2_forgot():
        listed = {r["id"] for r in (await ctx.peer.call("resource.list", {"provider": "part-b-l7-churn"}))["resources"]}
        return not any(rows[ref]["id"] in listed for ref in refs_old + refs_new)
    await ctx.wait(k2_forgot, label="extension forgets its unavailable agents after its grace", timeout=240)
    stats = await reservations(ctx)
    ctx.evidence.append({"kind": "k2_driven_forget", "seconds": round(time.monotonic() - started, 1), "stats": stats})
    ctx.check(stats["count"] <= stats["max_entries"] and stats["bytes"] <= stats["max_entries"] * stats["max_entry_bytes"],
              "core identity cache stays within its declared bound under churn and flood", stats=stats)
    if flood_size:
        ctx.check(stats["count"] == stats["max_entries"], "the flood filled the cache to its bound (eviction happened)", stats=stats)

    async def pruned():
        current = await tombstones(ctx)
        return current if len(current) <= 256 else None
    current = await ctx.wait(pruned, label="evidence-free tombstones pruned", timeout=60)
    ctx.evidence.append({"kind": "tombstones_after_prune", "before": before_count, "after": len(current)})
    ctx.check(all(agent in current and current[agent].get("held", 0) >= 1 for agent in evidence),
              "tombstones with durable evidence (held mail) are never pruned", evidence=evidence)
    for delivery in kept_mail:
        record = await ctx.get(delivery)
        ctx.check(record.get("state") == "held", "held mail of an unavailable agent stays held, never silently dropped",
                  record={k: record.get(k) for k in ("state", "reason")})
    old_free = [rows[ref]["id"] for ref in refs_old[5:]]
    new_free = [rows[ref]["id"] for ref in refs_new]
    removed_old = sum(agent not in current for agent in old_free)
    removed_new = sum(agent not in current for agent in new_free)
    ctx.check(removed_old > 0, "pressure prunes evidence-free tombstones", removed_old=removed_old, removed_new=removed_new)
    ctx.check(removed_new == 0 or removed_old == len(old_free), "evidence-free tombstones are pruned oldest first",
              removed_old=removed_old, removed_new=removed_new)

    # 3. Reappearance: the newest forgotten identity returns as the same rN; the oldest (evicted) one returns as a
    # NEW agent, and its old mail stays with the old rN (never rerouted to the new agent).
    back = Backend(ctx, "part-b-l7-churn", "l7-instance")
    returned = await back.connect([agent_fact(refs_new[-1])])
    ctx.check(returned[refs_new[-1]]["id"] == rows[refs_new[-1]]["id"],
              "a forgotten identity still in the cache returns as the same agent (rN)",
              before=rows[refs_new[-1]]["id"], after=returned[refs_new[-1]]["id"])
    await back.disconnect()
    if flood_size:
        backend, refs, _ = floods[0]
        again = Backend(ctx, backend.name, backend.instance)
        reborn = (await again.connect([agent_fact(refs[0])]))[refs[0]]
        ctx.evidence.append({"kind": "evicted_return", "old": first["id"], "new": reborn["id"]})
        ctx.check(reborn["id"] != first["id"], "an evicted identity returns as a new agent (fresh rN)", old=first["id"], new=reborn["id"])
        old_mail = await ctx.get(first_mail)
        ctx.check(old_mail.get("to", {}).get("agent") == first["id"] and old_mail.get("state") == "held",
                  "the evicted agent's durable mail stays with its old rN, never rerouted to the new agent",
                  record={k: old_mail.get(k) for k in ("state", "reason", "to")})
        listed = await ctx.command("inbox.list", {"target": reborn["id"]})
        ctx.check(not listed.get("records"), "the returned (new) agent inherits no old mail", listed=listed)
        await again.disconnect()
    else:
        gaps.append({"subclause": "eviction makes a return a new agent", "reason": "core declares no usable cache bound",
                     "component": "core"})

    # 4. Maximum legal identity (256 control characters in the instance) with four maximum handles.
    big = Backend(ctx, "p" * 120 + "-l7max", "\x01" * 256)
    maxrow = (await big.connect([agent_fact("r" * 128, incarnation="n" * 128)]))["r" * 128]
    for index in range(4):
        handle = "@" + "/".join((f"{index}" + "h" * 31, "s" * 32, "t" * 32))
        await ctx.command("names", {"op": await ctx.operation("name"), "handle": handle, "agent": maxrow["id"], "expect": 0})
    await ctx.scope("pause", "agent:" + maxrow["id"])
    stone = (await tombstones(ctx)).get(maxrow["id"]) or {}
    identity_bytes, summary_bytes = encoded(stone.get("identity")), encoded(stone)
    ctx.evidence.append({"kind": "maximum_identity", "identity_bytes": identity_bytes, "summary_bytes": summary_bytes,
                         "declared_summary_bound": summary_bound})
    ctx.check(not stone.get("identity_incomplete") and stone.get("identity", {}).get("instance") == "\x01" * 256,
              "maximum legal identity is retained whole, not compacted", stone_keys=sorted(stone))
    ctx.check(identity_bytes <= 2048 and summary_bytes <= 3072,
              "maximum identity and summary stay within 2 KiB and 3 KiB", identity=identity_bytes, summary=summary_bytes)
    await big.disconnect()

    # 5. A native result larger than the compact outcome is stored as size, sha256 and incomplete.
    blob = "x" * (64 * 1024)
    await ctx.native.call("plan", ref=ctx.agents[0]["backend_ref"], op="agent.send",
                          plan={"result": {"outcome": "accepted", "data": {"receipt": {"stage": "admitted"}, "blob": blob}}})
    sent = await ctx.send(text="large native result")
    await ctx.scope("resume")
    record = await settled(ctx, sent["delivery"])
    result = record.get("result") or {}
    native = result.get("native") or {}
    ctx.evidence.append({"kind": "large_native_result", "stored": result})
    ctx.check(encoded(result) <= 2048 and blob not in json.dumps(record), "stored outcome is compact, never the native payload")
    ctx.check(native.get("incomplete") is True and isinstance(native.get("bytes"), int) and native["bytes"] > len(blob)
              and isinstance(native.get("sha256"), str) and len(native["sha256"]) == 64,
              "large native result is recorded as size, sha256 and incomplete", native=native)
    calls = invocations(ctx, delivery=sent["delivery"], op="agent.send")
    ctx.check(len(calls) == 1, "the large result came from exactly one native write")

    # 5b. The live caller's reply carries the full native result; a later historical lookup is compact.
    await ctx.native.call("plan", ref=ctx.agents[0]["backend_ref"], op="agent.interrupt",
                          plan={"result": {"outcome": "accepted", "data": {"blob": blob}}})
    control_op = await ctx.operation("large-control")
    live = await ctx.command("control", {"op": control_op, "action": "interrupt", "target": ctx.agents[0]["id"], "args": {"v": 0}})
    historical = await ctx.command("inbox.op", {"op": control_op})
    ctx.evidence.append({"kind": "large_control_result", "live_bytes": encoded(live), "historical_bytes": encoded(historical),
                         "historical": {k: historical.get(k) for k in ("state", "native", "historical")}})
    ctx.check(blob in json.dumps(live), "the live control reply carries the full native result", live_bytes=encoded(live))
    ctx.check(blob not in json.dumps(historical) and encoded(historical) <= 4096,
              "a later lookup of the same operation is compact, never the native payload", historical_bytes=encoded(historical))

    ctx.evidence.append({"kind": "l7_gaps", "gaps": gaps})
    ctx.check(not gaps, "L7 subclauses the declarations cannot support", gaps=gaps)
