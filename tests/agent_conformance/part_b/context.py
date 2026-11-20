# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Private real-core fixture. Target coordination code is never imported in-process."""
import asyncio
from copy import deepcopy
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
from types import SimpleNamespace

from pi_provider_e2e import E2E, identity
from ..core import Native
from ..results import CheckFailed, FixtureFailed, HarnessGap, check
from ..wire import Peer, RpcError


class PublicCoordination:
    def __init__(self, root, manifest, vqro, hc=None):
        self.root, self.manifest, self.vqro = Path(root), manifest, Path(vqro)
        self.root.mkdir()
        self.backend = E2E(SimpleNamespace(isolated_root=self.root, agents_extension=False,
                                          vqro=self.vqro, hc=Path(hc or "/bin/false")))
        self.backend.env["VQRO_HERDR"] = '["/bin/false"]'
        self.peers, self.native_process, self.peer = {}, None, None
        self.assertions = 0
        self.serial, self.agents, self.evidence = 0, [], []
        self.native = Native(self.root / "n.sock", self.root / "native.jsonl")
        self.metadata = {"name": manifest["name"], "target_kind": "coordination", "fixture_native": True,
                         "real_runtime_certification": False, "extension_files": {}}
        source = Path(manifest["extensions"])
        if not source.is_absolute() or not (source / "coordination.py").is_file():
            raise FixtureFailed("explicit K2 extension directory with coordination.py required")
        for path in sorted(source.glob("*.py")):
            data = path.read_bytes(); (self.root / "ext" / path.name).write_bytes(data)
            self.metadata["extension_files"][path.name] = hashlib.sha256(data).hexdigest()
        original = source / "manifests/coord.json"
        if not original.is_file(): raise FixtureFailed("K2 managed coord manifest required, dependencies not stripped")
        (self.root / "ext/manifests").mkdir()
        shutil.copyfile(original, self.root / "ext/manifests/coord.json")
        (self.root / "ext/defaults.json").write_text(json.dumps({"version": 2, "extensions": [{
            "name": "coord", "since": 2, "default": True, "kind": "management",
            "argv": ["/usr/bin/python3", str(self.root / "ext/coordination.py")]}]}))
        self.metadata["core_sha256"] = hashlib.sha256(self.vqro.read_bytes()).hexdigest()
        self.fixture = Path(self.manifest.get("native_fixture", Path(__file__).parent / "native.py"))
        self.metadata["native_sha256"] = hashlib.sha256(self.fixture.read_bytes()).hexdigest()

    async def connect(self, principal="owner", *, label=None):
        peer = await Peer.connect(self.backend.env["VQRO_SOCKET"], max_record=64 << 20, max_pending=512)
        params = {"client": "coord-conformance"}
        if principal == "owner": params["identity"] = {"kind": "owner"}
        elif principal is not None: params["identity"] = {"kind": "principal", "principal": principal}
        if label is not None: params["label"] = label
        hello = await peer.call("hello", params)
        self.peers.setdefault(principal, []).append(peer)
        self.evidence.append({"kind": "connection", "requested_identity": params.get("identity"), "hello": hello})
        return peer

    async def start_core(self, fault=None):
        driver = self.manifest.get("core_driver")
        if fault is not None and not driver:
            raise FixtureFailed("case requires real-core durability/crash seam; no core_driver declared")
        if fault is None:
            argv = [str(self.vqro), "serve", "--fake"]
        else:
            values = {"{state}": self.backend.env["VQRO_STATE_DIR"], "{socket}": self.backend.env["VQRO_SOCKET"], "{fault}": fault}
            argv = [values.get(value, value) for value in driver]
        if not argv or not Path(argv[0]).is_absolute(): raise FixtureFailed("absolute private core executable required")
        with (self.root / "vqro.log").open("ab") as log:
            self.backend.core = subprocess.Popen(argv, cwd=self.root / "work", env=self.backend.env,
                                                 stdout=log, stderr=subprocess.STDOUT)
        self.backend.tracked.append(identity(self.backend.core.pid))
        async def connected():
            try: return await self.connect()
            except (OSError, RpcError): return None  # hello is a read-only startup handshake, not an effect retry
        self.peer = await self.wait(connected, label="private core handshake")

    def track_children(self):
        if self.backend.core is None or self.backend.core.poll() is not None: return
        pending = [self.backend.core.pid]
        while pending:
            pid = pending.pop()
            try: children = Path(f"/proc/{pid}/task/{pid}/children").read_text().split()
            except FileNotFoundError: continue
            for child in children:
                record = identity(int(child))
                if record and record not in self.backend.tracked:
                    self.backend.tracked.append(record); pending.append(int(child))

    def check(self, condition, message, *, component="coordination", **evidence):
        self.assertions += 1
        if evidence: self.evidence.append({"assertion": message, "passed":bool(condition), **evidence})
        check(condition, message, component=component)

    async def start(self):
        check(not any(key.startswith("HERDR_") for key in self.backend.env), "HERDR inherited", component="harness")
        await self.start_core()
        self.status = await self.wait_command("status", lambda row: row.get("state") in ("ready", "unavailable"))
        if self.status.get("state") != "ready":
            raise CheckFailed(f"coordination unavailable: {self.status}", component="core")
        self.metadata["coordination_declaration"] = deepcopy(self.status)
        self.track_children()
        env = dict(self.backend.env)
        argv = ["/usr/bin/python3", str(self.fixture), "--root", str(self.root), "--sdk", str(self.root / "ext"),
                "--agents", str(self.manifest.get("agents", 2))]
        with (self.root / "native.log").open("ab") as log:
            self.native_process = subprocess.Popen(argv, cwd=self.root / "work", env=env,
                                                   stdout=log, stderr=subprocess.STDOUT)
        self.backend.tracked.append(identity(self.native_process.pid))
        await self.wait(lambda: self.native.endpoint.exists(), label="fake native control socket")
        await self.wait_agents(self.manifest.get("agents", 2))
        return self

    async def wait(self, predicate, *, label="condition", timeout=8):
        async def polling():
            while True:
                value = predicate()
                if hasattr(value, "__await__"): value = await value
                if value: return value
                await asyncio.sleep(.01)
        try: return await asyncio.wait_for(polling(), timeout)
        except asyncio.TimeoutError as error: raise CheckFailed(f"timed out: {label}", component="coordination") from error

    async def resources(self):
        return (await self.peer.call("resource.list", {"provider": "part-b-native"}))["resources"]

    async def wait_agents(self, count):
        async def ready():
            rows = [row for row in await self.resources() if row.get("available") and row.get("kind") == "agent"]
            if len(rows) >= count:
                self.agents = sorted(rows, key=lambda row: row["backend_ref"])
                return self.agents
        return await self.wait(ready, label=f"{count} native agents")

    async def command(self, name, args=None, *, peer=None):
        return await (peer or self.peer).call("command.invoke", {"name": "coord." + name, "args": args or {}}, timeout=8)

    async def wait_command(self, name, predicate, *, args=None):
        async def ready():
            try: row = await self.command(name, args)
            except RpcError as error:
                if error.kind == "not_found": return None
                raise
            return row if predicate(row) else None
        return await self.wait(ready, label="coord." + name)

    async def operation(self, suffix="op"):
        self.serial += 1
        window = await self.command("inbox.window")
        return window["epoch"] + f":b{self.serial}:{suffix}"

    async def send(self, *, target=None, text="literal conformance input", peer=None, **fields):
        op = await self.operation("send")
        return await self.command("inbox.send", {"op": op, "submission": op,
            "to": target or self.agents[0]["id"], "mode": "start",
            "content": [{"type": "text", "text": text}], **fields}, peer=peer)

    async def get(self, delivery):
        return await self.command("inbox.get", {"delivery": delivery})

    async def grant(self, identifier, principal, rights, target="*", *, until_ms=None, peer=None):
        value = {"who": principal, "can": list(rights), "on": target}
        if until_ms is not None: value["until_ms"] = until_ms
        return await self.command("grants", {"op": await self.operation("grant"), "id": identifier,
            "value": value, "expect": 0}, peer=peer)

    async def native_set(self, index, **fields):
        target = self.agents[index]
        await self.native.call("set", ref=target["backend_ref"], fields=fields)
        async def updated():
            rows = await self.resources()
            return next((row for row in rows if row["id"] == target["id"] and
                all(row["data"]["agent"].get(key) == value for key, value in fields.items())), None)
        return await self.wait(updated, label="published native facts")

    async def scope(self, action, scope=None, *, peer=None, preview=None, op=None):
        scope = scope or "agent:" + self.agents[0]["id"]
        preview = preview or await self.command("scope", {"action": "inspect", "scope": scope}, peer=peer)
        return await self.command("scope", {"op": op or await self.operation(action), "action": action,
            "scope": scope, "expect": preview["expect"]}, peer=peer)

    async def rejected(self, name, args, kinds, *, peer=None):
        try: result = await self.command(name, args, peer=peer)
        except RpcError as error:
            self.check(error.kind in kinds, f"wrong refusal: {error.kind}; expected {kinds}")
            return error
        raise CheckFailed(f"expected definite refusal {kinds}, got {result}", component="coordination")

    async def wait_cut(self, expected):
        path=Path(self.backend.env["VQRO_STATE_DIR"])/"fault-boundary"
        await self.wait(lambda:path.is_file(),label="real committer barrier "+expected)
        actual=path.read_text()
        if actual!=expected: raise FixtureFailed(f"wrong real committer cut: {actual}, expected {expected}")
        self.evidence.append({"kind":"committer_barrier","phase":actual})

    def release_cut(self):
        (Path(self.backend.env["VQRO_STATE_DIR"])/"release-fault").touch()

    async def restart(self, *, crash=True, fault=None, wait_ready=True):
        for peers in self.peers.values():
            for peer in peers: await peer.close()
        self.peers.clear()
        self.track_children()
        await asyncio.to_thread(self.backend.stop, crash)
        for name in ("fault-boundary","release-fault","ready"):
            (Path(self.backend.env["VQRO_STATE_DIR"])/name).unlink(missing_ok=True)
        await self.start_core(fault)
        if not wait_ready: return None
        status = await self.wait_command("status", lambda row: row.get("state") == "ready")
        self.track_children()
        if self.native_process is not None: await self.wait_agents(self.manifest.get("agents", 2))
        return status

    async def close(self):
        self.track_children()
        for peers in self.peers.values():
            for peer in peers: await peer.close()
        if self.native_process is not None and self.native_process.poll() is None:
            self.native_process.terminate()
            try: await asyncio.to_thread(self.native_process.wait, 4)
            except subprocess.TimeoutExpired:
                self.native_process.kill(); await asyncio.to_thread(self.native_process.wait, 4)
        await asyncio.to_thread(self.backend.cleanup)
        (self.root / "lifecycle.json").write_text(json.dumps(self.backend.log, ensure_ascii=False, indent=2))
        self.evidence.append({"kind": "cleanup", "owned_native_reaped": self.native_process is None or self.native_process.poll() is not None,
                              "tracked": self.backend.tracked, "private_environment": self.backend.env})
