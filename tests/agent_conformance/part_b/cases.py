# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Coordination policy probes over public RPC. No target module/state-machine imports."""
import asyncio
from copy import deepcopy
import hashlib
import json
import time

from ..results import CheckFailed, FixtureFailed, HarnessGap
from ..wire import RpcError
from pathlib import Path

HANDLERS = {}


def case(identifier):
    def register(function):
        if identifier in HANDLERS: raise ValueError("duplicate Part B handler: " + identifier)
        HANDLERS[identifier] = function
        return function
    return register


def invocations(ctx, *, delivery=None, after=0, op=None):
    rows = ctx.native.records("invoke", after=after)
    return [row for row in rows if (op is None or row["op"] == op) and
            (delivery is None or row.get("args", {}).get("delivery") == delivery)]


async def native_idle(ctx, index=0):
    resource = ctx.agents[index]
    await ctx.native.call("point", ref=resource["backend_ref"], point="idle")
    async def ready():
        rows = await ctx.resources()
        return any(r["id"] == resource["id"] and r["data"]["agent"]["state"] == "idle" for r in rows)
    await ctx.wait(ready, label="native idle facts")


async def settled(ctx, delivery):
    return await ctx.wait_command("inbox.get", lambda row: row.get("state") not in ("held", "recorded", "claimed"),
                                  args={"delivery": delivery})


async def held(ctx, delivery, reason=None):
    return await ctx.wait_command("inbox.get", lambda row: row.get("state") == "held" and
                                  (reason is None or row.get("reason") == reason), args={"delivery": delivery})


async def ordinary(ctx, operations, *, peer=None, **fields):
    return await (peer or ctx.peer).call("workspace.apply", {"ops": operations, **fields})


async def history(ctx, peer=None):
    result = await (peer or ctx.peer).call("workspace.history", {"limit": 100000})
    return result, len(json.dumps(result, ensure_ascii=False, sort_keys=True, separators=(",", ":")).encode())


@case("I1")
async def claim_precedes_native(ctx):
    sent = await ctx.send()
    before = await held(ctx, sent["delivery"], "paused")
    ctx.check(before.get("attempt") is None, "paused mail has no durable claim")
    await ctx.scope("resume")
    await settled(ctx, sent["delivery"])
    calls = invocations(ctx, delivery=sent["delivery"], op="agent.send")
    ctx.check(len(calls) == 1, "native adapter sees exactly one delivery", calls=calls)
    entry = (calls[0].get("witness") or {}).get("entry") or {}
    claim = entry.get("value", {})
    ctx.check(claim.get("state") == "claimed", "real operational read at native invoke sees claimed")
    ctx.check(claim.get("delivery") == sent["delivery"] and claim.get("attempt", {}).get("id"),
              "claim names exact delivery and attempt")
    ctx.check(claim["attempt"].get("control_epoch"), "native witness includes claimant control epoch")
    disk=(calls[0].get("disk_witness") or {}).get("entry") or {}
    ctx.check(disk.get("value",{}).get("state")=="claimed" and
              disk["value"].get("attempt",{}).get("id")==claim["attempt"]["id"],
              "physical private-root domain file already contains the same claim before native invoke",
              component="core",disk_witness=calls[0].get("disk_witness"))


@case("I15")
async def resolve_requires_standing(ctx):
    worker = await ctx.connect("worker:no-grants")
    sent = await ctx.send()
    mark = ctx.native.mark()
    # A real unknown attempt is required for the positive sender/recipient/recover resolution branch.
    await ctx.native.call("plan", ref=ctx.agents[0]["backend_ref"], op="agent.send",
                          plan={"result": {"outcome": "indeterminate", "error": {"kind": "lost", "message": "fixture gap"}}})
    await ctx.scope("resume")
    await ctx.wait_command("inbox.get", lambda row: row.get("state") == "unknown", args={"delivery": sent["delivery"]})
    mark = ctx.native.mark()
    await ctx.rejected("inbox.resolve", {"op": await ctx.operation(), "delivery": sent["delivery"], "as": "not_delivered"},
                       {"no_grant"}, peer=worker)
    ctx.check(not invocations(ctx, after=mark), "unauthorized resolution of a real unknown attempt makes no native call")
    result = await ctx.command("inbox.resolve", {"op": await ctx.operation(), "delivery": sent["delivery"], "as": "not_delivered"})
    ctx.check(result.get("state") not in ("claimed", "recorded"), "resolution records judgment, not dispatch")
    ctx.check(not invocations(ctx, after=mark), "positive resolution itself never invokes")


@case("I16")
async def pending_exists(ctx):
    first = await ctx.send(text="first")
    second = {"op": await ctx.operation(), "submission": await ctx.operation("submission"), "to": ctx.agents[0]["id"],
              "mode": "start", "content": [{"type": "text", "text": "second"}]}
    mark = ctx.native.mark()
    await ctx.rejected("inbox.send", second, {"pending_exists"})
    allowed = await ctx.command("inbox.send", {**second, "op": await ctx.operation(), "send_new": True})
    ctx.check(allowed["delivery"] != first["delivery"], "explicit send_new creates a distinct delivery")
    ctx.check(not invocations(ctx, after=mark), "pending admissions while paused do not invoke")


@case("H1")
async def restart_before_claim(ctx):
    sent = await ctx.send()
    await held(ctx, sent["delivery"], "paused")
    await ctx.restart()
    record = await held(ctx, sent["delivery"], "paused")
    ctx.check(record.get("attempt") is None, "crash before claim remains never attempted")
    ctx.check(not invocations(ctx, delivery=sent["delivery"]), "restart does not invoke unsent mail")
    await ctx.scope("resume")
    await settled(ctx, sent["delivery"])
    ctx.check(len(invocations(ctx, delivery=sent["delivery"])) == 1, "fresh explicit resume permits exactly one delivery")


@case("H5")
async def stale_resume(ctx):
    old = await ctx.command("scope", {"action": "inspect", "scope": "workspace"})
    sent = await ctx.send()
    old_operation = await ctx.operation("resume-before-crash")
    await ctx.restart(fault="claim-before")
    await ctx.scope("resume")
    await ctx.wait_cut("before-claim-write")
    disk=json.loads((Path(ctx.backend.env["VQRO_STATE_DIR"])/"operational/coord.json").read_text())
    control=disk["entries"]["operational/coord/control"]["value"]
    ctx.check(control["gates"].get(ctx.agents[0]["id"]) is True,
              "real persisted open gate exists at the pre-claim writer cut")
    await ctx.restart(crash=True)
    await ctx.rejected("scope", {"op": old_operation, "action": "resume", "scope": "workspace", "expect": old["expect"]},
                       {"conflict", "expired_operation"})
    ctx.check((await ctx.get(sent["delivery"]))["state"] == "held", "stale resume cannot open restored gates")
    ctx.check(not invocations(ctx, delivery=sent["delivery"]), "restored open gate and stale resume create zero native permits")


@case("H9")
async def scope_grants_atomic(ctx):
    worker = await ctx.connect("worker:resume-none")
    await ctx.native_set(1, principal="worker:resume-none")
    await ctx.grant("partial-resume", "worker:resume-none", ["resume"], "agent:" + ctx.agents[0]["id"])
    sent = await ctx.send()
    preview = await ctx.command("scope", {"action": "inspect", "scope": "workspace"})
    ctx.check(ctx.agents[1]["id"] in preview["targets"], "scope includes caller's own ungranted endpoint")
    await ctx.rejected("scope", {"op": await ctx.operation(), "action": "resume", "scope": "workspace", "expect": preview["expect"]},
                       {"no_grant"}, peer=worker)
    after = await ctx.command("scope", {"action": "inspect", "scope": "workspace"})
    ctx.check(after["expect"] == preview["expect"], "failed scope grant leaves control revision unchanged")
    ctx.check(not invocations(ctx, delivery=sent["delivery"]), "failed scope grant opens no member")


@case("H10")
async def resume_never_renews_expired_preempt(ctx):
    expires = int(time.time() * 1000) + 1000
    sent = await ctx.send(mode="preempt", expires_at_ms=expires)
    pending = await held(ctx, sent["delivery"], "paused")
    ctx.check(pending["created_ms"] < expires, "preempt was admitted before its original expiry")
    await ctx.wait(lambda: int(time.time() * 1000) >= expires, label="original preempt expiry")
    await ctx.scope("resume")
    record = await ctx.wait_command("inbox.get", lambda row: row.get("state") == "expired", args={"delivery": sent["delivery"]})
    ctx.check(record["expires_at_ms"] == expires, "resume keeps the exact expired deadline")
    ctx.check(not invocations(ctx, delivery=sent["delivery"]), "expired preempt never reaches native")
    await ctx.scope("pause")
    future = int(time.time() * 1000) + 60000
    session = ctx.agents[1]["data"]["agent"]["session"]["id"]
    stale = await ctx.send(target=ctx.agents[1]["id"], mode="preempt", expires_at_ms=future, guard={"session":session})
    await ctx.native_set(1, session={"id":"s2-after-send"}, epoch="e2-after-send")
    await ctx.scope("resume", "agent:" + ctx.agents[1]["id"])
    record = await held(ctx, stale["delivery"])
    ctx.check(record.get("reason") in ("session_changed", "stale_guard"), "stale preempt remains for review")
    ctx.check(record["guard"]["session"] == session and record["expires_at_ms"] == future,
              "resume cannot rewrite guard or renew preempt expiry")
    ctx.check(not invocations(ctx, delivery=stale["delivery"]), "stale preempt never invokes native")


@case("G1")
async def principal_not_label(ctx):
    caller = await ctx.connect("worker:alice", label="owner")
    result = await ordinary(ctx, [{"op": "set", "key": "doc/part-b/label", "value": "alice", "expect": 0}], peer=caller, label="owner")
    hist = (await caller.call("workspace.history", {"limit": 1}))["txns"]
    ctx.check(hist and hist[-1].get("principal") == "worker:alice" and hist[-1].get("label") == "owner", "declared owner label does not change connection principal", transaction=result, history=hist)
    raise FixtureFailed("G1 launcher-injected pane CLI/TUI identity branch requires actual managed launcher fixture")


@case("G2")
async def per_principal_undo(ctx):
    alice, bob = await ctx.connect("worker:alice"), await ctx.connect("worker:bob")
    a = await ordinary(ctx, [{"op": "set", "key": "doc/part-b/alice", "value": "A", "expect": 0}], peer=alice)
    b = await ordinary(ctx, [{"op": "set", "key": "doc/part-b/bob", "value": "B", "expect": 0}], peer=bob)
    await alice.call("command.invoke", {"name": "vqro.revert", "args": {}})
    snapshot = await ctx.peer.call("workspace.snapshot")
    ctx.check("doc/part-b/alice" not in snapshot["entries"], "Alice undo removes Alice's change")
    ctx.check(snapshot["entries"]["doc/part-b/bob"]["value"] == "B", "Alice undo leaves Bob's later change")
    await alice.call("workspace.revert", {"txn": b["txn"]})
    snapshot = await ctx.peer.call("workspace.snapshot")
    ctx.check("doc/part-b/bob" not in snapshot["entries"], "Bob's transaction remains explicitly revertable by id")
    ctx.evidence.append({"alice_txn": a, "bob_txn": b})
    raise FixtureFailed("G2 still requires actual launcher-tagged pane CLI and physical Alice TUI u; raw RPC is not a substitute")


@case("G5")
async def undeclared_is_not_owner(ctx):
    raw = await ctx.connect(None)
    sent = {"op": await ctx.operation(), "submission": await ctx.operation(), "to": ctx.agents[0]["id"],
            "mode": "start", "content": [{"type": "text", "text": "ungranted"}]}
    await ctx.rejected("inbox.send", sent, {"no_grant"}, peer=raw)
    await ordinary(ctx, [{"op": "set", "key": "doc/part-b/undeclared", "value": True, "expect": 0}], peer=raw)
    hist = (await raw.call("workspace.history", {"limit": 1}))["txns"]
    ctx.check(hist[-1].get("principal") == "undeclared", "raw write attribution is undeclared, never owner", history=hist)
    try: await raw.call("command.invoke", {"name": "vqro.revert", "args": {}})
    except RpcError as error: ctx.check(error.kind in ("invalid", "no_grant", "forbidden"), "undeclared has no default undo")
    else: raise CheckFailed("undeclared caller received default undo", component="core")


@case("G10")
async def grants_query_not_authority(ctx):
    worker = await ctx.connect("worker:no-grants")
    mark = ctx.native.mark()
    await ctx.rejected("control", {"op": await ctx.operation(), "target": ctx.agents[0]["id"], "action": "interrupt", "args": {"v": 0}},
                       {"no_grant"}, peer=worker)
    ctx.check(not invocations(ctx, after=mark), "coordinated no_grant dispatches zero native calls")
    raw = await worker.call("resource.invoke", {"resource": ctx.agents[0]["id"], "op": "agent.interrupt", "args": {"v": 0}})
    ctx.check(raw.get("outcome") in ("accepted", "observed", "dispatched"), "raw primitive remains available independently of coordination grants")
    await ctx.grant("known-but-other","worker:somebody-else",["interrupt"])
    queried=(await worker.call("workspace.snapshot"))["entries"]
    ctx.check("doc/agents/grants/known-but-other" in queried and "doc/agents/grants/unknown-grant" not in queried,
              "public grant queries expose configuration, not standing")
    mark=ctx.native.mark()
    await ctx.rejected("control",{"op":await ctx.operation(),"target":ctx.agents[0]["id"],"action":"interrupt","args":{"v":0}},
                       {"no_grant"},peer=worker)
    await ctx.grant("revoked-interrupt","worker:no-grants",["interrupt"])
    queried=(await worker.call("workspace.snapshot"))["entries"]["doc/agents/grants/revoked-interrupt"]
    await ctx.command("grants",{"op":await ctx.operation(),"id":"revoked-interrupt","value":None,"expect":queried["version"]})
    await ctx.rejected("control",{"op":await ctx.operation(),"target":ctx.agents[0]["id"],"action":"interrupt","args":{"v":0}},
                       {"no_grant"},peer=worker)
    ctx.check(not invocations(ctx,after=mark),"unknown/inapplicable and revoked grants authorize zero coordinated native calls")


@case("G12")
async def principal_writes_checked_before_transaction(ctx):
    worker = await ctx.connect("worker:W")
    key = "doc/principals/delegations/part-b-owner-row"
    owner = (await ctx.peer.call("workspace.snapshot"))["entries"]["doc/principals/owner"]["value"]
    for operations in (
        [{"op": "set", "key": "doc/principals/owner", "value": "worker:W"}],
        [{"op": "delete", "key": "doc/principals/owner"}],
        [{"op": "set", "key": key, "value": {"from": owner, "to": "worker:W"}}],
    ):
        try: await ordinary(ctx, operations, peer=worker)
        except RpcError as error: ctx.check(error.kind in ("forbidden", "no_grant"), "non-owner cannot change owner/from-P delegation")
        else: raise CheckFailed("principal authority escalation accepted", component="core")
    async def refused(method,params,*,caller=worker):
        try: await caller.call(method,params)
        except RpcError as error:
            ctx.check(error.kind in ("forbidden","no_grant"),"principal write ownership checked before commit",component="core",refusal=str(error))
        else: raise CheckFailed("principal ownership mutation unexpectedly accepted",component="core")
    # No existing standing: a row cannot authorize the rest of the transaction that creates it.
    await refused("workspace.apply",{"on_behalf_of":owner,"ops":[
        {"op":"set","key":key,"value":{"from":owner,"to":"worker:W"}},
        {"op":"set","key":"doc/part-b/from-new-delegation","value":True}]})
    ctx.check(key not in (await ctx.peer.call("workspace.snapshot"))["entries"],"self-authorizing transaction is rejected whole",component="core")
    txns=(await ctx.peer.call("workspace.history",{"limit":1000}))["txns"]
    bootstrap=[row for row in txns if row.get("principal")=="system" and str(row.get("author","")).startswith("system:")]
    ctx.check(bool(bootstrap),"owner bootstrap has audited system attribution",component="core",bootstrap=bootstrap)
    owner_before=(await ctx.peer.call("workspace.snapshot"))["entries"]["doc/principals/owner"]
    try: await ctx.peer.call("command.invoke",{"name":"vqro.revert","args":{}})
    except RpcError as error: ctx.check(error.kind in ("not_found","invalid"),"no owner undo target before owner writes",component="core")
    ctx.check((await ctx.peer.call("workspace.snapshot"))["entries"]["doc/principals/owner"]==owner_before,
              "default owner undo cannot select system bootstrap",component="core")
    owner_write=await ordinary(ctx,[{"op":"set","key":"doc/principals/owner","value":owner}])
    await refused("workspace.revert",{"txn":owner_write["txn"]})
    own_key="doc/principals/delegations/part-b-own"
    await ordinary(ctx,[{"op":"set","key":own_key,"value":{"from":"worker:W","to":"worker:W"}}],peer=worker)
    await refused("workspace.apply",{"ops":[{"op":"set","key":own_key,"value":{"from":owner,"to":"worker:W"}}]})
    replace=await ordinary(ctx,[{"op":"set","key":own_key,"value":{"from":owner,"to":"worker:W"}}])
    await refused("workspace.apply",{"ops":[{"op":"set","key":own_key,"value":{"from":"worker:W","to":"worker:W"}}]})
    await refused("workspace.apply",{"ops":[{"op":"delete","key":own_key}]})
    await refused("workspace.revert",{"txn":replace["txn"]})
    row=(await ctx.peer.call("workspace.snapshot"))["entries"][own_key]
    expiry=int(time.time()*1000)+1000
    await ordinary(ctx,[{"op":"set","key":own_key,"value":{"from":owner,"to":"worker:W","until_ms":expiry},"expect":row["version"]}])
    delegated=await ordinary(ctx,[{"op":"set","key":"doc/part-b/delegated-before-expiry","value":True}],peer=worker,on_behalf_of=owner)
    history_row=(await worker.call("workspace.history",{"limit":1}))["txns"][-1]
    ctx.check(history_row.get("actor")=="worker:W" and history_row.get("principal")==owner and history_row.get("initiator")=="worker:W",
              "standing delegation records actual actor and initiator separately",component="core",transaction=delegated)
    await ctx.wait(lambda:int(time.time()*1000)>=expiry,label="standing delegation expiry")
    await refused("workspace.apply",{"on_behalf_of":owner,"ops":[{"op":"set","key":"doc/part-b/after-expiry","value":True}]})


async def member(ctx, index, project, reports_to=None):
    agent = ctx.agents[index]["id"]
    key = "doc/agents/members/" + agent
    entries = (await ctx.peer.call("workspace.snapshot"))["entries"]
    return await ctx.command("membership.set", {"op":await ctx.operation(), "agent":agent,
        "project":project,"reports_to":reports_to,"expect":entries.get(key,{}).get("version",0)})


@case("G3")
async def additive_dynamic_grants(ctx):
    principal="worker:dynamic"; worker=await ctx.connect(principal)
    await ctx.native_set(1,principal=principal)
    await member(ctx,0,"p1"); await member(ctx,1,"p1",ctx.agents[0]["id"])
    target="agent:"+ctx.agents[0]["id"]
    await ctx.grant("project-start","project:p1",["send:start"],target)
    await ctx.grant("team-interrupt","team:"+ctx.agents[0]["id"],["interrupt"],target)
    await ctx.grant("direct-boundary",principal,["send:boundary"],target)
    await ctx.grant("expired-pause",principal,["pause"],target,until_ms=int(time.time()*1000)-1)
    sent=await ctx.send(peer=worker)
    ctx.check(sent.get("delivery"),"project grant authorizes its present member")
    answer=await ctx.command("control",{"op":await ctx.operation(),"action":"interrupt","target":ctx.agents[0]["id"],"args":{"v":0}},peer=worker)
    ctx.check(answer.get("state") not in ("rejected","unknown"),"team grant adds interrupt independently")
    preview=await ctx.command("scope",{"action":"inspect","scope":target})
    await ctx.rejected("scope",{"op":await ctx.operation(),"action":"pause","scope":target,"expect":preview["expect"]},{"no_grant"},peer=worker)
    await member(ctx,1,"p2")
    await ctx.rejected("inbox.send",{"op":await ctx.operation(),"submission":await ctx.operation(),"to":ctx.agents[0]["id"],
        "mode":"start","content":[{"type":"text","text":"after project move"}],"send_new":True},{"no_grant"},peer=worker)
    await ctx.rejected("control",{"op":await ctx.operation(),"action":"interrupt","target":ctx.agents[0]["id"],"args":{"v":0}},{"no_grant"},peer=worker)
    direct=await ctx.send(peer=worker,mode="boundary",text="direct grant remains",send_new=True)
    ctx.check(direct.get("delivery"),"membership movement removes group rights, not additive direct rights")
    ctx.check(not invocations(ctx,delivery=sent["delivery"]),"grant checks do not open send gates")


@case("G6")
async def grant_survives_restart(ctx):
    principal="worker:persistent"; until=int(time.time()*1000)+60000
    await ctx.grant("persistent-start",principal,["send:start"],until_ms=until)
    before=(await ctx.peer.call("workspace.snapshot"))["entries"]["doc/agents/grants/persistent-start"]
    worker=await ctx.connect(principal)
    transaction=await ordinary(ctx,[{"op":"set","key":"doc/part-b/persistent-attribution","value":True,"expect":0}],peer=worker)
    previous=(await worker.call("workspace.history",{"limit":1}))["txns"][-1]
    await ctx.restart()
    after=(await ctx.peer.call("workspace.snapshot"))["entries"]["doc/agents/grants/persistent-start"]
    ctx.check(after==before,"restart preserves the same unexpired grant row/version")
    retained=(await ctx.peer.call("workspace.history",{"limit":1000}))["txns"]
    ctx.check(previous in retained and previous.get("principal")==principal,
              "ordinary attribution survives restart unchanged",transaction=transaction)
    worker=await ctx.connect(principal);sent=await ctx.send(peer=worker)
    await ctx.scope("resume");await settled(ctx,sent["delivery"])
    calls=invocations(ctx,delivery=sent["delivery"])
    ctx.check(len(calls)==1 and calls[0]["args"]["origin"]["principal"]==principal,
              "same declared principal uses its retained grant, without owner rebinding")


@case("G7")
async def name_and_self_are_not_grants(ctx):
    principal="worker:self";worker=await ctx.connect(principal)
    await ctx.native_set(0,principal=principal)
    await ctx.command("names",{"op":await ctx.operation(),"handle":"@worker","agent":ctx.agents[0]["id"],"expect":0})
    version=(await ctx.peer.call("workspace.snapshot"))["entries"]["doc/agents/names/@worker"]["version"]
    scope="agent:"+ctx.agents[0]["id"]
    preview=await ctx.command("scope",{"action":"inspect","scope":scope})
    await ctx.rejected("scope",{"op":await ctx.operation(),"action":"resume","scope":scope,"expect":preview["expect"]},{"no_grant"},peer=worker)
    await ctx.grant("explicit-self-resume",principal,["resume"],scope)
    preview=await ctx.command("scope",{"action":"inspect","scope":scope})
    resumed=await ctx.command("scope",{"op":await ctx.operation(),"action":"resume","scope":scope,"expect":preview["expect"]},peer=worker)
    ctx.check(resumed.get("state")=="accepted" and resumed.get("targets")==[ctx.agents[0]["id"]],
              "explicit self resume grant enables exactly its target")
    operations=(
        ("names",{"handle":"@worker","agent":ctx.agents[1]["id"],"expect":version}),
        ("grants",{"id":"self-escalation","value":{"who":principal,"on":"agent:"+ctx.agents[0]["id"],"can":["grants.manage","resume"]},"expect":0}),
        ("membership.set",{"agent":ctx.agents[0]["id"],"project":"self-project","reports_to":None,"expect":0}),
        ("handover",{"from":{"agent":ctx.agents[0]["id"],"incarnation":ctx.agents[0]["incarnation"]},
            "to":{"agent":ctx.agents[1]["id"],"incarnation":ctx.agents[1]["incarnation"]},"deliveries":[]}),
    )
    mark=ctx.native.mark()
    for command,args in operations:
        if command == "handover": await ctx.native_set(0,lifecycle="ended")
        await ctx.rejected(command,{"op":await ctx.operation(),"label":"owner",**args},{"no_grant"},peer=worker)
    ctx.check(not invocations(ctx,after=mark),"self identity/handle/free label creates no management standing or native call")


@case("G8")
async def management_cannot_widen_rights(ctx):
    principal="worker:limited-manager";worker=await ctx.connect(principal)
    target="agent:"+ctx.agents[0]["id"]
    await ctx.grant("manager",principal,["grants.manage","resume"],target)
    await ctx.grant("old-broad","worker:third",["resume"],"*")
    await ctx.grant("narrow-delegation","worker:third",["resume"],target,peer=worker)
    attempts=(
        {"id":"widen-scope","value":{"who":"worker:third","on":"*","can":["resume"]},"expect":0},
        {"id":"widen-right","value":{"who":principal,"on":target,"can":["send:start"]},"expect":0},
        {"id":"other-target","value":{"who":"worker:third","on":"agent:"+ctx.agents[1]["id"],"can":["resume"]},"expect":0},
    )
    for args in attempts: await ctx.rejected("grants",{"op":await ctx.operation(),**args},{"no_grant"},peer=worker)
    old=(await ctx.peer.call("workspace.snapshot"))["entries"]["doc/agents/grants/old-broad"]
    await ctx.rejected("grants",{"op":await ctx.operation(),"id":"old-broad","expect":old["version"],
        "value":{"who":"worker:third","on":target,"can":["resume"]}},{"no_grant"},peer=worker)
    ctx.check((await ctx.peer.call("workspace.snapshot"))["entries"]["doc/agents/grants/old-broad"]==old,
              "old broad scope is checked too; failed replacement has no partial change")
    outsider=await ctx.connect("worker:bootstrap-manager")
    await ctx.rejected("grants",{"op":await ctx.operation(),"id":"bootstrap-own-grant","expect":0,
        "value":{"who":"worker:bootstrap-manager","on":target,"can":["grants.manage","resume"]}},{"no_grant"},peer=outsider)
    bounded="worker:bounded-manager";bounded_peer=await ctx.connect(bounded)
    until=int(time.time()*1000)+60000
    await ctx.grant("bounded-manager",bounded,["grants.manage","resume"],target,until_ms=until)
    await ctx.grant("bounded-valid","worker:third",["resume"],target,until_ms=until,peer=bounded_peer)
    await ctx.rejected("grants",{"op":await ctx.operation(),"id":"extends-expiry","expect":0,
        "value":{"who":"worker:third","on":target,"can":["resume"],"until_ms":until+1}},{"no_grant"},peer=bounded_peer)
    ctx.check(not invocations(ctx),"grant management never invokes an agent")


@case("I2")
async def crash_cut_matrix(ctx):
    from .context import PublicCoordination
    for index,cut in enumerate(("claim-before","claim-after","native-before-result","after-result")):
        sub=PublicCoordination(ctx.root/str(index),ctx.manifest,ctx.vqro)
        try:
            await sub.start(); sent=await sub.send(text="cut:"+cut); delivery=sent["delivery"]
            if cut.startswith("claim-"):
                await sub.restart(fault=cut)
                await sub.scope("resume")
                await sub.wait_cut("before-claim-write" if cut=="claim-before" else "after-durable-before-receipt")
                ctx.check(not invocations(sub,delivery=delivery),cut+": native has not seen invoke")
            else:
                if cut=="native-before-result":
                    await sub.native.call("plan",ref=sub.agents[0]["backend_ref"],op="agent.send",plan={"after":"native-applied"})
                await sub.scope("resume")
                if cut=="native-before-result":
                    await sub.wait(lambda:any(row.get("delivery")==delivery for row in sub.native.records("model_request")),label="native applied before held reply")
                    ctx.check((await sub.get(delivery))["state"]=="claimed","executed cut is before result persistence")
                else: await settled(sub,delivery)
            await sub.restart(crash=True)
            restored=await sub.get(delivery)
            expected="held" if cut=="claim-before" else "unknown" if cut in ("claim-after","native-before-result") else "accepted"
            ctx.check(restored["state"]==expected,cut+": correct restored evidence state",record=restored)
            count=len(invocations(sub,delivery=delivery))
            ctx.check(count==(0 if cut.startswith("claim-") else 1),cut+": exact native invocation count before resume")
            await sub.scope("resume")
            if cut=="claim-before": await settled(sub,delivery)
            else:
                # Read-only barriers do not confer review/resend authority.
                for _ in range(4): await sub.command("status")
            final=len(invocations(sub,delivery=delivery))
            ctx.check(final==(1 if cut in ("claim-before","native-before-result","after-result") else 0),
                      cut+": no second invocation through recovery/resume")
            ctx.evidence.append({"kind":"crash_cut","cut":cut,"restored":expected,"native_invocations":final,
                                 "native":sub.native.records("invoke"),"models":sub.native.records("model_request")})
        finally:
            await sub.close();ctx.evidence.extend(sub.evidence)


@case("I3")
async def unconfirmed_claim(ctx):
    sent=await ctx.send(text="unconfirmed-claim-original");delivery=sent["delivery"]
    original=await ctx.get(delivery)
    await ctx.restart(fault="claim-unconfirmed")
    await ctx.peer.call("events.subscribe",{"types":["operational"]})
    events=[]
    def drain():
        events.extend(row["params"] for row in ctx.peer.take_notifications("event"))
        return [event for event in events if event.get("type")=="operational"]
    def uncertain():
        return next((event for event in drain() if event["data"].get("durability_unconfirmed")),None)
    await ctx.scope("resume")
    adopted=await ctx.wait(uncertain,label="actual adopted unconfirmed claim event")
    changes=[change for change in adopted["data"]["changes"] if change["key"]=="operational/coord/message/"+delivery]
    ctx.check(len(changes)==1 and changes[0]["value"]["state"]=="claimed",
              "unconfirmed event publishes actual claimed transaction, not prepared/rolled-back held",component="core",event=adopted)
    claim=changes[0]["value"];attempt=claim["attempt"]["id"]
    ctx.check(claim["content"]==original["content"] and claim["to"]==original["to"],"adopted event preserves exact intent",component="core")
    async def repaired():
        rows=drain()
        repair=next((event for event in rows if event["seq"]>adopted["seq"] and not event["data"].get("durability_unconfirmed")
            and any(change["key"]=="operational/coord/message/"+delivery and change.get("value",{}).get("state")=="held"
                    for change in event["data"]["changes"])),None)
        return repair
    repair=await ctx.wait(repaired,label="confirmed repair-to-held transaction")
    await ctx.scope("pause")
    await ctx.peer.call("workspace.get",{"key":"doc/part-b/read-barrier"});drain()
    calls=invocations(ctx,delivery=delivery)
    for call in calls:
        witnessed=((call.get("disk_witness") or {}).get("entry") or {})
        ctx.check(witnessed.get("value",{}).get("attempt",{}).get("id")!=attempt,
                  "uncertain claim itself never authorizes native invoke")
        confirmed=next((event for event in events if event["seq"]>repair["seq"] and not event["data"].get("durability_unconfirmed")
            and any(change["key"]=="operational/coord/message/"+delivery and change["version"]==witnessed.get("version")
                    and change.get("value",{}).get("state")=="claimed" for change in event["data"]["changes"])),None)
        ctx.check(confirmed is not None,"any later first delivery needs its own fresh confirmed claim",events=events)
    ctx.check(len(calls)<=1,"no duplicate native delivery after uncertain adoption/repair")
    ctx.evidence.append({"kind":"unconfirmed_claim","adopted":adopted,"repair":repair,"native_calls":calls})


@case("I12")
async def session_pinning_and_claim_race(ctx):
    pinned=await ctx.send(text="pinned S1")
    await ctx.native_set(0,session={"id":"S2"},epoch="e2")
    await ctx.scope("resume")
    await held(ctx,pinned["delivery"],"session_changed")
    ctx.check(not invocations(ctx,delivery=pinned["delivery"]),"pinned S1 intent never follows S2 implicitly")
    followed=await ctx.send(target=ctx.agents[1]["id"],text="explicitly follow S2",follow_current_session=True)
    await ctx.native_set(1,session={"id":"S2"},epoch="e2")
    await ctx.scope("resume","agent:"+ctx.agents[1]["id"]);await settled(ctx,followed["delivery"])
    calls=invocations(ctx,delivery=followed["delivery"])
    ctx.check(len(calls)==1 and calls[0]["args"]["guard"]["session"]=="S2",
              "explicit follow selects S2 and dispatches with S2 runtime guard")
    from .context import PublicCoordination
    sub=PublicCoordination(ctx.root/"r",ctx.manifest,ctx.vqro)
    try:
        await sub.start();sent=await sub.send(text="S3 race",follow_current_session=True)
        await sub.native_set(0,session={"id":"S2"},epoch="e2")
        await sub.restart(fault="claim-after")
        await sub.scope("resume");await sub.wait_cut("after-durable-before-receipt")
        path=Path(sub.backend.env["VQRO_STATE_DIR"])
        claim=json.loads((path/"operational/coord.json").read_text())["entries"]["operational/coord/message/"+sent["delivery"]]["value"]
        ctx.check(claim["attempt"]["session"]=="S2" and not invocations(sub,delivery=sent["delivery"]),
                  "durable S2 claim is blocked before acknowledgement and resource.invoke")
        await sub.native_set(0,session={"id":"S3"},epoch="e3")
        (path/"release-fault").touch()
        stale=await held(sub,sent["delivery"],"stale_guard")
        ctx.check(stale["attempt"]["session"]=="S2","S3 cannot rewrite the already pinned attempt")
        await sub.scope("resume")
        await asyncio.sleep(.25)
        ctx.check((await sub.get(sent["delivery"]))["state"]=="held" and
                  not any(row.get("delivery")==sent["delivery"] for row in sub.native.records("model_request")),
                  "stale intent stays for review, including through a current-epoch resume")
    finally:
        (Path(sub.backend.env["VQRO_STATE_DIR"])/"release-fault").touch()
        await sub.close();ctx.evidence.extend(sub.evidence)


@case("H15")
async def readonly_boot_before_writes(ctx):
    from .context import PublicCoordination
    failures=[]
    for index,mode in enumerate(("register-before","register-error")):
        sub=PublicCoordination(ctx.root/str(index),ctx.manifest,ctx.vqro)
        stage="fixture setup"
        try:
            await sub.start();await member(sub,0,"read-only-project")
            sent=await sub.send(text="held across read-only boot")
            await sub.native.call("plan",ref=sub.agents[0]["backend_ref"],op="agent.interrupt",
                plan={"result":{"outcome":"indeterminate","error":{"kind":"lost","message":"fake unknown control"}}})
            operation=await sub.operation("unknown-control")
            result=await sub.command("control",{"op":operation,"target":sub.agents[0]["id"],"action":"interrupt","args":{"v":0}})
            ctx.check(result["state"]=="unknown","read-only fixture has a real persisted unknown native control")
            baseline=await sub.peer.call("operational.snapshot",{"domain":"coord"})
            state=Path(sub.backend.env["VQRO_STATE_DIR"]);path=state/"operational/coord.json"
            disk_before=hashlib.sha256(path.read_bytes()).hexdigest()
            member_key="doc/agents/members/"+sub.agents[0]["id"]
            membership=(await sub.peer.call("workspace.snapshot"))["entries"][member_key]
            mark=sub.native.mark()
            await sub.restart(fault=mode,wait_ready=False)
            if mode=="register-before": await sub.wait_cut("before-register-write")
            wanted="initializing" if mode=="register-before" else "unavailable"
            status=await sub.wait_command("status",lambda row:row.get("state")==wanted)
            ctx.check(status["state"]==wanted,mode+": observable bootstrap state",status=status)
            actual=(await sub.peer.call("workspace.snapshot"))["entries"][member_key]
            ctx.check(actual==membership,mode+": membership is readable without bootstrap writes")
            stage="coord.inbox.get held record"
            record=await sub.command("inbox.get",{"delivery":sent["delivery"]})
            ctx.check(record["state"]=="held" and record.get("reason")=="paused",mode+": held record is publicly readable")
            stage="coord.inbox.op unknown native operation"
            unknown=await sub.command("inbox.op",{"op":operation})
            ctx.check(unknown["state"]=="unknown",mode+": unknown operation evidence is publicly readable",operation=unknown)
            after=await sub.peer.call("operational.snapshot",{"domain":"coord"})
            ctx.check(after["entries"]==baseline["entries"] and hashlib.sha256(path.read_bytes()).hexdigest()==disk_before,
                      mode+": captured reads do not mutate the domain before successful registration")
            ctx.check(not invocations(sub,after=mark),mode+": read-only startup performs no native invocation")
        except (CheckFailed,RpcError) as error:
            failures.append({"cut":mode,"stage":stage,"component":getattr(error,"component","coordination"),"detail":str(error)})
        finally:
            (Path(sub.backend.env["VQRO_STATE_DIR"])/"release-fault").touch()
            await sub.close();ctx.evidence.extend(sub.evidence)
    ctx.check(not failures,"both registration cuts must retain the required public read-only paths",failures=failures)
