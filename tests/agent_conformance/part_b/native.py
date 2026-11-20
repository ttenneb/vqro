#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Owned fake native endpoint host, exposed through the public provider protocol.

Every invocation is fsynced; delivery IDs are deliberately NOT deduplicated.
This is a coordination oracle, not an agent adapter qualification claim.
"""
import argparse
import asyncio
import hashlib
from copy import deepcopy
import json
import os
from pathlib import Path
import sys
import time


def send_caps():
    return {"v": 0, "modes": {"start": {"busy": "reject", "preflight": "serialized"},
            "admit": {}, "boundary": {"point": "after_tool_calls", "idle": "start", "batch": "one", "batch_at": "consume"},
            "settle": {"point": "settled", "idle": "start", "batch": "one", "batch_at": "consume"}, "preempt": {"busy": "replace"}},
            "guards": {"session": "runtime", "run": "runtime"},
            "content": {"types": ["text"], "shape": "ordered"}, "receipts": {"retain": 256},
            "correlation": "insertion", "literal": "always", "handled": "none", "keys": None,
            "origin": [{"as": "attributed_user", "role": "user", "frame": "sender_header"}]}


def item_provider(base):
    """SDK credit transport with a bounded agent-v0 items producer, not terminal emulation."""
    class ItemsProvider(base):
        async def call(self,method,params=None,*args,**kwargs):
            if method=="provider.register":
                params=deepcopy(params);params["limits"]["max_item_streams"]=1024
            return await super().call(method,params,*args,**kwargs)

        async def _open(self, params):
            if params.get("channel") != "items": return await super()._open(params)
            from vqro_provider import ProviderError,integer
            from vqro_provider_stream import Stream
            ident=integer(params.get("stream"));ref=params.get("ref")
            row=self.oracle.agents.get(ref)
            if not row or row["incarnation"]!=params.get("incarnation"):
                raise ProviderError("stale","item endpoint changed")
            if ident in self._stream_ids: raise ProviderError("conflict","used item stream id")
            if len(self.streams)>=self.limits.max_streams: raise ProviderError("overloaded","item stream bound full")
            integer(params.get("window_bytes"),1,self.window)
            journal=self.oracle.journal(ref)
            after=params.get("after")
            reset=not (isinstance(after,dict) and after.get("epoch")==journal["epoch"] and
                type(after.get("seq")) is int and 0<=after["seq"]<=journal["seq"] and
                (after["seq"]==journal["seq"] or journal["rows"] and after["seq"]>=journal["rows"][0][0]-1))
            cut=journal["seq"] if reset else after["seq"]
            snapshot=list(journal["entities"].values()) if reset else None
            self._stream_ids.add(ident,self.limits.identities)
            stream=Stream(self,params);self.streams[ident]=stream
            stream.on_end=lambda:journal["wake"].set()
            self._work(self._serve_items(stream,journal,snapshot,cut))
            return stream,{"initial":"reset" if reset else "continue","epoch":journal["epoch"]}

        async def _serve_items(self,stream,journal,snapshot,cursor):
            from vqro_provider import ProviderError
            encode=lambda row:json.dumps(row,ensure_ascii=False,separators=(",",":")).encode()+b"\n"
            try:
                if snapshot is not None:
                    await stream.send(encode({"op":"snapshot","phase":"begin","epoch":journal["epoch"],"coverage":"tail"}))
                    for entity in snapshot: await stream.send(encode(entity))
                    await stream.send(encode({"op":"snapshot","phase":"end","seq":cursor}))
                while not stream.closed:
                    if stream.ref not in self.oracle.agents or self.oracle.journal(stream.ref) is not journal:
                        await stream.gap("backend_dropped");return
                    rows=[(seq,data) for seq,data in journal["rows"] if seq>cursor]
                    if journal["seq"]>cursor and (not rows or rows[0][0]!=cursor+1):
                        await stream.gap("retention");return
                    for seq,data in rows:
                        await stream.send(data);cursor=seq
                    journal["wake"].clear()
                    if journal["seq"]>cursor: continue
                    await journal["wake"].wait()
            except ProviderError:
                if not stream.closed: await stream.gap("viewer_slow")
    return ItemsProvider


class Native:
    def __init__(self, provider, root):
        self.provider, self.root = provider, root
        self.agents, self.plans, self.barriers = {}, {}, {}
        self.queues, self.receipts, self.items, self.drafts = {}, {}, {}, {}
        self.pull_entries,self.pull_pending,self.context_pending,self.host_members={},{},{},{}
        self.serial = 0
        self.ledger = root / "native.jsonl"
        self.server = None
        self.attach(provider)

    def attach(self, provider):
        self.provider = provider
        provider.oracle = self
        provider.open_stream = self.open_terminal
        for op in ("agent.send", "agent.interrupt", "agent.queue", "agent.respond", "agent.stop", "agent.receipt", "terminal.kill"):
            provider.effect(op)(lambda effect, args, op=op: self.effect(op, effect, args))

    def append(self, kind, **fields):
        self.serial += 1
        row = {"seq": self.serial, "kind": kind, "pid": os.getpid(), "ns": time.monotonic_ns(), **fields}
        data = json.dumps(row, ensure_ascii=False, separators=(",", ":")).encode() + b"\n"
        if self.ledger.exists() and self.ledger.stat().st_size + len(data) > 64 << 20:
            raise RuntimeError("fixture ledger bound exceeded; evidence not discarded")
        with self.ledger.open("ab", buffering=0) as output:
            output.write(data); os.fsync(output.fileno())
        return row

    async def add(self, ref, *, pull=None, state="idle", session="s1", principal=None,kind="agent",app=None):
        host = self.provider.registration.get("host") or "fixture-process:" + str(os.getpid())
        caps = deepcopy(self.provider.capabilities)
        caps.pop("agent", None); caps["rev"] = 1
        native = {"v": 0, "epoch": "e1", "state": state, "lifecycle": "live", "session": {"id": session},
                  "run": None, "caps": caps, "incomplete": [], "host": {"id": host, "kind": "process", "owns_lifecycle": False}}
        if pull is not None: native["pull"] = deepcopy(pull)
        if principal is not None: native["principal"] = principal
        if kind=="terminal": native.pop("v");native["caps"]={}
        row = {"ref": ref, "incarnation": "i:" + ref, "kind": kind, "state": "live", "name": ref,
               "status": {"state": "idle" if state == "idle" else "working"}, "data": {"agent": native,
                   "items":{"retain_bytes":131072,"entities":512,"entity_bytes":524288}}}
        if app is not None: row["status"]["app"]=app
        self.agents[ref] = row
        self.queues[ref], self.receipts[ref], self.drafts[ref] = [], {}, b""
        self.pull_entries[ref],self.pull_pending[ref],self.context_pending[ref]=[],[],[]
        await self.provider.upsert(deepcopy(row))
        return row

    async def publish(self, ref):
        row = self.agents[ref]
        native = row["data"]["agent"]
        app=row["status"].get("app")
        row["status"] = {"state": "idle" if native["state"] == "idle" else "working"}
        if app is not None: row["status"]["app"]=app
        await self.provider.upsert(deepcopy(row))

    async def open_terminal(self,stream):
        from vqro_provider import ProviderError
        row=self.agents.get(stream.ref)
        if not row or row["incarnation"]!=stream.incarnation: raise ProviderError("stale","terminal changed")
        self.append("terminal_attach",ref=stream.ref)
        def input_bytes(data):
            if len(self.drafts[stream.ref])+len(data)>65536: raise ProviderError("overloaded","fake draft bound")
            self.append("terminal_input",ref=stream.ref,bytes=list(data))
            self.drafts[stream.ref]+=data
            stream.offer(data)
        stream.on_input=input_bytes
        stream.offer(b"fake editor> "+self.drafts[stream.ref])
        return "repaint"

    def journal(self,ref):
        epoch=self.agents[ref]["data"]["agent"]["epoch"]
        old=self.items.get(ref)
        if old is None or old["epoch"]!=epoch:
            if old is not None: old["wake"].set()
            self.items[ref]={"epoch":epoch,"seq":0,"rows":[],"bytes":0,"entities":{},"entity_bytes":0,"wake":asyncio.Event(),"position":0}
        return self.items[ref]

    def item(self,ref,value):
        journal=self.journal(ref);journal["position"]+=1
        self.entity(ref,"message","m"+str(journal["position"]),dict(value,position=journal["position"]))

    def entity(self,ref,kind,identifier,value):
        journal=self.journal(ref);key=(kind,identifier);previous=journal["entities"].get(key)
        entity={"op":"put","kind":kind,"id":identifier,"rev":previous["rev"]+1 if previous else 1,"value":deepcopy(value)}
        encoded=lambda row:json.dumps(row,ensure_ascii=False,separators=(",",":")).encode()+b"\n"
        if len(encoded(entity))>60000: raise RuntimeError("fake item exceeds declared record bound")
        def append(row):
            journal["seq"]+=1;data=encoded(dict(row,seq=journal["seq"]))
            journal["rows"].append((journal["seq"],data));journal["bytes"]+=len(data)
            while journal["bytes"]>131072:
                _,old=journal["rows"].pop(0);journal["bytes"]-=len(old)
        if previous: journal["entity_bytes"]-=len(encoded(previous))
        journal["entities"][key]=entity;journal["entity_bytes"]+=len(encoded(entity))
        while len(journal["entities"])>512 or journal["entity_bytes"]>524288:
            identifier=next(iter(journal["entities"]));removed=journal["entities"].pop(identifier)
            journal["entity_bytes"]-=len(encoded(removed))
            append({"op":"remove","kind":removed["kind"],"id":removed["id"],"rev":removed["rev"]+1})
        append(entity);journal["wake"].set()

    def insert(self, ref, args):
        # This is a fake model request, not an API/model call. Preserve attribution as a sender
        # header and label any supplied quote as a claim, never as a system/owner instruction.
        self.append("model_request", ref=ref, delivery=args.get("delivery"), role="user",
                    sender=deepcopy(args.get("origin")), quoted_claim=deepcopy(args.get("quoted")),
                    content=deepcopy(args.get("content")))
        native=self.agents[ref]["data"]["agent"]
        native.update(state="running",run="run:"+str(self.serial))
        self.item(ref,{"role":"user","content":deepcopy(args.get("content")),"final":True,
            "origin":deepcopy(args.get("origin")),"quoted":deepcopy(args.get("quoted")),
            "cause":{"kind":"delivery","id":args.get("delivery"),"provenance":"adapter"}})
        delivery = args.get("delivery")
        if delivery:
            saved = self.receipts[ref]
            saved[delivery] = {"delivery": delivery, "stage": "inserted", "run": native["run"]}
            while len(saved) > 256: saved.pop(next(iter(saved)))

    async def effect(self, op, effect, args):
        row = self.agents.get(effect.ref)
        if row is None or row["incarnation"] != effect.incarnation:
            from vqro_provider import ProviderError
            raise ProviderError("stale", "fake endpoint incarnation changed")
        witness = None
        if op=="terminal.kill":
            witness=await self.provider.call("operational.get",{"domain":"coord","key":"operational/coord/control"})
            self.append("invoke",op=op,effect=effect.id,ref=effect.ref,args=deepcopy(args),control_witness=witness)
            for ref in self.host_members.get(effect.ref,[])+[effect.ref]:
                native=self.agents[ref]["data"]["agent"];native["lifecycle"]="ended";self.agents[ref]["state"]="finished"
                await self.publish(ref)
            self.append("host_terminated",ref=effect.ref,members=self.host_members.get(effect.ref,[]))
            return {"outcome":"observed","data":{"native_host":"ended"}}
        if args.get("delivery"):
            try: witness = await self.provider.call("operational.get", {"domain": "coord", "key": "operational/coord/message/" + args["delivery"]})
            except Exception as error: witness = {"read_error": {"kind": getattr(error, "kind", "transport"), "message": str(error)}}
        disk_witness = None
        if args.get("delivery"):
            try:
                path = Path(os.environ["VQRO_STATE_DIR"]) / "operational" / "coord.json"
                with path.open("rb") as file: raw = file.read((8 << 20) + 1)
                if len(raw) > 8 << 20: raise ValueError("disk claim witness exceeds 8 MiB")
                document = json.loads(raw)
                disk_witness = {"entry":document["entries"].get("operational/coord/message/"+args["delivery"]),
                                "sha256":hashlib.sha256(raw).hexdigest()}
            except Exception as error:
                disk_witness = {"read_error":type(error).__name__+": "+str(error)}
        self.append("invoke", op=op, effect=effect.id, ref=effect.ref, args=deepcopy(args),
                    witness=witness, disk_witness=disk_witness)
        plan = self.plans.get((effect.ref, op), {})
        if plan.get("before"):
            await self.barriers.setdefault(plan["before"], asyncio.Event()).wait()
        native = row["data"]["agent"]
        guard = args.get("guard") or {}
        kind = "stale" if guard.get("session", native["session"]["id"]) != native["session"]["id"] else None
        caps = native.get("caps", {})
        if args.get("v") != 0: kind = "invalid"
        elif op not in caps: kind = "unsupported"
        elif args.get("caps_rev") is not None and args["caps_rev"] != caps.get("rev"): kind = "stale"
        if op == "agent.send":
            declaration = caps.get(op, {})
            content = args.get("content")
            if args.get("mode") not in declaration.get("modes", {}): kind = "unsupported"
            elif not isinstance(content, list): kind = "invalid"
            elif any(not isinstance(block, dict) or block.get("type") not in declaration.get("content", {}).get("types", []) for block in content): kind = "unsupported"
        if op == "agent.send" and args.get("mode") == "start" and native["state"] != "idle": kind = "busy"
        if op == "agent.send" and native.get("lifecycle") != "live": kind = "stale"
        if guard.get("run", native.get("run")) != native.get("run"): kind = "stale"
        if op == "agent.queue" and args.get("epoch") != native["epoch"]: kind = "stale"
        supplied_result = plan.get("result") is not None
        result = deepcopy(plan.get("result"))
        if kind:
            result = {"outcome": "rejected", "error": {"kind": kind, "message": "fake native precondition"}}
        if result is None:
            result = {"outcome": "accepted", "data": {"receipt": {"delivery": args.get("delivery"), "stage": "admitted"}}}
        if op == "agent.receipt" and not kind:
            return {"outcome": "observed", "data": deepcopy(self.receipts[effect.ref].get(args.get("delivery"), {"stage": "unknown"}))}
        if op == "agent.queue" and args.get("action") == "list" and not kind:
            return {"outcome": "observed", "data": {"queue": deepcopy(self.queues[effect.ref])}}
        if result["outcome"] in ("accepted", "observed", "dispatched"):
            if op == "agent.respond":
                request = next((r for r in native.get("requests", []) if r.get("request") == args.get("request")), None)
                if (not request or request.get("revision") != args.get("revision") or request.get("status") != "pending"
                        or args.get("epoch") != native["epoch"] or request.get("scope",{}).get("session") != native["session"]["id"]
                        or request.get("expires_at_ms",float("inf")) <= time.time()*1000):
                    return {"outcome": "rejected", "error": {"kind": "stale", "message": "request changed or answered"}}
                response=args.get("response")
                if (request.get("method") != "confirm" or request.get("incomplete") or not isinstance(response,dict)
                        or not (type(response.get("value")) is bool or response.get("action")=="cancel")):
                    return {"outcome":"rejected","error":{"kind":"invalid","message":"invalid confirm response"}}
                self.append("native_response",request=args["request"],revision=args["revision"],response=deepcopy(response))
                request["status"] = "cancelled" if response.get("action")=="cancel" else "answered"
            self.append("applied", op=op, ref=effect.ref, args=deepcopy(args))
            if op == "agent.send":
                mode = args.get("mode")
                if mode in ("start", "preempt") or native["state"] == "idle":
                    if mode == "preempt": self.queues[effect.ref].clear()
                    self.insert(effect.ref, args)
                    native["state"] = "running"; native["run"] = "run:" + str(self.serial)
                else:
                    self.queues[effect.ref].append(deepcopy(args))
                stage = self.receipts[effect.ref].get(args.get("delivery"), {"delivery": args.get("delivery"), "stage": "admitted"})
                self.receipts[effect.ref][args.get("delivery")] = deepcopy(stage)
                while len(self.receipts[effect.ref]) > 256: self.receipts[effect.ref].pop(next(iter(self.receipts[effect.ref])))
                if not supplied_result: result["data"] = {"receipt": deepcopy(stage)}
            elif op in ("agent.interrupt", "agent.queue"):
                self.queues[effect.ref].clear()
                if op == "agent.interrupt": native["state"] = "idle"; native["run"] = None
            elif op == "agent.stop":
                native["lifecycle"] = "ended"; row["state"] = "finished"
            await self.publish(effect.ref)
        if plan.get("after"):
            await self.barriers.setdefault(plan["after"], asyncio.Event()).wait()
        if plan.get("drop_reply"):
            await self.provider.close()
            await asyncio.Event().wait()
        return result

    async def control(self, request):
        verb = request.get("verb")
        if verb == "bind-host":
            self.host_members[request["ref"]]=list(request["members"]);return {"bound":True}
        if verb == "state": return {"agents": self.agents, "ledger_seq": self.serial,
            "registration": {k: v for k, v in self.provider.registration.items() if "token" not in k}}
        if verb == "add": return await self.add(request["ref"], **request.get("fields", {}))
        if verb == "add-many":
            count=request["count"];prefix=request.get("prefix","a")
            if type(count) is not int or not 1<=count<=2048: raise ValueError("fake endpoint bound")
            for index in range(count):
                ref=prefix+str(index)
                if ref not in self.agents: await self.add(ref,**request.get("fields",{}))
            return {"published":count}
        if verb == "items-flood":
            count=request.get("count",2000);size=request.get("size",1024);ref=request["ref"]
            if not 1<=count<=10000 or not 1<=size<=32000: raise ValueError("fake item flood bound")
            for index in range(count):
                self.item(ref,{"role":"assistant","content":[{"type":"text","text":str(index)+":"+"x"*size}],"final":True})
                if index%8==0: await asyncio.sleep(0)
            journal=self.journal(ref)
            return {"seq":journal["seq"],"retained_bytes":journal["bytes"],"entity_bytes":journal["entity_bytes"]}
        if verb == "entry":
            ref=request["ref"];supplied=request["records"]
            if not isinstance(supplied,list) or len(supplied)>16: raise ValueError("fake entry batch bound")
            entries=[]
            for row in supplied:
                entry={"id":"entry:"+str(self.serial+1),**deepcopy(row)}
                self.append("session_entry",ref=ref,entry=entry)
                self.pull_entries[ref].append(entry);self.pull_pending[ref].append(entry)
                entries.append({"delivery":row["delivery"],"entry":entry["id"]})
            if len(self.pull_entries[ref])>256: raise ValueError("fake session entry bound; evidence not dropped")
            return {"records":entries}
        if verb == "session": return {"entries":deepcopy(self.pull_entries[request["ref"]])}
        if verb == "context":
            ref=request["ref"];self.context_pending[ref].extend(deepcopy(request["records"]))
            if len(self.context_pending[ref])>16: raise ValueError("context bound")
            self.append("context_supplied",ref=ref,records=deepcopy(request["records"]))
            return {"supplied":True}
        if verb == "emit-receipt":
            ref=request["ref"];delivery=request["delivery"]
            observed=self.receipts.get(ref,{}).get(delivery)
            if not observed or observed.get("stage") not in ("inserted","read"):
                raise ValueError("no native insertion evidence for this delivery")
            self.entity(ref,"receipt",delivery,observed)
            self.append("receipt_observation",ref=ref,receipt=deepcopy(observed))
            return deepcopy(observed)
        if verb == "model":
            ref=request["ref"];entries=self.pull_pending[ref];context=self.context_pending[ref]
            self.append("model_request",ref=ref,deliveries=[row["delivery"] for row in entries],entries=deepcopy(entries),context=deepcopy(context))
            self.pull_pending[ref]=[];self.context_pending[ref]=[]
            self.agents[ref]["data"]["agent"].update(state="running",run="run:"+str(self.serial))
            await self.publish(ref);return {"deliveries":[row["delivery"] for row in entries]}
        if verb in ("abort","message_start"):
            ref=request["ref"];self.append(verb,ref=ref)
            self.agents[ref]["data"]["agent"]["state"]="idle" if verb=="abort" else "running"
            if verb=="abort": self.agents[ref]["data"]["agent"]["run"]=None
            await self.publish(ref);return {"observed":True}
        if verb == "draft":
            import base64
            ref=request["ref"]
            if "data" in request:
                data=base64.b64decode(request["data"],validate=True)
                if len(data)>65536: raise ValueError("fake draft bound")
                self.drafts[ref]=data
            return {"data":base64.b64encode(self.drafts[ref]).decode()}
        if verb == "set":
            row = self.agents[request["ref"]]
            row["data"]["agent"].update(deepcopy(request.get("fields", {})))
            await self.publish(request["ref"])
            return row
        if verb == "point":
            ref, point = request["ref"], request["point"]
            native = self.agents[ref]["data"]["agent"]
            if point in ("settle", "idle"):
                finished=native.get("run")
                for receipt in self.receipts[ref].values():
                    if finished is not None and receipt.get("run")==finished and receipt["stage"]=="inserted":
                        receipt["stage"]="read"
                native.update(state="idle", run=None, completion={"seq": self.serial, "reason": "completed"})
            pending = self.queues[ref]
            for index, args in enumerate(pending):
                if args["mode"] == "admit" or args["mode"] == "boundary" and point in ("tool", "turn", "idle") or args["mode"] == "settle" and point in ("settle", "idle"):
                    pending.pop(index); self.insert(ref, args); break
            await self.publish(ref)
            return {"pending": len(pending)}
        if verb == "plan":
            self.plans[(request["ref"], request["op"])] = deepcopy(request.get("plan", {})); return {"planned": True}
        if verb == "release":
            self.barriers.setdefault(request["barrier"], asyncio.Event()).set(); return {"released": True}
        if verb == "remove":
            row = self.agents.pop(request["ref"])
            await self.provider.gone(row["ref"], row["incarnation"])
            return {"removed": True}
        if verb == "mark": return {"seq": self.serial}
        raise ValueError("unknown fixture control: " + str(verb))

    async def connection(self, reader, writer):
        try:
            raw = await asyncio.wait_for(reader.readline(), 3)
            if not raw.endswith(b"\n") or len(raw) > 1 << 20: raise ValueError("bounded LF control required")
            request = json.loads(raw)
            response = {"id": request.get("id"), "result": await self.control(request)}
        except Exception as error:
            response = {"error": {"message": str(error)}}
        try:
            writer.write(json.dumps(response).encode() + b"\n"); await writer.drain()
        finally:
            writer.close(); await writer.wait_closed()


async def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", type=Path, required=True)
    parser.add_argument("--sdk", type=Path, required=True)
    parser.add_argument("--agents", type=int, default=2)
    parser.add_argument("--role", choices=["backend", "runtime"], default="backend")
    args = parser.parse_args()
    if not 1 <= args.agents <= 200: parser.error("agents must be 1..200")
    sys.path.insert(0, str(args.sdk))
    from vqro_provider import Provider, Limits
    capabilities = {"agent": {"v": 0, "modules": {}}, "agent.send": send_caps(), "agent.interrupt": {"v": 0},
        "agent.queue": {"v": 0, "actions": ["list", "clear"]},
        "agent.respond": {"v": 0, "methods": ["confirm"], "scopes": ["session"], "background_requests": False},
        "agent.stop": {"v": 0}, "agent.receipt": {"v": 0}}
    from vqro_provider import ProviderError
    Provider=item_provider(Provider)
    capabilities["items.stream"]={"retain_bytes":131072,"entities":512,"entity_bytes":524288}
    capabilities["terminal.output"]={"raw":True}
    capabilities["terminal.input"]={"stream":True,"effect":False}
    capabilities["terminal.kill"]={}
    native, provider = None, None
    credential = os.environ.get("VQRO_BRIDGE_TOKEN")
    try:
        while True:
            provider = Provider("part-b-native", "part-b-instance", capabilities, role=args.role,
                token=credential, limits=Limits(pending=256,max_streams=1024), software={"name": "coordination-native-oracle", "version": "0"})
            if native is None: native = Native(provider, args.root)
            else: native.attach(provider)
            try:
                await provider.connect()
                if provider.registration.get("resume_token"): credential = provider.registration["resume_token"]
                await provider.full_sync([deepcopy(row) for row in native.agents.values()])
                if not native.agents:
                    for index in range(args.agents): await native.add("a" + str(index))
                if native.server is None:
                    native.server = await asyncio.start_unix_server(native.connection, str(args.root / "n.sock"), limit=(1 << 20) + 1)
                await provider.wait_closed()
            except (OSError, ProviderError):
                pass  # Reconnect/resync facts only; no effect, content, or native action replay.
            finally:
                await provider.close()
            await asyncio.sleep(.1)
    finally:
        if native and native.server:
            native.server.close(); await native.server.wait_closed()
        if provider: await provider.close()


if __name__ == "__main__":
    asyncio.run(main())
