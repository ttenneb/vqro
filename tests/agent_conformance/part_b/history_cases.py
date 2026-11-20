# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Real historical-Core seeding, public protected-namespace and horizon tests."""
import asyncio
import hashlib
import json
import shutil
import subprocess
from pathlib import Path
from .cases import case, ordinary, settled, invocations, native_idle
from .launcher import cli_write, tui, undo
from ..wire import Peer, RpcError
from ..results import FixtureFailed
from pi_provider_e2e import identity


def history_bytes(rows):
    return len(json.dumps(rows,sort_keys=True,separators=(",",":"),ensure_ascii=False).encode())


async def seed_old_delete(ctx,record):
    source=Path(ctx.manifest.get("legacy_core","/nonexistent"))
    expected=ctx.manifest.get("legacy_core_sha256")
    if not source.is_file() or not expected or hashlib.sha256(source.read_bytes()).hexdigest()!=expected:
        raise FixtureFailed("I4 requires explicit hash-pinned historical Core binary, not handcrafted history")
    legacy=ctx.root/"legacy";legacy.mkdir(mode=0o700)
    state=Path(ctx.backend.env["VQRO_STATE_DIR"])
    ctx.track_children();await asyncio.to_thread(ctx.backend.stop,False)
    shutil.copyfile(state/"workspace.json",ctx.root/"workspace-before-legacy.json")
    shutil.copyfile(state/"workspace.json",legacy/"workspace.json")
    socket=ctx.root/"l.sock";env=dict(ctx.backend.env,VQRO_STATE_DIR=str(legacy),VQRO_SOCKET=str(socket))
    log=(ctx.root/"legacy-core.log").open("wb")
    process=subprocess.Popen([str(source),str(legacy),str(socket),"none"],env=env,cwd=ctx.root,stdout=log,stderr=subprocess.STDOUT)
    ctx.backend.tracked.append(identity(process.pid));peer=None
    key="operational/coord/message/"+record["delivery"]
    imported="d:legacy-import-fixture";import_key="operational/coord/message/"+imported
    legacy_key="doc/i4-old-"+record["delivery"];legacy_import="doc/i4-old-import"
    try:
        for _ in range(200):
            if process.poll() is not None: raise FixtureFailed("historical Core exited; see legacy-core.log")
            if socket.exists(): break
            await asyncio.sleep(.02)
        peer=await Peer.connect(str(socket),max_record=64<<20,max_pending=32)
        await peer.call("hello",{"client":"i4-legacy-seed"})
        first=None
        for index in range(16):
            row=await peer.call("workspace.apply",{"ops":[{"op":"set","key":"doc/i4-pad","value":str(index%10)*800000}]})
            if first is None: first=row["txn"]
        await peer.call("workspace.apply",{"ops":[{"op":"set","key":legacy_key,"value":record},{"op":"set","key":"doc/i4-sentinel","value":"before"}]})
        deleted=await peer.call("workspace.apply",{"ops":[{"op":"delete","key":legacy_key},{"op":"set","key":"doc/i4-sentinel","value":"after"}]})
        shadow=dict(record,delivery=imported)
        await peer.call("workspace.apply",{"ops":[{"op":"set","key":legacy_import,"value":shadow}]})
        rows=(await peer.call("workspace.history",{"limit":500}))["txns"]
        ids=[row["id"] for row in rows]
        ctx.check(first not in ids and deleted["txn"] in ids and history_bytes(rows)>7000000,
                  "real old committer pruned full byte horizon but retained the challenged mixed delete",history_ids=ids,history_bytes=history_bytes(rows))
        ctx.evidence.append({"kind":"historical_core","sha256":expected,"delete_txn":deleted["txn"],"generated_doc_key":legacy_key,"challenged_key":key,"import_key":import_key})
    finally:
        if peer is not None: await peer.close()
        if process.poll() is None: process.terminate()
        try: await asyncio.to_thread(process.wait,4)
        except subprocess.TimeoutExpired: process.kill();await asyncio.to_thread(process.wait,3)
        log.close()
    # The old Core rejects unknown operational namespace at API validation. Build
    # an explicit namespace-import fixture from its real retained delete, rather
    # than falsely claiming it generated an operational-key transaction.
    mapping={legacy_key:key,legacy_import:import_key};modified=[]
    def migrate(value,path=()):
        if isinstance(value,dict):
            result={}
            for k,v in value.items():
                if k in mapping: modified.append({"path":[*path,k],"kind":"object_key","before":k,"after":mapping[k]})
                result[mapping.get(k,k)]=migrate(v,(*path,k))
            return result
        if isinstance(value,list): return [migrate(v,(*path,i)) for i,v in enumerate(value)]
        if isinstance(value,str) and value in mapping:
            modified.append({"path":list(path),"kind":"string","before":value,"after":mapping[value]})
            return mapping[value]
        return value
    original=(legacy/"workspace.json").read_bytes()
    transformed=json.dumps(migrate(json.loads(original)),separators=(",",":")).encode()
    (state/"workspace.json").write_bytes(transformed)
    ctx.evidence.append({"kind":"explicit_legacy_namespace_import_fixture","mapping":mapping,"modified_json_paths":modified,
                         "original_sha256":hashlib.sha256(original).hexdigest(),"transformed_sha256":hashlib.sha256(transformed).hexdigest(),
                         "genuine_old_operational_transaction":False})
    await ctx.restart(crash=False)
    return deleted["txn"],key,imported


@case("I4")
async def ordinary_history_cannot_recreate_authority(ctx):
    sent=await ctx.send(text="actual historical pending input")
    record=await ctx.get(sent["delivery"])
    deleted,key,imported=await seed_old_delete(ctx,record)
    rows=(await ctx.peer.call("workspace.history",{"limit":1000}))["txns"]
    ctx.check(deleted in [row["id"] for row in rows] and history_bytes(rows)>7000000,
              "current public Core retained a genuinely full ordinary byte horizon and the old delete")
    await ctx.scope("resume");await settled(ctx,sent["delivery"])
    held=await ctx.send(text="busy gate-open message to edit",send_new=True)
    await ctx.wait_command("inbox.get",lambda row:row.get("reason")=="agent_busy",args={"delivery":held["delivery"]})
    baseline=(await ctx.peer.call("workspace.history",{"limit":1000}))["txns"]
    for index in range(501):
        for retry in range(8):
            current=await ctx.get(held["delivery"])
            try:
                await ctx.command("inbox.edit",{"op":await ctx.operation(),"delivery":held["delivery"],"expect":current["revision"],"content":[{"type":"text","text":str(index)}]})
                break
            except RpcError as error:
                if error.kind!="conflict" or retry==7: raise
    after=(await ctx.peer.call("workspace.history",{"limit":1000}))["txns"]
    ctx.check(after==baseline and history_bytes(after)==history_bytes(baseline),
              "501 real protected message edits under open gates preserve every full-horizon ordinary record and byte",history_bytes=history_bytes(after))
    current=await ctx.get(held["delivery"])
    await ctx.command("inbox.withdraw",{"op":await ctx.operation(),"delivery":held["delivery"],"expect":current["revision"]})
    async def refused(method,args):
        before=(await ctx.peer.call("workspace.history",{"limit":1000}))["txns"]
        snapshot=await ctx.peer.call("workspace.snapshot")
        try: await ctx.peer.call(method,args)
        except RpcError as error: ctx.check(error.kind=="forbidden","reserved namespace rejects a retained delete, not a pruned/not-found substitute",error=error.kind)
        else: ctx.check(False,"ordinary mutation unexpectedly crossed protected namespace")
        ctx.check((await ctx.peer.call("workspace.history",{"limit":1000}))["txns"]==before and (await ctx.peer.call("workspace.snapshot"))["entries"]==snapshot["entries"],
                  "rejected mixed/protected operation changes no ordinary entry or history")
    await refused("workspace.apply",{"ops":[{"op":"set","key":"doc/i4-sentinel","value":"bad"},{"op":"set","key":key,"value":record}]})
    await refused("workspace.apply",{"ops":[{"op":"delete","key":key}]})
    await refused("workspace.revert",{"txn":deleted})
    for principal in ("owner","worker:alice","worker:bob"):
        own_key="doc/i4-undo-"+principal.replace(":","-")
        await cli_write(ctx,"i4-cli-"+principal,principal,[{"op":"set","key":own_key,"value":True}])
        async with tui(ctx,"i4-tui-"+principal,principal) as ui: await undo(ctx,ui,own_key)
    before=len(invocations(ctx));await native_idle(ctx);await asyncio.sleep(.15)
    ctx.check(len(ctx.native.records("model_request"))==1 and len(invocations(ctx))==before and not invocations(ctx,delivery=imported),
              "ordinary import/delete/revert and every selected principal's actual u create no protected pending authority or extra native input with gates open")
