# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Public-core archive and recovery-funding witnesses (I23, L11).

No target-module imports or protected-store writes. A missing physical cut is a
harness gap, never a successful archive qualification.
"""
import asyncio
import errno
import hashlib
import json
import os
from pathlib import Path

from .cases import case, invocations, held
from .scale_cases import declared_limits, sub_coordination
from ..results import HarnessGap, NotApplicable
from ..wire import RpcError, WireFailure


def canonical(value):
    return json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode()


async def entry(ctx, key):
    """Read the real public operational document; never manufacture a row."""
    return (await ctx.peer.call("operational.get", {"domain": "coord", "key": "operational/coord/" + key})).get("entry")


async def archive(ctx, action="freeze", **fields):
    return await ctx.command("inbox.archive", {"op": await ctx.operation("archive-" + action), "action": action, **fields})


async def inspect_archive(ctx, identifier):
    return await ctx.command("inbox.archive", {"action": "inspect", "archive": identifier})


async def uncertain_interrupt(ctx, target, padding=0):
    operation = await ctx.operation("uncertain-interrupt")
    reply = await ctx.command("control", {"op": operation, "target": target, "action": "interrupt",
                                          "args": {"v": 0, "padding": "x" * padding}})
    return operation, reply


async def saturate_content(ctx, sub, targets, limits):
    """Global fill without hundreds of per-sender refusals exhausting the op window."""
    quotas = limits["quotas"]
    senders = (limits["domain_bytes"] + quotas["sender_bytes"] - 1) // quotas["sender_bytes"] + 2
    if senders > 64:
        raise HarnessGap("declared domain exceeds this bounded64-sender saturation fixture")
    peers = []
    for index in range(senders):
        name = f"i23-s{index}"
        await sub.grant("i23-" + name, name, ["send:admit"], "*")
        peers.append(await sub.connect(name))
    await sub.grant("i23-probe", "i23-probe", ["send:admit"], "*")
    probe = await sub.connect("i23-probe")
    accepted = {row["id"]: [] for row in targets}
    serial = 0; refusals = []
    size = min(30000, quotas["content_bytes"] - 256)
    ctx.check(size > 0, "declared content bound admits a nonempty saturation record", size=size)
    closed_content_reservation = False
    busy_streak = 0
    for chunk in (size, max(1, size // 4), max(1, size // 16), max(1, size // 64), 1):
        for _ in range(256):
            row = targets[serial % len(targets)]
            peer = peers[(serial // len(targets)) % len(peers)]
            serial += 1
            try:
                sent = await sub.send(peer=peer, target=row["id"], mode="admit", send_new=True, text="s" * chunk)
            except RpcError as error:
                window = await sub.command("inbox.window")
                ctx.check(error.kind in ("quota", "busy"), "content fill stops only at an admission reservation refusal", size=chunk, refusal=error.kind, window=window)
                if error.kind == "busy":
                    ctx.check(window["used"] < limits["operation_window"], "content reservation closes before numeric operation-window exhaustion", window=window)
                    busy_streak += 1
                else:
                    busy_streak = 0
                refusals.append({"size": chunk, "kind": error.kind, "window": window})
                # A busy refusal rolls the epoch and GC may release old result
                # records. Fill that newly available room using a NEW operation;
                # no uncertain/native effect is retried. Two empty-window busy
                # cuts in succession demonstrate stable admission exhaustion.
                if error.kind == "busy" and busy_streak < 2:
                    continue
                closed_content_reservation = busy_streak >= 2
                break
            busy_streak = 0
            accepted[row["id"]].append(sent["delivery"])
        else:
            raise HarnessGap("global quota not reached within256 balanced sends at one size")
        if closed_content_reservation: break
    def charge(record):
        value = dict(record); value.pop("revision", None)
        return max(len(canonical(value)), len(canonical(value["content"])) + quotas["metadata_bytes"]) + value.get("outcome_steps", 1) * quotas["outcome_bytes"]
    target_charge = {row["id"]: 0 for row in targets}
    for target, deliveries in accepted.items():
        for delivery in deliveries:
            target_charge[target] += charge(await sub.get(delivery))
    probe_count, probe_bytes, busy_streak = 0, 0, 0
    probe_target_count = {row["id"]: 0 for row in targets}
    probe_target_bytes = dict(probe_target_count)
    minimum = len(canonical([{"type": "text", "text": "p"}])) + quotas["metadata_bytes"] + quotas["outcome_bytes"]
    # Rollover/GC can make room after an earlier busy refusal. Consume that
    # room explicitly, not by relabelling an accepted fresh probe as failure.
    for _ in range(min(64, quotas["sender_records"] - 1)):
        eligible = [row for row in targets if len(accepted[row["id"]]) < quotas["target_records"] and
                    target_charge[row["id"]] + minimum < quotas["target_bytes"] and
                    probe_target_count[row["id"]] < quotas["sender_target_records"] and
                    probe_target_bytes[row["id"]] + minimum < quotas["sender_target_bytes"]]
        if not eligible or probe_bytes + minimum >= quotas["sender_bytes"]:
            raise HarnessGap("local quotas bind before this fixture can prove global content exhaustion")
        lightest = min(eligible, key=lambda row: target_charge[row["id"]])
        target = lightest["id"]
        before = (await sub.command("inbox.list", {"target": target}))["items"]
        try:
            sent = await sub.send(peer=probe, target=target, mode="admit", text="p", send_new=True)
        except RpcError as error:
            window = await sub.command("inbox.window")
            ctx.check(error.kind in ("quota", "busy"), "minimal content stops only at quota or pre-evaluation reservation closure", refusal=error.kind, window=window)
            busy_streak = busy_streak + 1 if error.kind == "busy" else 0
            refusals.append({"size": 1, "kind": error.kind, "window": window})
            after = (await sub.command("inbox.list", {"target": target}))["items"]
            ctx.check(after == before, "the refused minimal probe creates no message", refusal=error.kind)
            if error.kind == "busy" and busy_streak < 2:
                continue
            refusal = {"kind": error.kind, "message": str(error), "open_records": sum(map(len, accepted.values())),
                       "fill_refusals": refusals, "window": window, "probe_admitted": probe_count,
                       "target_charge": target_charge[target], "probe_charge": probe_bytes}
            return accepted, probe, refusal
        else:
            busy_streak = 0; probe_count += 1
            record_charge = charge(await sub.get(sent["delivery"]))
            accepted[target].append(sent["delivery"])
            target_charge[target] += record_charge
            probe_target_count[target] += 1; probe_target_bytes[target] += record_charge; probe_bytes += record_charge
    raise HarnessGap("global minimum-record exhaustion not stable before the bounded fresh sender's own record quota")


@case("I23")
async def recovery_and_archive_funding(ctx):
    limits = await declared_limits(ctx)
    if "agent.interrupt" not in ctx.agents[0]["data"]["agent"]["caps"]:
        raise NotApplicable("data.agent.caps.agent.interrupt", None)
    headroom, escrow, future = (limits.get(name) or {} for name in
                                ("normal_control_headroom", "archive_escrow", "archive_plan_future"))
    ctx.check(headroom == {"bytes": 65536, "records": 16}, "normal content declares the required bounded-control share", declared=headroom)
    ctx.check(escrow.get("bytes") == 65536 and escrow.get("records") == 16 and escrow.get("within_control_reserve") is True,
              "the defensive archive share is inside the recovery reserve", declared=escrow)
    ctx.check(future == {"bytes": 16384, "records": 4, "exported_bytes": 8192, "exported_records": 2},
              "archive phases declare the required future funding", declared=future)
    target_bytes = (limits.get("quotas") or {}).get("target_bytes")
    domain_bytes = limits.get("domain_bytes")
    ctx.check(type(target_bytes) is int and target_bytes > 0 and type(domain_bytes) is int and domain_bytes > 0,
              "the real target declares domain and per-target byte limits", limits=limits)
    count = (domain_bytes + target_bytes - 1) // target_bytes + 2
    if count > 200:
        raise HarnessGap("declared domain requires more than200 native endpoints to isolate global saturation")
    sub = await sub_coordination(ctx, "i23", max(3, count))
    try:
        archive_target, control_target = sub.agents[-1], sub.agents[0]
        unknown = {"result": {"outcome": "indeterminate", "error": {"kind": "lost", "message": "owned fake native uncertainty"}}}
        for row in (archive_target, control_target):
            await sub.native.call("plan", ref=row["backend_ref"], op="agent.interrupt", plan=unknown)
        original_op, original = await uncertain_interrupt(sub, archive_target["id"])
        ctx.check(original.get("state") == "unknown", "one native uncertainty exists before the archive freeze", reply=original)
        destination = sub.root / "archive"; destination.mkdir()
        frozen = await archive(sub, agents=[archive_target["id"]], destination=str(destination))
        identifier = frozen["archive"]
        plan = await inspect_archive(sub, identifier)
        ctx.check(frozen.get("records") == 1 and plan.get("state") == "frozen" and len(plan.get("frozen", {})) == 1,
                  "a real one-record archive plan is accepted before saturation", plan=plan)
        source_key = next(iter(plan["frozen"]))
        ctx.check((await entry(sub, source_key)) is not None, "freeze keeps its original source record")
        accepted, probe, refusal = await saturate_content(ctx, sub, sub.agents[:-1], limits)
        ctx.check(refusal["kind"] in ("quota", "busy"), "a fresh sender's minimal content cannot reserve global admission space", refusal=refusal)
        mark = sub.native.mark()
        first_op, first = await uncertain_interrupt(sub, control_target["id"])
        ctx.check(first.get("state") == "unknown" and len(invocations(sub, after=mark, op="agent.interrupt")) == 1,
                  "normal saturation still admits a bounded control and persists its uncertain outcome", reply=first)
        stored = await sub.command("inbox.op", {"op": first_op})
        ctx.check(stored.get("state") == "unknown", "the already admitted uncertain control has a public durable outcome", stored=stored)
        quota_observations = []
        # Finish filling normal control space, progressively reducing intent size.
        # Refusals must be quota BEFORE a further native invocation, not a full-window busy substitute.
        for padding in (headroom["bytes"] // 4, headroom["bytes"] // 16, 1024, 0):
            for attempt in range(128):
                before = len(invocations(sub, op="agent.interrupt"))
                try:
                    operation, reply = await uncertain_interrupt(sub, control_target["id"], padding)
                except RpcError as error:
                    ctx.check(error.kind == "quota", "uncertain intents stop at normal quota rather than borrowing recovery", padding=padding, refusal=error.kind)
                    ctx.check(len(invocations(sub, op="agent.interrupt")) == before,
                              "the rejected uncertain control never reaches native", padding=padding, calls=before)
                    quota_observations.append({"padding": padding, "calls": before})
                    break
                ctx.check(reply.get("state") == "unknown", "every admitted uncertain intent has its reserved outcome", operation=operation, state=reply.get("state"))
            else:
                raise HarnessGap("normal control quota not reached within128 intents at one bounded size")
        ctx.check(len(quota_observations) == 4 and not invocations(sub, op="agent.send"),
                  "normal control exhaustion is observed without executing held content", observations=quota_observations)
        before_export = len(invocations(sub, op="agent.interrupt"))
        await archive(sub, "export", archive=identifier)
        exported = await inspect_archive(sub, identifier)
        path = Path(exported["locator"]); data = path.read_bytes()
        ctx.check(exported.get("state") == "exported" and hashlib.sha256(data).hexdigest() == exported.get("manifest_hash"),
                  "pre-funded export completes at normal quota and its real bytes match public confirmation", plan=exported, bytes=len(data))
        # Export reduces the declared future reserve from16KiB to8KiB. Observe
        # useful newly available normal admission, not merely that declaration.
        try:
            released_op, released = await uncertain_interrupt(sub, control_target["id"], 1024)
        except RpcError as error:
            ctx.check(False, "exported-plan funding reduction permits a previously refused bounded control", refusal=error.kind, plan=exported)
        ctx.check(released.get("state") == "unknown" and len(invocations(sub, op="agent.interrupt")) == before_export + 1,
                  "export funding reduction releases observable normal admission while preserving its outcome", operation=released_op)
        freed = await archive(sub, "free", archive=identifier)
        ctx.check(freed.get("state") == "completed" and freed.get("freed") == [source_key] and (await entry(sub, source_key)) is None,
                  "explicit one-record archive confirms and CAS-frees its original source", result=freed)
        ctx.check(len(invocations(sub, op="agent.interrupt")) == before_export + 1 and not invocations(sub, op="agent.send"),
                  "archive stages never resend native work")
        # Real non-archive recovery writes must stop while the archive-only share
        # remains usable. Fresh gates consume recovery space, not native intents.
        await sub.native.call("add-many", count=150, prefix="g")
        async def grown():
            rows = [row for row in await sub.resources() if row.get("available") and row["backend_ref"].startswith("g")]
            return rows if len(rows) == 150 else None
        fresh = await sub.wait(grown, label="150 recovery-pressure endpoints", timeout=60)
        paused, recovery_stop = 0, None
        for row in fresh:
            refusals = []
            for _ in range(3):
                before_window = await sub.command("inbox.window")
                try:
                    await sub.scope("pause", "agent:" + row["id"])
                except RpcError as error:
                    if error.kind not in ("quota", "busy"): raise
                    ctx.check(before_window["used"] < limits["operation_window"],
                              "recovery pressure is not merely numeric operation-window exhaustion", window=before_window, refusal=error.kind)
                    refusals.append({"kind": error.kind, "message": str(error), "before_window": before_window})
                    # A fresh operation can consume room released by rollover/GC;
                    # require two consecutive refusals before claiming saturation.
                    if len(refusals) == 2:
                        recovery_stop = {"agent": row["id"], "refusals": refusals}
                        break
                else:
                    paused += 1
                    break
            if recovery_stop: break
        if recovery_stop is None:
            raise HarnessGap("150 fresh recovery gates did not reach the non-archive reserve boundary")
        ctx.check(recovery_stop is not None, "real non-archive recovery is refused with a non-full numeric window", paused=paused, stop=recovery_stop)
        # Four actual one-record selections, not empty plans, survive that pressure.
        selections = [(target, delivery) for target, deliveries in accepted.items() for delivery in deliveries][:4]
        ctx.check(len(selections) == 4, "four real public content records exist for the funded archive plans", selections=selections)
        # I23 requires an explicit one-record archive to still work and accepted plans to be pre-funded; how many
        # plans fit beside earlier archive rows (this case already completed one) is not specified. Freeze up to four.
        plans, plan_stop = [], None
        for target, delivery in selections:
            try: plans.append(await archive(sub, agents=[target], deliveries=[delivery], destination=str(destination)))
            except RpcError as error:
                plan_stop = {"kind": error.kind, "message": str(error)}; break
        ctx.check(plans and all(plan.get("records") == 1 and plan.get("state") == "frozen" for plan in plans)
                  and (plan_stop is None or plan_stop["kind"] == "quota"),
                  "one-record archive plans freeze from the archive-only margin while non-archive recovery is refused",
                  accepted=len(plans), stop=plan_stop)
        if len(plans) == 4:
            # Refused either way; at saturation K2 answers busy (no reply slot) where it cannot record a quota refusal,
            # the kind L8 grades.
            fifth = await sub.rejected("inbox.archive", {"op": await sub.operation(), "action": "freeze", "agents": [archive_target["id"]],
                                                         "destination": str(destination)}, {"quota", "busy"})
            sub.evidence.append({"kind": "fifth_plan_refusal", "refusal": fifth.kind, "message": str(fifth)})
            ctx.check(len({row["archive"] for row in plans}) == 4,
                      "four accepted plans are individually funded and the fifth is refused", plans=plans)
        for (target, delivery), plan in zip(selections, plans):
            await archive(sub, "export", archive=plan["archive"])
            result = await archive(sub, "free", archive=plan["archive"])
            lookup = await sub.get(delivery)
            ctx.check(result.get("state") == "completed" and result.get("freed") == ["message/" + delivery] and
                      (await entry(sub, "message/" + delivery)) is None and lookup.get("state") == "archived",
                      "a pre-funded real archive exports and CAS-frees under recovery pressure", result=result, lookup=lookup)
        ctx.check(len(invocations(sub, op="agent.interrupt")) == before_export + 1 and not invocations(sub, op="agent.send"),
                  "archive-only funding never replays either uncertain controls or held content")
        for _ in range(20): await sub.scope("pause", "agent:" + archive_target["id"])
        snapshot = await sub.peer.call("operational.snapshot", {"domain": "coord"})
        scopes = [key for key in snapshot.get("entries", {}) if key.startswith("operational/coord/scope/")]
        ctx.check(len(scopes) == 16, "the public domain retains exactly the latest16 scope receipts", scopes=scopes)
        ctx.evidence.append({"kind": "i23_public_witness", "source_control_op": original_op,
                             "one_record_archive": identifier, "quota_observations": quota_observations,
                             "content_records": sum(map(len, accepted.values())), "future_declaration": future})
    finally:
        await sub.close()
        ctx.evidence.extend(sub.evidence)


async def archive_filesystem_read_cut(ctx):
    """Crash inside real destination verification, with a witnessed blocked read."""
    sub = await sub_coordination(ctx, "io", 2)
    task, writer, fifo = None, None, None
    try:
        sent = await sub.send(text="inside destination verification")
        current = await sub.get(sent["delivery"])
        await sub.command("inbox.withdraw", {"op": await sub.operation(), "delivery": sent["delivery"], "expect": current["revision"]})
        source_key = "message/" + sent["delivery"]
        original = await entry(sub, source_key)
        destination = sub.root / "arc"; destination.mkdir()
        frozen = await archive(sub, agents=[sub.agents[0]["id"]], deliveries=[sent["delivery"]], destination=str(destination))
        identifier = frozen["archive"]
        fifo = destination / (identifier + ".json")
        os.mkfifo(fifo)
        task = asyncio.create_task(archive(sub, "export", archive=identifier))
        async def reader_waiting():
            try:
                # Success proves a real reader opened/is waiting on this FIFO.
                # Keep this writer open without supplying bytes: verification
                # now blocks in its actual filesystem read, not a timer/barrier.
                return {"fd": os.open(fifo, os.O_WRONLY | os.O_NONBLOCK)}
            except OSError as error:
                if error.errno == errno.ENXIO: return None
                raise
        writer = (await sub.wait(reader_waiting, label="export reader inside real destination FIFO", timeout=8))["fd"]
        source = await entry(sub, source_key)
        ctx.check(not task.done() and source is not None and source["value"] == original["value"],
                  "actual destination reader blocks while the public source remains intact", archive=identifier)
        await sub.restart()
        os.close(writer); writer = None
        fifo.unlink()
        outcomes = await asyncio.wait_for(asyncio.gather(task, return_exceptions=True), 2)
        source = await entry(sub, source_key)
        plan = await inspect_archive(sub, identifier)
        ctx.check(source is not None and source["value"] == original["value"] and plan.get("state") == "frozen",
                  "crash inside filesystem export preserves every source and the unconfirmed plan", source=source, plan=plan)
        # Explicit new user operations, never an automatic replay of the lost call.
        await archive(sub, "export", archive=identifier)
        exported = await inspect_archive(sub, identifier)
        data = Path(exported["locator"]).read_bytes()
        ctx.check(hashlib.sha256(data).hexdigest() == exported.get("manifest_hash"),
                  "explicit recovery produces a real verified destination after removing only the owned FIFO", plan=exported)
        result = await archive(sub, "free", archive=identifier)
        lookup = await sub.get(sent["delivery"])
        ctx.check(result.get("state") == "completed" and (await entry(sub, source_key)) is None and lookup.get("state") == "archived",
                  "explicit recovery safely frees the unchanged source only after confirmed export", result=result, lookup=lookup)
        ctx.check(not invocations(sub, op="agent.send"), "filesystem crash recovery executes no native input")
        ctx.evidence.append({"kind": "archive_filesystem_cut", "archive": identifier,
                             "reader_observed_by_nonblocking_writer_open": True, "bytes_supplied": 0,
                             "rpc_after_crash": [type(value).__name__ if isinstance(value, BaseException) else value for value in outcomes]})
    finally:
        if writer is not None: os.close(writer)
        if fifo is not None and fifo.is_fifo(): fifo.unlink()
        if task is not None and not task.done():
            task.cancel(); await asyncio.gather(task, return_exceptions=True)
        try:
            await asyncio.wait_for(sub.close(), 20)
        finally:
            ctx.evidence.extend(sub.evidence)


async def late_typed_receipt_during_export(ctx):
    """A real stored native receipt arrives late, after export and before free: that record stays in place."""
    from copy import deepcopy
    sub = await sub_coordination(ctx, "receipt", 2)
    try:
        target = sub.agents[1]
        row = next(row for row in await sub.resources() if row["id"] == target["id"])
        caps = deepcopy(row["data"]["agent"]["caps"]); caps["agent.send"]["correlation"] = "insertion"
        caps["rev"] = caps.get("rev", 1) + 1
        await sub.native_set(1, caps=caps)
        # The native reply reports only `admitted`; its actual insertion stores the higher `inserted` receipt.
        await sub.native.call("plan", ref=target["backend_ref"], op="agent.send",
                              plan={"result": {"outcome": "accepted", "data": {"receipt": {"stage": "admitted"}}}})
        sent = await sub.send(target=target["id"], text="receipt arrives late")
        await sub.scope("resume", "agent:" + target["id"])
        admitted = await sub.wait_command("inbox.get", lambda r: r.get("state") not in ("held", "recorded", "claimed"),
                                          args={"delivery": sent["delivery"]})
        await sub.scope("pause", "agent:" + target["id"])
        ctx.check(admitted.get("state") == "accepted", "the public record first knows only the admitted reply", record=admitted)
        source_key = "message/" + sent["delivery"]
        original = await entry(sub, source_key)
        destination = sub.root / "late"; destination.mkdir()
        frozen = await archive(sub, agents=[target["id"]], deliveries=[sent["delivery"]], destination=str(destination))
        await archive(sub, "export", archive=frozen["archive"])
        exported = await inspect_archive(sub, frozen["archive"])
        ctx.check(exported.get("state") == "exported", "the plan is exported before the late receipt", plan=exported)
        witness = await sub.native.call("publish-receipt", ref=target["backend_ref"], delivery=sent["delivery"])
        ctx.check(witness["receipt"] == {"delivery": sent["delivery"], "stage": "inserted"} and witness["model_request_seq"],
                  "the late receipt is the native's own stored insertion receipt", witness=witness)
        late = await sub.wait_command("inbox.get", lambda r: r.get("revision") != admitted.get("revision"),
                                      args={"delivery": sent["delivery"]})
        ctx.check(late.get("state") == "inserted", "the late typed receipt advances the public record", record=late)
        freed = await archive(sub, "free", archive=frozen["archive"])
        kept = await entry(sub, source_key)
        ctx.check(freed.get("state") == "partial" and freed.get("changed") == [source_key] and not freed.get("freed")
                  and kept is not None and kept["value"].get("state") == "inserted",
                  "a late receipt during export leaves that record in place, listed changed", freed=freed)
        document = json.loads(Path(exported["locator"]).read_bytes())
        archived = next(row for row in document["records"] if row["delivery"] == source_key)
        ctx.check(archived["record"] == original["value"] and hashlib.sha256(Path(exported["locator"]).read_bytes()).hexdigest()
                  == exported["manifest_hash"], "the exported original stays exact")
        ctx.check(len(invocations(sub, op="agent.send")) == 1 and
                  len([r for r in sub.native.records("model_request") if r.get("delivery") == sent["delivery"]]) == 1,
                  "the late receipt sends and inserts nothing again")
    finally:
        try:
            await asyncio.wait_for(sub.close(), 20)
        finally:
            ctx.evidence.extend(sub.evidence)


async def archive_cut_matrix(ctx):
    """Real protected-committer cuts; never label these destination-fsync errors."""
    if not ctx.manifest.get("core_driver"):
        raise HarnessGap("L11 requires an explicitly pinned archive-cut core driver")
    driver = Path(ctx.manifest["core_driver"][0])
    ctx.evidence.append({"kind": "archive_cut_driver", "path": str(driver),
                         "sha256": hashlib.sha256(driver.read_bytes()).hexdigest(),
                         "revision": ctx.manifest.get("fixture_driver_revision"), "destination_fsync_cut": False})
    stages = ("freeze", "export-intent", "export-confirm", "free")
    actions = ("before", "after", "error", "unconfirmed")
    completed = []
    for index, (stage, action) in enumerate((stage, action) for stage in stages for action in actions):
        sub = await sub_coordination(ctx, f"c{index:02}", 2)
        task = None
        try:
            mode = f"archive-{stage}-{action}"
            # Arm before creating the frozen selection, avoiding an intervening
            # owner restart changing a legitimate pending record's version.
            await sub.restart(fault=mode)
            target = sub.agents[0]["id"]
            sent = await sub.send(text=f"physical cut {stage}/{action}")
            current = await sub.get(sent["delivery"])
            await sub.command("inbox.withdraw", {"op": await sub.operation(), "delivery": sent["delivery"], "expect": current["revision"]})
            source_key = "message/" + sent["delivery"]
            original = await entry(sub, source_key)
            ctx.check(original is not None and original["value"]["state"] == "withdrawn",
                      "cut fixture retains real withdrawn evidence without a native invoke", mode=mode, source=original)
            destination = sub.root / "arc"; destination.mkdir()
            identifier, frozen = None, None
            if stage != "freeze":
                plan = await archive(sub, agents=[target], deliveries=[sent["delivery"]], destination=str(destination))
                identifier = plan["archive"]
                frozen = await inspect_archive(sub, identifier)
                ctx.check(frozen["frozen"] == {source_key: original["version"]},
                          "archive cut names the actual public source version", mode=mode, frozen=frozen)
            if stage == "free":
                await archive(sub, "export", archive=identifier)
                plan = await inspect_archive(sub, identifier)
                ctx.check(plan.get("state") == "exported", "free cut is armed only after a real export confirmation", mode=mode, plan=plan)
            async def invoke():
                try:
                    if stage == "freeze":
                        value = await archive(sub, agents=[target], deliveries=[sent["delivery"]], destination=str(destination))
                    else:
                        value = await archive(sub, "free" if stage == "free" else "export", archive=identifier)
                    return {"reply": value}
                except (RpcError, WireFailure) as error:
                    return {"rpc_error": getattr(error, "kind", type(error).__name__), "message": str(error)}
            task = asyncio.create_task(invoke())
            phase = (f"after-durable-archive-{stage}-before-receipt" if action == "after" else
                     f"after-rename-archive-{stage}-unconfirmed" if action == "unconfirmed" else
                     f"before-archive-{stage}-write")
            await sub.wait_cut(phase)
            if action in ("before", "after"):
                ctx.check(not task.done(), "physical cut holds the real archive RPC before crash", mode=mode, phase=phase)
                await sub.restart()
                outcome = await asyncio.wait_for(task, 2)
            else:
                outcome = await asyncio.wait_for(task, 10)
                await sub.restart()
            source = await entry(sub, source_key)
            plans = (await sub.peer.call("operational.snapshot", {"domain": "coord"}))["entries"]
            if identifier is None:
                matches = [(key.rsplit("/", 1)[-1], row["value"]) for key, row in plans.items()
                           if key.startswith("operational/coord/archive/") and source_key in row["value"].get("frozen", {})]
                ctx.check(len(matches) <= 1, "freeze cut never creates a second archive selection", mode=mode, matches=matches)
                identifier, plan = matches[0] if matches else (None, None)
            else:
                plan = await inspect_archive(sub, identifier)
            committed_free = stage == "free" and action in ("after", "unconfirmed")
            if committed_free:
                lookup = await sub.get(sent["delivery"])
                ctx.check(source is None and plan.get("state") == "completed" and lookup.get("state") == "archived",
                          "committed free atomically replaces source with its public verified archive locator", mode=mode, plan=plan, lookup=lookup)
            else:
                ctx.check(source is not None and source["value"] == original["value"],
                          "an incomplete or refused archive step retains every exact source field", mode=mode, source=source)
            needs_bundle = stage in ("export-confirm", "free")
            path = destination / (str(identifier) + ".json")
            if needs_bundle:
                data = path.read_bytes(); document = json.loads(data)
                records = document.get("records", [])
                expected_records = [{"delivery": source_key, "version": original["version"], "record": original["value"]}]
                ctx.check(records == expected_records and document["manifest"].get("archive") == identifier and
                          document["manifest"].get("content_hash") == hashlib.sha256(canonical(records)).hexdigest(),
                          "actual destination bytes preserve the exact public frozen record and manifest hash", mode=mode, bytes=len(data))
                if plan and plan.get("state") in ("exported", "completed"):
                    ctx.check(plan.get("locator") == str(path) and plan.get("manifest_hash") == hashlib.sha256(data).hexdigest(),
                              "public export confirmation matches the real destination", mode=mode, plan=plan)
            else:
                ctx.check(not list(destination.iterdir()), "freeze or export-intent cut executes no filesystem export", mode=mode)
            ctx.check(not invocations(sub, op="agent.send"), "archive durability cut never replays native input", mode=mode)
            completed.append(mode)
            ctx.evidence.append({"kind": "archive_fault_witness", "mode": mode, "phase": phase,
                                 "core_plan_cut_not_destination_fsync": True, "outcome": outcome,
                                 "source_key": source_key, "archive": identifier, "plan": plan})
        finally:
            if task is not None and not task.done():
                task.cancel(); await asyncio.gather(task, return_exceptions=True)
            try:
                await asyncio.wait_for(sub.close(), 20)
            finally:
                ctx.evidence.extend(sub.evidence)
    ctx.check(len(completed) == 16, "all four real archive transitions execute before/after/error/unconfirmed cuts", completed=completed)


@case("L11")
async def archive_crash_and_cas_fences(ctx):
    target = ctx.agents[0]["id"]
    destination = ctx.root / "archive"; destination.mkdir()
    baseline = await ctx.send(text="ordinary pending archive, no crash or concurrent sender")
    first = await archive(ctx, agents=[target], deliveries=[baseline["delivery"]], destination=str(destination))
    frozen_plan = await inspect_archive(ctx, first["archive"])
    source_before = await entry(ctx, "message/" + baseline["delivery"])
    export_reply = await archive(ctx, "export", archive=first["archive"])
    confirmed = await inspect_archive(ctx, first["archive"])
    source_after = await entry(ctx, "message/" + baseline["delivery"])
    ctx.check(confirmed.get("state") == "exported", "an ordinary frozen pending record can export without a crash or competing edit",
              frozen=frozen_plan, source_before=source_before, source_after=source_after, export_reply=export_reply, plan=confirmed)
    ordinary_free = await archive(ctx, "free", archive=first["archive"])
    ctx.check(ordinary_free.get("freed") == ["message/" + baseline["delivery"]], "ordinary pending archive frees exactly its selected record", reply=ordinary_free)
    located = await ctx.get(baseline["delivery"])
    ctx.check(located.get("state") == "archived" and located.get("locator") == confirmed["locator"],
              "archived delivery lookup returns its actual locator", lookup=located)
    Path(confirmed["locator"]).unlink()
    ctx.check((await ctx.get(baseline["delivery"])).get("state") == "unknown", "missing external archive is unknown, not no effect")
    failed = await ctx.send(text="failed destination keeps source", send_new=True)
    failed_plan = await archive(ctx, agents=[target], deliveries=[failed["delivery"]], destination=str(ctx.root / "absent-destination"))
    failed_export = await archive(ctx, "export", archive=failed_plan["archive"])
    ctx.check((await inspect_archive(ctx, failed_plan["archive"])).get("state") == "frozen" and
              (await ctx.get(failed["delivery"]))["content"] == [{"type": "text", "text": "failed destination keeps source"}],
              "actual nonexistent destination write confirms nothing and frees no source", reply=failed_export)
    sent = await ctx.send(text="original archive evidence", send_new=True)
    await held(ctx, sent["delivery"], "paused")
    original = await ctx.get(sent["delivery"])
    freeze_op = await ctx.operation("freeze")
    args = {"op": freeze_op, "agents": [target], "deliveries": [sent["delivery"]], "destination": str(destination)}
    frozen = await ctx.command("inbox.archive", args)
    identifier = frozen["archive"]
    repeated = await ctx.command("inbox.archive", args)
    ctx.check(repeated.get("archive") == identifier and repeated.get("records") == frozen.get("records") and
              repeated.get("state") == frozen.get("state") and repeated.get("historical") is True,
              "repeated freeze op inspects the original archive, not a new selection", reply=repeated)
    await ctx.rejected("inbox.archive", {"op": await ctx.operation(), "action": "free", "archive": identifier}, {"conflict"})
    ctx.check((await ctx.get(sent["delivery"]))["content"] == original["content"], "free before export retains the exact source")
    await ctx.restart()
    ctx.check((await inspect_archive(ctx, identifier))["state"] == "frozen" and
              (await ctx.get(sent["delivery"]))["content"] == original["content"],
              "actual private-core crash after freeze retains plan and source")
    # The freeze-cut fixture is independent: recovery may change its source
    # version, which must never be silently installed as the old frozen cut.
    sent = await ctx.send(target=ctx.agents[1]["id"], text="independent after-export crash evidence", send_new=True)
    original = await ctx.get(sent["delivery"])
    export_plan = await archive(ctx, agents=[ctx.agents[1]["id"]], deliveries=[sent["delivery"]], destination=str(destination))
    identifier = export_plan["archive"]
    export_frozen = await inspect_archive(ctx, identifier)
    await archive(ctx, "export", archive=identifier)
    exported = await inspect_archive(ctx, identifier)
    ctx.check(exported.get("state") == "exported", "independent archive has real confirmed export before its crash cut", plan=exported)
    exported_bytes = Path(exported["locator"]).read_bytes()
    ctx.check(hashlib.sha256(exported_bytes).hexdigest() == exported["manifest_hash"],
              "public exported confirmation matches actual private destination bytes")
    await ctx.restart()
    ctx.check((await inspect_archive(ctx, identifier))["state"] == "exported" and
              (await ctx.get(sent["delivery"]))["content"] == original["content"],
              "actual crash after export before free retains source and verified plan")
    # Tampering only with the fixture-owned destination is an external failure,
    # never a mutation of target operational evidence.
    Path(exported["locator"]).write_bytes(b"changed fixture destination")
    await ctx.rejected("inbox.archive", {"op": await ctx.operation(), "action": "free", "archive": identifier}, {"conflict"})
    ctx.check((await ctx.get(sent["delivery"]))["content"] == original["content"], "failed destination readback frees nothing")
    Path(exported["locator"]).write_bytes(exported_bytes)
    with Path(exported["locator"]).open("rb") as source:
        import os
        os.fsync(source.fileno())
    source_key = "message/" + sent["delivery"]
    before_free = await entry(ctx, source_key)
    current = await ctx.get(sent["delivery"])
    await ctx.rejected("inbox.edit", {"op": await ctx.operation(), "delivery": sent["delivery"], "expect": current["revision"],
                                      "content": [{"type": "text", "text": "cannot replace a frozen source"}]}, {"conflict"})
    ctx.check((await ctx.get(sent["delivery"]))["content"] == original["content"], "an edit of frozen evidence is refused without losing its source")
    freed = await archive(ctx, "free", archive=identifier)
    if before_free["version"] == export_frozen["frozen"][source_key]:
        ctx.check(freed.get("state") == "completed" and freed.get("freed") == [source_key] and (await entry(ctx, source_key)) is None,
                  "confirmed unchanged source is freed by exact CAS", result=freed)
    else:
        ctx.check(freed.get("state") == "partial" and freed.get("changed") == [source_key] and not freed.get("freed") and
                  (await ctx.get(sent["delivery"]))["content"] == original["content"],
                  "a real post-freeze version change is retained instead of stale-CAS deletion", result=freed, before=before_free)

    ctx.check(not invocations(ctx, op="agent.send"), "archive and crash recovery never send the held source")
    await archive_cut_matrix(ctx)
    await archive_filesystem_read_cut(ctx)
    await late_typed_receipt_during_export(ctx)
