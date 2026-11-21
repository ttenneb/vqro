# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Coordination policy probes over public RPC. No target module/state-machine imports."""
import asyncio
from copy import deepcopy
import hashlib
import json
import time

from ..results import CheckFailed, FixtureFailed, HarnessGap, NotApplicable
from ..wire import RpcError
from pathlib import Path

HANDLERS = {}


def case(identifier):
    def register(function):
        if identifier in HANDLERS: raise ValueError("duplicate Part B handler: " + identifier)
        HANDLERS[identifier] = function
        return function
    return register


def inbox_dispatch_origin(origin, principal):
    """C2 core transport identity stays separate from the protected inbox sender assertion."""
    if not isinstance(origin, dict) or not isinstance(principal, str) or not principal:
        return False
    sender = origin.get("sender")
    return (origin.get("principal") == "extension:coord" and origin.get("actor") == "extension:coord"
            and isinstance(sender, dict) and sender.get("principal") == principal
            and sender.get("provenance") == "inbox-record")


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


async def settled(ctx, delivery, *, timeout=8):
    return await ctx.wait_command("inbox.get", lambda row: row.get("state") not in ("held", "recorded", "claimed"),
                                  args={"delivery": delivery}, timeout=timeout)


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


@case("I7")
async def held_start_does_not_block_other_lanes(ctx):
    await ctx.native_set(0,state="running",run="existing-run")
    start=await ctx.send(mode="start",text="held start")
    others=[]
    for mode in ("boundary","admit","preempt"):
        fields={"expires_at_ms":int(time.time()*1000)+60000} if mode=="preempt" else {}
        others.append(await ctx.send(mode=mode,text=mode,send_new=True,**fields))
    await ctx.scope("resume")
    for row in others: await settled(ctx,row["delivery"])
    ctx.check(not invocations(ctx,delivery=start["delivery"]) and all(len(invocations(ctx,delivery=row["delivery"]))==1 for row in others),
              "busy start lane does not block boundary, admit or preempt")
    reply=await ctx.command("control",{"op":await ctx.operation(),"action":"interrupt","target":ctx.agents[0]["id"],"args":{"v":0}})
    ctx.check(reply["state"] in ("accepted","observed","dispatched") and invocations(ctx,op="agent.interrupt"),"control is admitted without waiting for held start")


@case("I8")
async def busy_retry_has_fresh_attempts_and_a_bound(ctx):
    expiry=int(time.time()*1000)+60000;guard={"session":"s1"}
    await ctx.native.call("plan",ref=ctx.agents[0]["backend_ref"],op="agent.send",plan={"result":{"outcome":"rejected","error":{"kind":"busy","message":"native busy at its serialized preflight"}}})
    sent=await ctx.send(guard=guard,expires_at_ms=expiry);await ctx.scope("resume")
    for count in (1,2,3):
        await ctx.wait(lambda:len(invocations(ctx,delivery=sent["delivery"]))>=count,label="bounded busy attempt")
        if count<3:
            observed=await ctx.wait_command("inbox.get",lambda row:row.get("state") in ("held","unknown","rejected"),args={"delivery":sent["delivery"]})
            ctx.check(observed["state"]=="held" and observed.get("reason")=="agent_busy","definite native busy returns held, not unknown",record=observed)
            await asyncio.sleep(.15)
            ctx.check(len(invocations(ctx,delivery=sent["delivery"]))==count,"busy retry waits for a later idle generation, never polls hot")
            await native_idle(ctx)
    record=await ctx.wait_command("inbox.get",lambda row:row.get("state")=="rejected",args={"delivery":sent["delivery"]})
    calls=invocations(ctx,delivery=sent["delivery"])
    ids=[call["witness"]["entry"]["value"]["attempt"]["id"] for call in calls]
    witnesses=[call["witness"]["entry"]["value"] for call in calls]
    ctx.check(len(ids)==len(set(ids))==3 and all(row["guard"]==guard and row["expires_at_ms"]==expiry and row["to"]==witnesses[0]["to"] for row in witnesses),
              "three fresh durable attempts preserve exact target guard and original expiry",record=record)
    await ctx.native.call("plan",ref=ctx.agents[1]["backend_ref"],op="agent.send",plan={"before":"stale-session"})
    stale=await ctx.send(target=ctx.agents[1]["id"],guard={"session":"s1"})
    await ctx.scope("resume","agent:"+ctx.agents[1]["id"])
    await ctx.wait(lambda:invocations(ctx,delivery=stale["delivery"]),label="stale runtime guard race")
    await ctx.native_set(1,session={"id":"s2"},epoch="e2")
    await ctx.native.call("release",barrier="stale-session")
    await held(ctx,stale["delivery"],"stale_guard")
    await ctx.scope("resume","agent:"+ctx.agents[1]["id"])
    ctx.check(not any(row.get("delivery")==stale["delivery"] for row in ctx.native.records("model_request")),"stale session is held for review, never unguarded input")


@case("G4")
async def resolved_strategy_right_is_rechecked_at_claim(ctx):
    principal="strategy-sender";worker=await ctx.connect(principal);scope="agent:"+ctx.agents[0]["id"]
    await ctx.grant("requested-mode",principal,["send:preempt"],scope)
    await ctx.grant("resolved-strategy",principal,["send:interrupt_then_start"],scope)
    await ctx.grant("strategy-steps",principal,["queue","interrupt","send:start"],scope)
    sent=await ctx.send(peer=worker,mode="preempt",strategy="interrupt_then_start",expires_at_ms=int(time.time()*1000)+60000)
    async def replace(identifier,value):
        entry=(await ctx.peer.call("workspace.snapshot"))["entries"]["doc/agents/grants/"+identifier]
        await ctx.command("grants",{"op":await ctx.operation(),"id":identifier,"expect":entry["version"],"value":value})
    await replace("resolved-strategy",{"who":"other-sender","on":scope,"can":["send:interrupt_then_start"]})
    await ctx.scope("resume");await held(ctx,sent["delivery"],"no_grant")
    ctx.check(not invocations(ctx,delivery=sent["delivery"]),"requested mode grant cannot replace revoked resolved-strategy grant at claim")
    await ctx.scope("pause")
    await replace("resolved-strategy",{"who":principal,"on":scope,"can":["send:interrupt_then_start"]})
    await replace("strategy-steps",{"who":principal,"on":scope,"can":["queue","send:start"]})
    await ctx.scope("resume");await held(ctx,sent["delivery"],"no_grant")
    ctx.check(not invocations(ctx,delivery=sent["delivery"]),"each composed step is separately reauthorized before native invocation")


@case("I20")
async def quotes_never_impersonate_the_sender(ctx):
    principal="agent-A";sender=await ctx.connect(principal)
    await ctx.native.call("add-many",count=3);await ctx.wait_agents(3)
    await ctx.native_set(0,principal=principal)
    await ctx.grant("A-only-start",principal,["send:start"])
    quote={"principal":"owner","kind":"human"}
    sent=await ctx.send(target=ctx.agents[1]["id"],peer=sender,quoted=quote,render=["attributed_user"],text="A's quoted instruction")
    record=await ctx.get(sent["delivery"])
    await ctx.scope("resume","agent:"+ctx.agents[1]["id"]);await settled(ctx,sent["delivery"])
    model=next(row for row in ctx.native.records("model_request") if row.get("delivery")==sent["delivery"])
    ctx.check(record["origin"]["principal"]==principal and record["origin"]["kind"]=="agent" and
              model["role"]=="user" and inbox_dispatch_origin(model["sender"], principal) and model["quoted_claim"]==quote,
              "inbox sender assertion remains A; core actor stays coord and owner quote remains a claim",model=model)
    caps=deepcopy(ctx.agents[2]["data"]["agent"]["caps"]);caps["rev"]+=1;caps["agent.send"]["origin"]=[]
    await ctx.native_set(2,caps=caps)
    unsupported=await ctx.send(target=ctx.agents[2]["id"],peer=sender,quoted=quote,render=["attributed_user"])
    await ctx.scope("resume","agent:"+ctx.agents[2]["id"])
    await held(ctx,unsupported["delivery"],"origin_unsupported")
    ctx.check(not invocations(ctx,delivery=unsupported["delivery"]),"no declared accepted rendering means hold, never strip sender attribution")


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
    from .launcher import cli_write,tui,undo
    launched=await cli_write(ctx,"alice-cli","worker:alice",[{"op":"set","key":"doc/part-b/launched-alice","value":True}],label="owner")
    ctx.check(launched.get("principal")=="worker:alice" and launched.get("label")=="owner",
              "actual PTY CLI uses its configured launcher identity, never its declared label",component="core",transaction=launched)
    await cli_write(ctx,"owner-cli","owner",[{"op":"set","key":"doc/part-b/default-tui-owner","value":True}])
    async with tui(ctx,"default-owner-tui") as ui:
        await undo(ctx,ui,"doc/part-b/default-tui-owner")
    entries=(await ctx.peer.call("workspace.snapshot"))["entries"]
    ctx.check("doc/part-b/launched-alice" in entries,"default TUI owner undo leaves the launched agent principal alone",component="core")


@case("G2")
async def per_principal_undo(ctx):
    alice, bob = await ctx.connect("worker:alice"), await ctx.connect("worker:bob")
    from .launcher import cli_write,tui,undo
    a=await cli_write(ctx,"alice-pane-cli","worker:alice",[{"op":"set","key":"doc/part-b/alice","value":"A","expect":0}])
    b=await cli_write(ctx,"bob-pane-cli","worker:bob",[{"op":"set","key":"doc/part-b/bob","value":"B","expect":0}])
    ctx.check(a.get("principal")=="worker:alice" and b.get("principal")=="worker:bob","real pane CLI writes retain distinct configured principals",component="core")
    async with tui(ctx,"alice-tui","worker:alice") as ui:
        await undo(ctx,ui,"doc/part-b/alice")
    snapshot = await ctx.peer.call("workspace.snapshot")
    ctx.check("doc/part-b/alice" not in snapshot["entries"], "Alice undo removes Alice's change")
    ctx.check(snapshot["entries"]["doc/part-b/bob"]["value"] == "B", "Alice undo leaves Bob's later change")
    await alice.call("workspace.revert", {"txn": b["id"]})
    snapshot = await ctx.peer.call("workspace.snapshot")
    ctx.check("doc/part-b/bob" not in snapshot["entries"], "Bob's transaction remains explicitly revertable by id")
    ctx.evidence.append({"alice_txn": a, "bob_txn": b})



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
    ctx.check(len(calls)==1 and inbox_dispatch_origin(calls[0]["args"]["origin"], principal),
              "same inbox sender uses its retained grant via the core-stamped coord actor, without owner rebinding")


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


async def terminal(ctx,*,pull=None,marker=True):
    ctx.serial+=1;ref="terminal"+str(ctx.serial)
    await ctx.native.call("add",ref=ref,fields={"kind":"terminal","app":"fake-osc-agent" if marker else None})
    async def found(): return next((row for row in await ctx.resources() if row["backend_ref"]==ref),None)
    row=await ctx.wait(found,label="public terminal fixture")
    if pull is not None:
        await ordinary(ctx,[{"op":"set","key":"doc/agents/terminals/"+row["id"],"value":{"pull":pull},"expect":0}])
    return row


def user_pull():
    points = ("turn", "settle", "idle", "tool")
    return {"points": {point: {"precision": "exact"} for point in points},
            "framing": {point: {"role": "user"} for point in points},
            "insert": "user", "evidence": "entry", "tool_boundary": True}


async def consumer(ctx,row,name="consumer-1",records=None):
    return await ctx.command("inbox.reconcile",{"op":await ctx.operation(),"agent":row["id"],"consumer":name,"records":records or []})


async def take(ctx,row,*,name="consumer-1",at="turn",**fields):
    return await ctx.command("inbox.take",{"op":await ctx.operation(),"agent":row["id"],"consumer":name,"at":at,"max_bytes":65536,**fields})


async def report_pull(ctx,row,taken,stage,*,name="consumer-1",**fields):
    results=[]
    for claim in taken["claims"]:
        results.append(await ctx.command("inbox.report",{"op":await ctx.operation(),"consumer":name,"stage":stage,**claim,**fields}))
    return results


async def insert_pull(ctx,row,taken,*,name="consumer-1"):
    entries=await ctx.native.call("entry",ref=row["backend_ref"],records=taken["records"])
    await report_pull(ctx,row,taken,"inserted",name=name)
    return entries["records"]


@case("T1")
async def terminal_marker_without_plugin_never_types(ctx):
    plain=await terminal(ctx,marker=False)
    selection=await ctx.command("scope",{"action":"inspect","scope":"workspace"})
    ctx.check(plain["id"] not in selection["targets"],"unmarked raw terminal is not inferred to be an agent")
    marked=await terminal(ctx)
    await ordinary(ctx,[{"op":"set","key":"doc/agents/terminals/"+plain["id"],"value":{},"expect":0}])
    selection=await ctx.command("scope",{"action":"inspect","scope":"workspace"})
    ctx.check({plain["id"],marked["id"]}<=set(selection["targets"]),"backend OSC-shaped app status and explicit marker both map terminal targets")
    sent=await ctx.send(target=marked["id"])
    await ctx.scope("resume","agent:"+marked["id"])
    await held(ctx,sent["delivery"],"integration_unavailable")
    ctx.check(not ctx.native.records("terminal_input") and not invocations(ctx,delivery=sent["delivery"]),
              "without plugin input remains held, not keyboard-typed or adapted to send")


@case("T2")
async def pull_points_single_flight_and_order(ctx):
    row=await terminal(ctx,pull=user_pull());await consumer(ctx,row)
    deliveries={}
    for mode in ("boundary","settle","start","preempt"):
        fields={"expires_at_ms":int(time.time()*1000)+60000} if mode=="preempt" else {}
        deliveries[mode]=(await ctx.send(target=row["id"],mode=mode,text=mode,send_new=True,**fields))["delivery"]
    await ctx.scope("resume","agent:"+row["id"])
    first=await take(ctx,row,at="turn")
    ctx.check([r["delivery"] for r in first["records"]]==[deliveries["boundary"]],"turn safe point excludes start, settle and preempt")
    concurrent=await take(ctx,row,at="idle")
    ctx.check(not concurrent.get("records"),"one outstanding take batch fences every other point on that target",reply=concurrent)
    await insert_pull(ctx,row,first);await ctx.native.call("model",ref=row["backend_ref"])
    midrun=await take(ctx,row,at="tool")
    ctx.check(not midrun.get("records") and (await ctx.get(deliveries["settle"]))["state"]=="held","mid-run tool point leaves settle pending")
    for point in ("turn","settle","idle","tool"):
        batch=await take(ctx,row,at=point)
        ctx.check(deliveries["preempt"] not in [r["delivery"] for r in batch.get("records",[])],"preempt is never pulled at "+point)
        if batch.get("records"): await insert_pull(ctx,row,batch)
    # Separate dense batch: more records than the take page, retaining full typed content.
    other=await terminal(ctx,pull=user_pull());await consumer(ctx,other,"dense-consumer")
    ids={};senders=[]
    for index in range(3):
        principal="dense-sender-"+str(index)
        await ctx.grant("dense-grant-"+str(index),principal,["send:boundary"],"agent:"+other["id"])
        senders.append((principal,await ctx.connect(principal)));ids[principal]=[]
    for index in range(17):
        principal,peer=senders[index%3]
        delivery=(await ctx.send(target=other["id"],peer=peer,mode="boundary",text=str(index)+":"+"x"*17000,send_new=True))["delivery"]
        ids[principal].append(delivery)
    await ctx.scope("resume","agent:"+other["id"])
    dense=await take(ctx,other,name="dense-consumer")
    ctx.check(0<len(dense["records"])<17 and len(json.dumps(dense["records"]).encode())<=250*1024,"oversized queued batch returns only records fitting page bounds")
    chosen=[r["delivery"] for r in dense["records"]]
    for principal,expected in ids.items():
        selected=[r["delivery"] for r in dense["records"] if r["from"]["principal"]==principal]
        ctx.check(selected==expected[:len(selected)],"same sender records remain FIFO within the selected round",principal=principal)
    await insert_pull(ctx,other,dense,name="dense-consumer")
    before=ctx.native.mark();await ctx.native.call("model",ref=other["backend_ref"])
    model=ctx.native.records("model_request",after=before)
    ctx.check(len(model)==1 and model[0]["deliveries"]==chosen,"one next native model request contains the entire take exactly once in order")


@case("T3")
async def aborted_insert_and_start_markers_are_not_read(ctx):
    row=await terminal(ctx,pull=user_pull());await consumer(ctx,row)
    sent=await ctx.send(target=row["id"],mode="boundary");await ctx.scope("resume","agent:"+row["id"])
    batch=await take(ctx,row);await insert_pull(ctx,row,batch)
    await ctx.native.call("abort",ref=row["backend_ref"])
    ctx.check((await ctx.get(sent["delivery"]))["state"]=="inserted" and not ctx.native.records("model_request"),"abort immediately after insertion leaves inserted/unread input")
    await ctx.native.call("context",ref=row["backend_ref"],records=batch["records"])
    await ctx.native.call("message_start",ref=row["backend_ref"])
    ctx.check((await ctx.get(sent["delivery"]))["state"]=="inserted","context hook and message_start facts cannot manufacture read evidence")


@case("T8")
async def pending_edit_and_editor_draft_are_independent(ctx):
    import base64
    row=await terminal(ctx,pull=user_pull());await consumer(ctx,row)
    stream=await ctx.peer.call("resource.attach",{"resource":row["id"],"cols":80,"rows":24,"history_lines":0})
    await ctx.peer.call("stream.input",{"stream":stream["stream"],"data":base64.b64encode(b"manual-probe").decode()})
    await ctx.wait(lambda:ctx.native.records("terminal_input"),label="actual raw terminal input spy")
    draft=b"human draft: \xce\xbb\x00keep it unchanged"
    await ctx.native.call("draft",ref=row["backend_ref"],data=base64.b64encode(draft).decode())
    mark=ctx.native.mark();sent=await ctx.send(target=row["id"],mode="boundary",text="before edit")
    old=await ctx.get(sent["delivery"])
    content=[{"type":"text","text":"edited before take"}]
    await ctx.command("inbox.edit",{"op":await ctx.operation(),"delivery":sent["delivery"],"expect":old["revision"],"content":content})
    await ctx.scope("resume","agent:"+row["id"]);batch=await take(ctx,row)
    ctx.check(batch["records"][0]["content"]==content,"latest pre-claim edit is exactly the pulled content")
    current=await ctx.get(sent["delivery"])
    await ctx.rejected("inbox.edit",{"op":await ctx.operation(),"delivery":sent["delivery"],"expect":current["revision"],"content":[{"type":"text","text":"too late"}]},{"too_late"})
    await insert_pull(ctx,row,batch);await ctx.native.call("model",ref=row["backend_ref"])
    after=await ctx.native.call("draft",ref=row["backend_ref"])
    ctx.check(base64.b64decode(after["data"])==draft and not ctx.native.records("terminal_input",after=mark),"pull insertion leaves the actual fake editor byte-identical; input spy is proven live")
    ctx.check(ctx.native.records("model_request")[-1]["entries"][0]["content"]==content,"model receives the pre-take edit, never the late payload")


@case("T11")
async def pull_filters_content_without_claiming_or_flattening(ctx):
    import base64,struct,zlib
    def chunk(kind,data): return struct.pack(">I",len(data))+kind+data+struct.pack(">I",zlib.crc32(kind+data)&0xffffffff)
    png=b"\x89PNG\r\n\x1a\n"+chunk(b"IHDR",struct.pack(">IIBBBBB",1,1,8,2,0,0,0))+chunk(b"IDAT",zlib.compress(b"\x00\x00\x00\x00"))+chunk(b"IEND",b"")
    row=await terminal(ctx,pull=user_pull());await consumer(ctx,row)
    rich=[{"type":"image","mime":"image/png","data":base64.b64encode(png).decode()}]
    image=await ctx.send(target=row["id"],mode="boundary",content=rich)
    text=await ctx.send(target=row["id"],mode="boundary",text="eligible text",send_new=True)
    await ctx.scope("resume","agent:"+row["id"])
    batch=await take(ctx,row,accept=["text"])
    ctx.check([r["delivery"] for r in batch["records"]]==[text["delivery"]],"text-only consumer leaves rich input unclaimed and still progresses text")
    await insert_pull(ctx,row,batch)
    for _ in range(3):
        repeated=await take(ctx,row,accept=["text"])
        record=await ctx.get(image["delivery"])
        ctx.check(not repeated["records"] and repeated.get("held")=="unsupported_content" and record["state"]=="held" and not record.get("attempt"),"unsupported image stays held with zero claims/releases at repeated points",record=record,reply=repeated)
    capable=await take(ctx,row,accept=["text","image"])
    ctx.check(capable["records"][0]["content"]==rich,"image-capable consumer gets original typed blocks without flattening")


@case("T14")
async def consumer_record_budget_is_not_a_claim_loop(ctx):
    row=await terminal(ctx,pull=user_pull());await consumer(ctx,row)
    large=await ctx.send(target=row["id"],mode="boundary",text="L"*2000)
    small=await ctx.send(target=row["id"],mode="boundary",text="small",send_new=True)
    await ctx.scope("resume","agent:"+row["id"])
    batch=await take(ctx,row,max_bytes=512)
    ctx.check([r["delivery"] for r in batch["records"]]==[small["delivery"]],"consumer per-record budget skips large input but serves smaller eligible mail",reply=batch,max_bytes=512)
    await insert_pull(ctx,row,batch)
    for _ in range(3):
        repeated=await take(ctx,row,max_bytes=512);record=await ctx.get(large["delivery"])
        ctx.check(not repeated["records"] and repeated.get("held")=="too_large_for_consumer" and record["state"]=="held" and not record.get("attempt"),"too-large input stays held without claim/release or truncation")
    sufficient=await take(ctx,row,max_bytes=5000)
    ctx.check(sufficient["records"][0]["content"]==[{"type":"text","text":"L"*2000}],"sufficient budget admits original full content exactly once")


@case("T4")
async def released_or_crashed_before_entry(ctx):
    row=await terminal(ctx,pull=user_pull());await consumer(ctx,row)
    sent=await ctx.send(target=row["id"],mode="boundary")
    await ctx.scope("resume","agent:"+row["id"])
    first=await take(ctx,row);ctx.check(len(first["records"])==1,"safe-point take claims pending input")
    ctx.check(not (await ctx.native.call("session",ref=row["backend_ref"]))["entries"],"release fixture handler returns with no inserted entry")
    await report_pull(ctx,row,first,"released")
    ctx.check((await ctx.get(sent["delivery"]))["state"]=="held","release returns uninserted input to held")
    second=await take(ctx,row);await insert_pull(ctx,row,second)
    await ctx.rejected("inbox.report",{"op":await ctx.operation(),"consumer":"consumer-1","stage":"released",**second["claims"][0]},{"conflict"})
    fresh=await ctx.send(target=row["id"],mode="boundary",text="crash before insertion",send_new=True)
    third=await take(ctx,row);ctx.check(third["claims"][0]["delivery"]==fresh["delivery"],"fresh claim precedes consumer loss")
    await ctx.restart()
    ctx.check((await ctx.get(fresh["delivery"]))["state"]=="unknown","consumer/core crash before insertion preserves unknown")


@case("T5")
async def reconcile_and_history_never_reinsert(ctx):
    row=await terminal(ctx,pull=user_pull());await consumer(ctx,row)
    inserted=await ctx.send(target=row["id"],mode="boundary",text="entry witness")
    absent=await ctx.send(target=row["id"],mode="boundary",text="no entry witness",send_new=True)
    await ctx.scope("resume","agent:"+row["id"])
    op=await ctx.operation();args={"op":op,"agent":row["id"],"consumer":"consumer-1","at":"turn","max_bytes":65536}
    batch=await ctx.command("inbox.take",args);ctx.check(len(batch["claims"])==2,"two open claims acquired")
    entries=await ctx.native.call("entry",ref=row["backend_ref"],records=[next(r for r in batch["records"] if r["delivery"]==inserted["delivery"])])
    await ctx.restart()
    await consumer(ctx,row,"consumer-2",entries["records"])
    ctx.check((await ctx.get(inserted["delivery"]))["state"]=="inserted" and (await ctx.get(absent["delivery"]))["state"]=="unknown",
              "reconcile uses actual delivery-bearing entry, otherwise retains unknown")
    for claim in batch["claims"]:
        await ctx.rejected("inbox.report",{"op":await ctx.operation(),"consumer":"consumer-1","stage":"inserted",**claim},{"conflict"})
    replay=await ctx.command("inbox.take",args)
    ctx.check(not replay.get("records"),"repeated take returns historical claims, never insertion content",replay=replay)
    ctx.check(not ctx.native.records("model_request"),"reconcile and replay grant no native model request")


@case("T6")
async def delayed_old_report_cannot_change_new_claim(ctx):
    row=await terminal(ctx,pull=user_pull());await consumer(ctx,row)
    sent=await ctx.send(target=row["id"],mode="boundary");await ctx.scope("resume","agent:"+row["id"])
    first=await take(ctx,row);await report_pull(ctx,row,first,"released")
    second=await take(ctx,row)
    ctx.check(first["claims"][0]["claim"]!=second["claims"][0]["claim"],"retake creates a new exact claim")
    await ctx.rejected("inbox.report",{"op":await ctx.operation(),"consumer":"consumer-1","stage":"inserted",**first["claims"][0]},{"conflict"})
    ctx.check((await ctx.get(sent["delivery"]))["attempt"]["claim"]==second["claims"][0]["claim"],"late old report leaves new claim untouched")


@case("T10")
async def context_evidence_never_claims_entry(ctx):
    points = ("turn", "settle", "idle")
    declaration={"points": {point: {"precision": "exact"} for point in points},
                 "framing": {point: {"role": "context"} for point in points},
                 "insert":"context","evidence":"none"}
    row=await terminal(ctx,pull=declaration);await consumer(ctx,row)
    sent=await ctx.send(target=row["id"],mode="boundary",render=["context"],text="context with actual sender")
    await ctx.scope("resume","agent:"+row["id"]);batch=await take(ctx,row)
    ctx.check(len(batch["records"])==1,"declared context path takes bounded content")
    await ctx.native.call("context",ref=row["backend_ref"],records=batch["records"])
    await ctx.rejected("inbox.report",{"op":await ctx.operation(),"consumer":"consumer-1","stage":"inserted",**batch["claims"][0]},{"invalid"})
    await report_pull(ctx,row,batch,"supplied")
    ctx.check((await ctx.get(sent["delivery"]))["state"]=="supplied","context/no-entry reports supplied, never inserted")
    await ctx.native.call("model",ref=row["backend_ref"])
    model=ctx.native.records("model_request")[-1]
    ctx.check(not model["entries"] and model["context"][0]["origin"]["principal"]=="owner",
              "fake next model envelope presents context with origin, not a user entry")


@case("G11")
async def shared_host_native_tool_provenance(ctx):
    for row in await ctx.resources():
        value=(row.get("data") or {}).get("agent",{}).get("caps",{}).get("agent",{}).get("tool_principal")
        path="resources."+row["id"]+".data.agent.caps.agent.tool_principal"
        if value!="env":
            ctx.evidence.append({"subcase":"endpoint-env-tool","status":"not-applicable","declaration":{"path":path,"actual":value}})
        else:
            raise HarnessGap("declared tool_principal=env requires actual runtime-child tool-to-CLI fixture; asserted hello is not evidence")
    from .host_tool_binding import shared_host_route
    await shared_host_route(ctx)


@case("T12")
async def claude_style_hook_paths(ctx):
    entries=(await ctx.peer.call("workspace.snapshot"))["entries"]
    declarations={}
    for row in await ctx.resources():
        key="doc/agents/terminals/"+row["id"]
        explicit=entries.get(key,{}).get("value",{}).get("pull")
        native=(row.get("data") or {}).get("agent",{}).get("pull")
        declarations[key+".pull"]=explicit if explicit is not None else native
    selected=[value for value in declarations.values() if isinstance(value,dict) and
              value.get("insert")=="context" and value.get("evidence")=="none"]
    if not selected:
        raise NotApplicable("doc/agents/terminals/*/pull (insert=context, evidence=none)",declarations)
    raise HarnessGap("declared Claude-style hook paths require native hook/transcript fixture; raw assertions cannot qualify framing")


@case("G13")
async def handle_grants_pin_both_ends(ctx):
    original_principal,new_principal="worker:original-reviewer","worker:new-reviewer"
    await ctx.native_set(1,principal=original_principal)
    await ctx.native.call("add",ref="a2")
    await ctx.native.call("add",ref="a3",fields={"principal":new_principal})
    await ctx.wait_agents(4)
    async def bind(handle,index,**fields):
        key="doc/agents/names/"+handle
        entries=(await ctx.peer.call("workspace.snapshot"))["entries"]
        return await ctx.command("names",{"op":await ctx.operation(),"handle":handle,"agent":ctx.agents[index]["id"],
            "expect":entries.get(key,{}).get("version",0),**fields})
    await bind("@worker",0);await bind("@reviewer",1)
    await ctx.grant("pinned-handles","@reviewer",["send:start"],"@worker")
    grant=(await ctx.peer.call("workspace.snapshot"))["entries"]["doc/agents/grants/pinned-handles"]["value"]
    ctx.check(grant["who"]==original_principal and grant["on"]=="agent:"+ctx.agents[0]["id"],
              "public grant issuance stores exact principal and target")
    await bind("@worker",2);await bind("@reviewer",3)
    original=await ctx.connect(original_principal);new=await ctx.connect(new_principal)
    sent=await ctx.send(peer=original,to=ctx.agents[0]["id"])
    for peer,target in ((original,"@worker"),(new,ctx.agents[0]["id"]),(new,"@worker")):
        await ctx.rejected("inbox.send",{"op":await ctx.operation(),"submission":await ctx.operation(),"to":target,
            "mode":"start","content":[{"type":"text","text":"no transferred grant"}],"send_new":True},{"no_grant"},peer=peer)
    await bind("@missing-principal",2)
    await bind("@conflicting-principals",3,endpoint_principal="worker:another-binding")
    for handle in ("@missing-principal","@conflicting-principals"):
        await ctx.rejected("grants",{"op":await ctx.operation(),"id":"bad-"+handle[1:],"expect":0,
            "value":{"who":handle,"on":"agent:"+ctx.agents[0]["id"],"can":["send:start"]}},{"invalid"})
    await ctx.scope("resume");await settled(ctx,sent["delivery"])
    calls=invocations(ctx,delivery=sent["delivery"])
    ctx.check(len(calls)==1 and inbox_dispatch_origin(calls[0]["args"]["origin"], original_principal),
              "only original bound inbox sender reaches the exact agent via the core-stamped coord actor")
    from .context import PublicCoordination
    sub=PublicCoordination(ctx.root/"g",ctx.manifest,ctx.vqro)
    try:
        await sub.start();await additive_dynamic_grants(sub)
        ctx.assertions+=sub.assertions
    finally:
        await sub.close();ctx.evidence.extend(sub.evidence)


async def policy_tombstone(ctx,agent):
    snapshot=await ctx.peer.call("operational.snapshot",{"domain":"coord"})
    for entry in snapshot["entries"].values():
        value=entry.get("value")
        if isinstance(value,dict) and isinstance(value.get(agent),dict) and value[agent].get("agent")==agent:
            return value[agent]
    return None


@case("L5")
async def stop_closes_admission_and_confirms_exact_host(ctx):
    caps=deepcopy(ctx.agents[0]["data"]["agent"]["caps"]);caps.pop("agent.stop");caps["rev"]+=1
    await ctx.native_set(0,caps=caps);await ctx.scope("resume")
    stopped=await ctx.command("control",{"op":await ctx.operation(),"action":"stop","target":ctx.agents[0]["id"],"args":{"v":0}})
    ctx.check(stopped["state"]=="partial" and stopped.get("native") is None,"undeclared stop closes admission but never claims native termination",reply=stopped)
    sent=await ctx.send();await held(ctx,sent["delivery"],"paused")
    process=await ctx.command("control",{"action":"inspect","target":ctx.agents[0]["id"]})
    ctx.check(process.get("confirm_host") is None and not process["graceful"],"process host without core termination route offers no guessed kill")
    await ctx.rejected("control",{"op":await ctx.operation(),"action":"stop","target":ctx.agents[0]["id"],"args":{"v":0,"how":"host_kill"}},{"unsupported"})
    host=await terminal(ctx,marker=False)
    descriptor={"id":"owned-fake-host","kind":"terminal","resource":host["id"],"incarnation":host["incarnation"],"owns_lifecycle":True}
    await ctx.native.call("bind-host",ref=host["backend_ref"],members=[row["backend_ref"] for row in ctx.agents[:2]])
    await ctx.native_set(0,host=descriptor);await ctx.native_set(1,host=descriptor)
    await ctx.scope("resume","agent:"+ctx.agents[1]["id"])
    preview=await ctx.command("control",{"action":"inspect","target":ctx.agents[0]["id"]})
    confirmation=preview["confirm_host"]
    ctx.check(set(confirmation["endpoints"])=={row["id"] for row in ctx.agents[:2]} and bool(preview.get("warning")),
              "host kill preview includes every observed served endpoint and unreported-work warning",preview=preview)
    wrong=dict(confirmation,incarnation="changed-host")
    await ctx.rejected("control",{"op":await ctx.operation(),"action":"stop","target":ctx.agents[0]["id"],"args":{"v":0,"how":"host_kill"},"confirm_host":wrong},{"conflict"})
    ctx.check(not ctx.native.records("host_terminated"),"unconfirmed host identity never invokes termination")
    result=await ctx.command("control",{"op":await ctx.operation(),"action":"stop","target":ctx.agents[0]["id"],"args":{"v":0,"how":"host_kill"},"confirm_host":confirmation})
    calls=invocations(ctx,op="terminal.kill")
    ctx.check(len(calls)==1 and len(ctx.native.records("host_terminated"))==1,"only explicit exact confirmation invokes declared termination once",reply=result)
    gates=calls[0]["control_witness"]["entry"]["value"]["gates"]
    ctx.check(all(gates.get(agent) is False for agent in confirmation["endpoints"]),"real protected store has closed all served gates before native host termination")
    ctx.check(not invocations(ctx,delivery=sent["delivery"]),"closed pending input is not rerouted or physically resent")


@case("L9")
async def retirement_is_not_native_end(ctx):
    sent=await ctx.send();await ctx.native_set(0,lifecycle="disconnected")
    retired=await ctx.command("control",{"op":await ctx.operation(),"action":"retire","target":ctx.agents[0]["id"],"args":{"reason":"routing retirement, not kill"}})
    ctx.check(retired["state"]=="retired" and retired["execution"]=="unknown" and retired.get("native") is None,
              "administrative retirement reports unknown native execution, never ended",reply=retired)
    await ctx.scope("resume")
    await held(ctx,sent["delivery"],"agent_retired")
    ctx.check(not invocations(ctx,delivery=sent["delivery"]),"routing remains stopped after explicit resume of retired endpoint")
    await ctx.native_set(0,lifecycle="ended")
    async def ended():
        stone=await policy_tombstone(ctx,ctx.agents[0]["id"])
        return stone if stone and stone.get("lifecycle")=="ended" else None
    stone=await ctx.wait(ended,label="native end evidence after retirement")
    ctx.check(stone.get("last_evidence")=="native_ended","only later native evidence makes retired endpoint ended",tombstone=stone)


@case("L6")
async def ended_targets_preserve_pending_and_unknown(ctx):
    pending=await ctx.send(text="pending at native end")
    uncertain=await ctx.send(target=ctx.agents[1]["id"],text="claimed without reply")
    await ctx.native.call("plan",ref=ctx.agents[1]["backend_ref"],op="agent.send",plan={"after":"end-without-result"})
    await ctx.scope("resume","agent:"+ctx.agents[1]["id"])
    await ctx.wait(lambda:invocations(ctx,delivery=uncertain["delivery"]),label="native applied before held result")
    ctx.check((await ctx.get(uncertain["delivery"]))["state"]=="claimed","end fixture has an outstanding claim without outcome")
    await ctx.native_set(0,lifecycle="ended");await ctx.native_set(1,lifecycle="ended")
    unknown=await ctx.wait_command("inbox.get",lambda row:row.get("state")=="unknown",args={"delivery":uncertain["delivery"]})
    ctx.check(unknown["to"]["agent"]==ctx.agents[1]["id"],"end never reroutes an uncertain claim")
    await ctx.rejected("inbox.send",{"op":await ctx.operation(),"submission":await ctx.operation(),"to":ctx.agents[0]["id"],
        "mode":"start","content":[{"type":"text","text":"must reject ended handle"}],"send_new":True},{"agent_ended","ended","too_late","not_found"})
    record=await ctx.get(pending["delivery"])
    ctx.check(record["state"]=="held" and record.get("reason")=="agent_ended","pending remains held with explicit ended reason",record=record)
    ctx.check(not invocations(ctx,delivery=pending["delivery"]) and len(invocations(ctx,delivery=uncertain["delivery"]))==1,
              "neither held nor uncertain input is resent at native end")


@case("H2")
async def durable_claim_stays_unknown_on_resume(ctx):
    sent=await ctx.send();await ctx.restart(fault="claim-after")
    await ctx.scope("resume");await ctx.wait_cut("after-durable-before-receipt")
    await ctx.restart()
    for _ in range(3):
        await ctx.scope("resume")
        ctx.check((await ctx.get(sent["delivery"]))["state"]=="unknown","restored durable claim stays unknown through each fresh resume")
    ctx.check(not invocations(ctx,delivery=sent["delivery"]),"no resume dispatches an uncertain old claim")


@case("H3")
async def native_queue_is_not_physically_suspended(ctx):
    await ctx.native_set(0,state="running",run="preexisting-native-work")
    sent=await ctx.send(mode="boundary");await ctx.scope("resume");await settled(ctx,sent["delivery"])
    ctx.check(not any(row.get("delivery")==sent["delivery"] for row in ctx.native.records("model_request")),"native boundary input is initially held by the runtime")
    pause=await ctx.scope("pause")
    ctx.check(pause.get("execution")=="not_asserted","pause reports admission, never physical suspension",reply=pause)
    before=len(invocations(ctx,delivery=sent["delivery"]))
    await ctx.native.call("point",ref=ctx.agents[0]["backend_ref"],point="tool")
    models=[row for row in ctx.native.records("model_request") if row.get("delivery")==sent["delivery"]]
    ctx.check(len(models)==1 and len(invocations(ctx,delivery=sent["delivery"]))==before==1,
              "native consumes already-held input while paused without a second coordination delivery")


@case("H13")
async def handle_rebind_moves_no_tree_gate_or_unknown(ctx):
    await member(ctx,0,"handle-project");await member(ctx,1,"handle-project",ctx.agents[0]["id"])
    await ctx.command("names",{"op":await ctx.operation(),"handle":"@lead","agent":ctx.agents[0]["id"],"expect":0})
    before=await ctx.command("scope",{"action":"inspect","scope":"team:"+ctx.agents[0]["id"]})
    await ctx.native.call("plan",ref=ctx.agents[0]["backend_ref"],op="agent.send",plan={"result":{"outcome":"indeterminate","error":{"kind":"lost","message":"fake uncertain effect"}}})
    unknown=await ctx.send();await ctx.scope("resume")
    await ctx.wait_command("inbox.get",lambda row:row.get("state")=="unknown",args={"delivery":unknown["delivery"]})
    entry=(await ctx.peer.call("workspace.snapshot"))["entries"]["doc/agents/names/@lead"]
    await ctx.command("names",{"op":await ctx.operation(),"handle":"@lead","agent":ctx.agents[1]["id"],"expect":entry["version"]})
    after=await ctx.command("scope",{"action":"inspect","scope":"team:"+ctx.agents[0]["id"]})
    ctx.check(after["targets"]==before["targets"],"name rebind changes no recorded membership tree")
    old=await ctx.get(unknown["delivery"])
    ctx.check(old["state"]=="unknown" and old["to"]["agent"]==ctx.agents[0]["id"],"unknown claim remains pinned to the exact predecessor")
    await ctx.native.call("plan",ref=ctx.agents[0]["backend_ref"],op="agent.send",plan={})
    original=await ctx.send(target=ctx.agents[0]["id"],mode="boundary",send_new=True);await settled(ctx,original["delivery"])
    successor=await ctx.send(target="@lead",mode="boundary")
    await held(ctx,successor["delivery"],"paused")
    ctx.check(len(invocations(ctx,delivery=original["delivery"]))==1 and not invocations(ctx,delivery=successor["delivery"]),
              "old exact agent keeps its gate; new handle binding inherits no permit")


async def team_fixture(ctx,count,*,chain=False):
    await ctx.native.call("add-many",count=count)
    await ctx.wait_agents(count)
    operations=[]
    for index,row in enumerate(ctx.agents):
        parent=None if index==0 else ctx.agents[index-1 if chain else 0]["id"]
        operations.append({"op":"set","key":"doc/agents/members/"+row["id"],
            "value":{"project":"large-team","reports_to":parent},"expect":0})
    for start in range(0,len(operations),64): await ordinary(ctx,operations[start:start+64])
    return "team:"+ctx.agents[0]["id"]


@case("H8")
async def mixed_member_reasons_do_not_wait_for_parent(ctx):
    await ctx.native.call("add-many",count=5);await ctx.wait_agents(5)
    principal="mixed-sender";worker=await ctx.connect(principal)
    for index in (4,0,1,2,3): await member(ctx,index,"mixed-project",None if index==4 else ctx.agents[4]["id"])
    records=[]
    for index in range(4):
        await ctx.grant("mixed-"+str(index),principal,["send:start"],"agent:"+ctx.agents[index]["id"])
        records.append(await ctx.send(target=ctx.agents[index]["id"],peer=worker,text="mixed-"+str(index)))
    await ctx.native_set(4,state="running",run="parent-still-working")
    await ctx.native_set(1,lifecycle="disconnected")
    caps=deepcopy(ctx.agents[2]["data"]["agent"]["caps"]);caps["rev"]+=1;caps["agent.send"]["modes"]={}
    await ctx.native_set(2,caps=caps)
    grant=(await ctx.peer.call("workspace.snapshot"))["entries"]["doc/agents/grants/mixed-3"]
    await ctx.command("grants",{"op":await ctx.operation(),"id":"mixed-3","value":None,"expect":grant["version"]})
    await ctx.scope("resume","team:"+ctx.agents[4]["id"])
    await settled(ctx,records[0]["delivery"])
    for index,reason in ((1,"agent_absent"),(2,"unsupported_mode"),(3,"no_grant")):
        await held(ctx,records[index]["delivery"],reason)
        ctx.check(not invocations(ctx,delivery=records[index]["delivery"]),"ineligible member reports its own reason without native call: "+reason)
    ctx.check(len(invocations(ctx,delivery=records[0]["delivery"]))==1 and
              next(r for r in await ctx.resources() if r["id"]==ctx.agents[4]["id"])["data"]["agent"]["state"]=="running",
              "ready child admits without waiting for the parent's work")


@case("H14")
async def bounded_nested_scope_with_stalled_viewer(ctx):
    capacity=ctx.status["limits"]["scope_targets"]
    if capacity<200 or capacity>1024: raise HarnessGap("deep-scope fixture requires declared200..1024 capacity")
    count=capacity+1
    await ctx.native.call("add-many",count=count);await ctx.wait_agents(count)
    def operations(start,end):
        return [{"op":"set","key":"doc/agents/members/"+ctx.agents[i]["id"],"expect":0,
                 "value":{"project":"nested-project","reports_to":None if i==0 else ctx.agents[i-1 if i<32 else 31]["id"]}}
                for i in range(start,end)]
    for start in range(0,200,64): await ordinary(ctx,operations(start,min(start+64,200)))
    for row in ctx.agents[100:140]: await ctx.native.call("remove",ref=row["backend_ref"])
    viewer=await ctx.connect("owner");view=await ctx.attach(peer=viewer)
    await ctx.wait_view(view,lambda reducer:reducer.snapshots>0)
    flood=await ctx.native.call("items-flood",ref=ctx.agents[0]["backend_ref"],count=2000,size=1024)
    scope="team:"+ctx.agents[0]["id"]
    preview=await ctx.command("scope",{"action":"inspect","scope":scope})
    result=await ctx.scope("resume",scope,preview=preview)
    ctx.check(len(preview["targets"])==200 and result["selected"]==200 and len(result["targets"])<=64,
              "deep hierarchy includes absent members, while expansion and first page remain bounded under withheld viewer credit",flood=flood)
    for start in range(200,count,64): await ordinary(ctx,operations(start,min(start+64,count)))
    small=await ctx.command("scope",{"action":"inspect","scope":"agent:"+ctx.agents[0]["id"]})
    await ctx.rejected("scope",{"op":await ctx.operation(),"action":"resume","scope":scope,"expect":small["expect"]},{"too_large","quota"})
    after=await ctx.command("scope",{"action":"inspect","scope":"agent:"+ctx.agents[0]["id"]})
    ctx.check(after["expect"]==small["expect"],"over-capacity expansion refuses before gate/control changes")


@case("H16")
async def two_hundred_member_single_scope(ctx):
    scope=await team_fixture(ctx,200)
    preview=await ctx.command("scope",{"action":"inspect","scope":scope})
    ctx.check(len(preview["targets"])==200,"exact200-member input is selected before one scope action")
    first=await ctx.send(target=ctx.agents[0]["id"],text="ready root")
    last=await ctx.send(target=ctx.agents[-1]["id"],text="ready leaf")
    result=await ctx.scope("resume",scope,preview=preview)
    ctx.check(result["selected"]==200 and len(result["targets"])<=64,"single committed scope result is paged, not a reduced input")
    targets=list(result["targets"]);page=result.get("next");pages=1
    while page is not None:
        row=await ctx.command("scope",{"action":"result","receipt":result["receipt"],"page":page})
        ctx.check(row["selected"]==200 and len(row["targets"])<=64,"each scope page retains the frozen result cut")
        targets.extend(row["targets"]);page=row.get("next");pages+=1
        if pages>10: raise HarnessGap("scope result cursor failed to terminate")
    ctx.check(len(targets)==len(set(targets))==200 and set(targets)==set(preview["targets"]),"all pages enumerate exactly the original membership revision")
    await settled(ctx,first["delivery"]);await settled(ctx,last["delivery"])
    ctx.check(len(invocations(ctx,delivery=first["delivery"]))==len(invocations(ctx,delivery=last["delivery"]))==1,
              "one action admits ready root and leaf independently")


@case("H6")
async def frozen_scope_and_historical_targets(ctx):
    await member(ctx,0,"project-frozen")
    preview=await ctx.command("scope",{"action":"inspect","scope":"project:project-frozen"})
    sent=await ctx.send()
    await member(ctx,1,"project-frozen")
    await ctx.rejected("scope",{"op":await ctx.operation(),"action":"resume","scope":"project:project-frozen","expect":preview["expect"]},{"conflict"})
    ctx.check(not invocations(ctx,delivery=sent["delivery"]),"changed membership refuses the whole stale scope before dispatch")
    fresh=await ctx.command("scope",{"action":"inspect","scope":"project:project-frozen"})
    args={"op":await ctx.operation(),"action":"resume","scope":"project:project-frozen","expect":fresh["expect"]}
    original=await ctx.command("scope",args)
    await member(ctx,1,"other-project")
    replay=await ctx.command("scope",args)
    ctx.check(set(original["targets"])=={row["id"] for row in ctx.agents} and replay["targets"]==original["targets"],
              "committed operation returns the frozen original set, not a later expansion",original=original,replay=replay)


@case("H7")
async def moving_member_does_not_replay_resume(ctx):
    await member(ctx,0,"moving-project");await member(ctx,1,"moving-project",ctx.agents[0]["id"])
    await ctx.scope("resume","team:"+ctx.agents[0]["id"])
    await member(ctx,1,"other-project")
    move_txn=(await ctx.peer.call("workspace.history",{"limit":1}))["txns"][-1]["id"]
    sent=await ctx.send(target=ctx.agents[1]["id"],text="gate survives move")
    await settled(ctx,sent["delivery"])
    ctx.check(len(invocations(ctx,delivery=sent["delivery"]))==1,"active member retains its existing gate after movement")
    await ctx.scope("pause","agent:"+ctx.agents[1]["id"]);await native_idle(ctx,1)
    await ctx.peer.call("workspace.revert",{"txn":move_txn})
    selection=await ctx.wait_command("scope",lambda row: ctx.agents[1]["id"] in row.get("targets", []),
                                     args={"action":"inspect","scope":"team:"+ctx.agents[0]["id"]})
    ctx.check(ctx.agents[1]["id"] in selection["targets"],"undoing membership restores selection")
    pending=await ctx.send(target=ctx.agents[1]["id"],text="undo must not restore old resume")
    await held(ctx,pending["delivery"],"paused")
    ctx.check(not invocations(ctx,delivery=pending["delivery"]),"membership undo cannot reopen a separately paused gate")


@case("H12")
async def controls_answers_bypass_held_mail(ctx):
    request={"request":"question","revision":1,"scope":{"session":"s1"},"status":"pending",
             "method":"confirm","title":"continue?","params":{},"incomplete":False}
    await ctx.native_set(0,requests=[request],state="running")
    sent=await ctx.send(text="held start must not block controls")
    await held(ctx,sent["delivery"],"paused")
    interrupted=await ctx.command("control",{"op":await ctx.operation(),"target":ctx.agents[0]["id"],"action":"interrupt","args":{"v":0}})
    ctx.check(interrupted["state"] in ("accepted","observed","dispatched"),"interrupt is admitted while start is held")
    request["revision"]=2;await ctx.native_set(0,requests=[request])
    stale=await ctx.command("control",{"op":await ctx.operation(),"target":ctx.agents[0]["id"],"action":"respond",
        "args":{"v":0,"request":"question","epoch":"e1","revision":1,"response":{"value":True}}})
    ctx.check(not ctx.native.records("native_response"),
              "old question revision produces zero native responses",reply=stale)
    answered=await ctx.command("control",{"op":await ctx.operation(),"target":ctx.agents[0]["id"],"action":"respond",
        "args":{"v":0,"request":"question","epoch":"e1","revision":2,"response":{"value":True}}})
    ctx.check(answered["state"] in ("accepted","observed","dispatched") and len(ctx.native.records("native_response"))==1,
              "valid answer bypasses held start and reaches native request",reply=answered)
    ctx.check(not invocations(ctx,delivery=sent["delivery"]),"neither control opens the send gate")
    await ctx.native_set(0,requests=[])
    mark=ctx.native.mark();await ctx.scope("resume");await settled(ctx,sent["delivery"])
    ctx.check(not invocations(ctx,after=mark,op="agent.respond"),"resume itself never answers an old request")


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


# Registration of independently grouped probes; imported after their shared helpers exist.
from . import inbox_cases, history_cases, priority_cases, scale_cases, archive_cases, lifecycle_cases, ordering_status_cases, control_epoch_cases, pull_followup_cases  # noqa: E402,F401
from . import identity_request_cases, native_framing_case, handover_cases, endpoint_handover_cases  # noqa: E402,F401
