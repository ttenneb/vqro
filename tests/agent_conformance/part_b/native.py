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
            "correlation": "delivery", "literal": "literal", "handled": "accepted", "keys": None,
            "origin": [{"as": "attributed_user", "role": "user", "frame": "sender_header"}]}


class Native:
    def __init__(self, provider, root):
        self.provider, self.root = provider, root
        self.agents, self.plans, self.barriers = {}, {}, {}
        self.queues, self.receipts = {}, {}
        self.serial = 0
        self.ledger = root / "native.jsonl"
        self.server = None
        self.attach(provider)

    def attach(self, provider):
        self.provider = provider
        for op in ("agent.send", "agent.interrupt", "agent.queue", "agent.respond", "agent.stop", "agent.receipt"):
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

    async def add(self, ref, *, pull=None, state="idle", session="s1", principal=None):
        host = self.provider.registration.get("host") or "fixture-process:" + str(os.getpid())
        caps = deepcopy(self.provider.capabilities)
        caps.pop("agent", None); caps["rev"] = 1
        native = {"v": 0, "epoch": "e1", "state": state, "lifecycle": "live", "session": {"id": session},
                  "run": None, "caps": caps, "incomplete": [], "host": {"id": host, "kind": "process", "owns_lifecycle": False}}
        if pull is not None: native["pull"] = deepcopy(pull)
        if principal is not None: native["principal"] = principal
        row = {"ref": ref, "incarnation": "i:" + ref, "kind": "agent", "state": "live", "name": ref,
               "status": {"state": "idle" if state == "idle" else "working"}, "data": {"agent": native}}
        self.agents[ref] = row
        self.queues[ref], self.receipts[ref] = [], {}
        await self.provider.upsert(deepcopy(row))
        return row

    async def publish(self, ref):
        row = self.agents[ref]
        native = row["data"]["agent"]
        row["status"] = {"state": "idle" if native["state"] == "idle" else "working"}
        await self.provider.upsert(deepcopy(row))

    def insert(self, ref, args):
        # This is a fake model request, not an API/model call. Preserve attribution as a sender
        # header and label any supplied quote as a claim, never as a system/owner instruction.
        self.append("model_request", ref=ref, delivery=args.get("delivery"), role="user",
                    sender=deepcopy(args.get("origin")), quoted_claim=deepcopy(args.get("quoted")),
                    content=deepcopy(args.get("content")))
        delivery = args.get("delivery")
        if delivery:
            saved = self.receipts[ref]
            saved[delivery] = {"delivery": delivery, "stage": "inserted"}
            while len(saved) > 256: saved.pop(next(iter(saved)))

    async def effect(self, op, effect, args):
        row = self.agents.get(effect.ref)
        if row is None or row["incarnation"] != effect.incarnation:
            from vqro_provider import ProviderError
            raise ProviderError("stale", "fake endpoint incarnation changed")
        witness = None
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
        if op == "agent.send" and native.get("lifecycle") == "ended": kind = "stale"
        if guard.get("run", native.get("run")) != native.get("run"): kind = "stale"
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
                request = next((r for r in native.get("requests", []) if r.get("id") == args.get("request")), None)
                if not request or request.get("revision") != args.get("revision") or request.get("status") != "pending" or args.get("epoch") != native["epoch"]:
                    return {"outcome": "rejected", "error": {"kind": "stale", "message": "request changed or answered"}}
                request["status"] = "answered"
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
        if verb == "state": return {"agents": self.agents, "ledger_seq": self.serial,
            "registration": {k: v for k, v in self.provider.registration.items() if "token" not in k}}
        if verb == "add": return await self.add(request["ref"], **request.get("fields", {}))
        if verb == "set":
            row = self.agents[request["ref"]]
            row["data"]["agent"].update(deepcopy(request.get("fields", {})))
            await self.publish(request["ref"])
            return row
        if verb == "point":
            ref, point = request["ref"], request["point"]
            native = self.agents[ref]["data"]["agent"]
            if point in ("settle", "idle"):
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
        "agent.respond": {"v": 0, "methods": ["permission"], "scopes": ["session"], "background_requests": False},
        "agent.stop": {"v": 0}, "agent.receipt": {"v": 0}}
    from vqro_provider import ProviderError
    native, provider = None, None
    credential = os.environ.get("VQRO_BRIDGE_TOKEN")
    try:
        while True:
            provider = Provider("part-b-native", "part-b-instance", capabilities, role=args.role,
                token=credential, limits=Limits(pending=256), software={"name": "coordination-native-oracle", "version": "0"})
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
