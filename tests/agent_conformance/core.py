"""Public-core fixture, reusing the existing private backend lifecycle harness.

No adapter imports or internal resource mutation. Every test effect goes over
resource.invoke; native control goes only to the fixture-owned socket.
"""
import asyncio
from collections import deque
from copy import deepcopy
import hashlib
import json
import os
from pathlib import Path
import shutil
from types import SimpleNamespace

from pi_provider_e2e import E2E, identity, live
from .results import CheckFailed, FixtureFailed, HarnessGap, check
from .wire import ItemReducer, Peer, RpcError, WireFailure, close_writer

HERE = Path(__file__).resolve().parent
MAX_LEDGER = 16 << 20


class Native:
    def __init__(self, control, ledger):
        self.endpoint, self.path = control, ledger
        self.serial = 0
        self.state = None

    async def call(self, verb, *, timeout=2, **params):
        self.serial += 1
        ident = self.serial
        try:
            reader, writer = await asyncio.wait_for(asyncio.open_unix_connection(str(self.endpoint)), timeout)
        except (OSError, asyncio.TimeoutError) as error:
            raise FixtureFailed(f"native control unavailable: {error}") from error
        try:
            writer.write(json.dumps({"id": ident, "verb": verb, **params}).encode() + b"\n")
            await writer.drain()
            raw = await asyncio.wait_for(reader.readline(), timeout)
            if not raw:
                raise FixtureFailed("native fixture closed without control reply")
            row = json.loads(raw)
            check(row.get("id") == ident, "native fixture reply id mismatch", component="fixture")
            if "error" in row:
                raise FixtureFailed(row["error"].get("message", "fixture control error"))
            return row["result"]
        except (ValueError, KeyError, asyncio.TimeoutError) as error:
            raise FixtureFailed(f"invalid/timed-out native fixture control: {error}") from error
        finally:
            await close_writer(writer)

    def records(self, kind=None, *, after=0, operation=None):
        if not self.path.exists():
            return []
        if self.path.stat().st_size > MAX_LEDGER:
            raise FixtureFailed("native ledger exceeds declared fixture bound")
        data = self.path.read_bytes()
        # A concurrent write can leave one partial final LF record. It is not
        # evidence yet; the next pure read may observe the completed append.
        rows = []
        for raw in data.split(b"\n")[:-1]:
            try:
                row = json.loads(raw)
            except ValueError as error:
                raise FixtureFailed("corrupt completed native ledger record") from error
            if row["seq"] > after and (kind is None or row["kind"] == kind) and (operation is None or row.get("operation") == operation):
                rows.append(row)
        return rows

    def mark(self):
        rows = self.records()
        return rows[-1]["seq"] if rows else 0

    def writes(self, after=0):
        return [row for row in self.records("command", after=after) if row.get("effectful")]

    async def wait(self, predicate, *, timeout=2):
        async def waiting():
            while True:
                result = predicate(self.records())
                if result:
                    return result
                await asyncio.sleep(.01)
        try:
            return await asyncio.wait_for(waiting(), timeout)
        except asyncio.TimeoutError as error:
            raise CheckFailed("native ledger condition did not occur within fixture deadline") from error


class View:
    def __init__(self, peer, stream, reducer, initial, epoch):
        self.peer, self.stream, self.reducer = peer, stream, reducer
        self.initial, self.epoch, self.closed = initial, epoch, None


class PublicImplementation:
    def __init__(self, root, manifest, vqro, hc, *, script=None):
        self.root, self.manifest = Path(root), manifest
        self.root.mkdir()
        (self.root / ".private-e2e").touch()
        args = SimpleNamespace(isolated_root=self.root, agents_extension=False, vqro=vqro, hc=hc)
        self.backend = E2E(args)
        self.native = Native(self.root / "c.sock", self.root / "native.jsonl")
        self.peers, self.views, self.early = [], {}, {}
        self.early_bytes, self.early_records = 0, 0
        self.events = deque(maxlen=128)
        self.resource_cache, self.resource_sizes, self.removed = {}, {}, {}
        self.resource_cache_bytes = 0
        self.resource_history, self.resource_history_bytes = deque(), 0
        self.evidence = []
        self.agents, self.provider, self.host = [], None, None
        self.endpoint_count = manifest.get("endpoints", 1)
        self.serial = 0
        self.script = script or {"script": []}
        self.harness_dir = self.root / "harness"
        self.harness_dir.mkdir()
        self.fixture_dir = self.harness_dir / "agent_conformance"
        shutil.copytree(HERE, self.fixture_dir, ignore=shutil.ignore_patterns("__pycache__"))
        native_fixtures = self.harness_dir / "fixtures"
        native_fixtures.mkdir()
        for filename in ("fake_codex.py", "codex_control.py", "acp_conformance.py", "acp_conformance_model.py"):
            source = HERE.parent / "fixtures" / filename
            if source.is_file():
                shutil.copy2(source, native_fixtures / filename)
        (self.root / "script.json").write_text(json.dumps(self.script) + "\n")
        placeholders = {"{python}": "/usr/bin/python3", "{fixture}": str(self.fixture_dir / "runtime.py"),
                        "{control}": str(self.native.endpoint), "{ledger}": str(self.native.path),
                        "{script}": str(self.root / "script.json"), "{root}": str(self.root),
                        "{harness_root}": str(self.harness_dir), "{codex_fixture}": str(native_fixtures / "fake_codex.py"),
                        "{acp_fixture}": str(native_fixtures / "acp_conformance.py"),
                        "{work}": str(self.root / "work")}
        self.argv = [placeholders.get(value, value) for value in manifest["argv"]]
        if not self.argv or not Path(self.argv[0]).is_absolute():
            raise HarnessGap("adapter argv[0] must be an explicit absolute executable")
        self.metadata = {"name": manifest["name"], "kind": manifest.get("kind", "adapter"),
                         "declared_adapter_version": manifest.get("adapter_version", "unreported"),
                         "runtime": {"name": "scripted-native", "wire": manifest.get("wire", "pi"), "version": "fixture-v0",
                                     "source_sha256": hashlib.sha256((self.fixture_dir / "runtime.py").read_bytes()).hexdigest(),
                                     "native_sources": {path.name: hashlib.sha256(path.read_bytes()).hexdigest()
                                                        for path in native_fixtures.glob("*.py")}},
                         "provenance": "native adapter contract; only exercised native-ledger traces are verified",
                         "argv": self.argv}

    def _notice(self, peer, method, params):
        key = (id(peer), params.get("stream"))
        view = self.views.get(key)
        if view is None:
            size = len(json.dumps(params).encode()) + 64
            if self.early_bytes + size > 1 << 20 or self.early_records >= 1024:
                raise WireFailure("bounded early-attach viewer buffer exceeded")
            self.early.setdefault(key, []).append((method, params, size))
            self.early_bytes += size
            self.early_records += 1
            return
        if method == "stream.data":
            view.reducer.stream_data(params)
        else:
            reason = params.get("reason", "unknown")
            if isinstance(reason, dict):
                reason = reason.get("reason", "unknown")
            view.closed = reason
            view.reducer.ended(reason)

    def _event(self, row):
        if row.get("type") == "effect":
            self.events.append(row)
        elif row.get("type") == "resource":
            resource = row.get("data", {})
            ident = resource.get("id")
            if not isinstance(ident, str):
                raise WireFailure("resource event lacks exact id")
            size = len(json.dumps(resource).encode())
            total = self.resource_cache_bytes - self.resource_sizes.get(ident, 0) + size
            if size > 256 << 10 or total > 16 << 20 or (ident not in self.resource_cache and len(self.resource_cache) >= 512):
                raise WireFailure("lifecycle observer cache exceeds its count/byte bound")
            self.resource_cache[ident], self.resource_sizes[ident] = deepcopy(resource), size
            self.resource_cache_bytes = total
            self.resource_history.append((row.get("epoch"), row.get("seq"), self.resource_cache[ident], size))
            self.resource_history_bytes += size
            while len(self.resource_history) > 64 or self.resource_history_bytes > 4 << 20:
                self.resource_history_bytes -= self.resource_history.popleft()[3]
        elif row.get("type") == "resource.removed":
            if len(self.removed) >= 512 and row.get("data", {}).get("id") not in self.removed:
                raise WireFailure("removed-resource observer bound exceeded")
            self.removed[row["data"]["id"]] = deepcopy(row["data"])

    def facts_at_audit(self, rid, audit):
        for epoch, sequence, resource, _ in reversed(self.resource_history):
            if epoch == audit.get("epoch") and type(sequence) is int and sequence <= audit.get("seq", -1) and resource.get("id") == rid:
                return deepcopy(resource.get("data", {}).get("agent", {}))
        raise HarnessGap("resource fact event before final effect audit is absent from bounded observer history")

    async def viewer(self):
        peer = await Peer.connect(self.backend.env["VQRO_SOCKET"], max_record=8 << 20)
        self.peers.append(peer)
        await peer.call("hello", {"client": "agent-conformance", "identity": {"kind": "owner"}})
        # Resource facts are read explicitly; do not queue token/fact events in
        # the generic viewer's diagnostic notification queue.
        peer.on("event", self._event)
        for method in ("stream.data", "stream.closed", "stream.gap"):
            peer.on(method, lambda params, method=method: self._notice(peer, method, params))
        return peer

    async def start(self):
        check(not any(key.startswith("HERDR_") for key in self.backend.env), "HERDR environment inherited", component="harness")
        started = await asyncio.to_thread(self.backend.hc, "server", "start")
        self.backend.hc_process = identity(started["pid"])
        check(not (await asyncio.to_thread(self.backend.hc, "ls", "--all"))["terminals"], "private backend was not empty", component="harness")
        await asyncio.to_thread(self.backend.start)
        self.metadata["core_image_sha256"] = hashlib.sha256(Path(f"/proc/{self.backend.core.pid}/exe").read_bytes()).hexdigest()
        self.peer = await self.viewer()
        snapshot = await self.peer.call("workspace.snapshot")
        await self.peer.call("events.subscribe", {"epoch": snapshot["epoch"], "from": snapshot["seq"], "types": ["effect", "resource", "resource.removed"]})
        spawned = await self.peer.call("resource.spawn", {"bridge": True, "name": "agent-conformance",
            "argv": self.argv, "cwd": str(self.root / "work"), "env": {}, "deadline_ms": 1500})
        check(spawned.get("outcome") == "observed", "bridge launch not observed", component="fixture")
        self.host = spawned["resource"]["id"]
        terminal = await asyncio.to_thread(self.backend.hc, "get", spawned["resource"]["backend_ref"])
        bridge = identity(terminal["pid"])
        if bridge:
            self.backend.tracked.append(bridge)
            try:
                self.metadata["adapter_image"] = {"pid": bridge["pid"], "birth": bridge["birth"],
                    "sha256": hashlib.sha256(Path(f'/proc/{bridge["pid"]}/exe').read_bytes()).hexdigest()}
            except FileNotFoundError:
                self.metadata["adapter_image"] = {"unavailable": "adapter exited before image hash"}
        if self.manifest.get("expected_image_sha256"):
            check(self.metadata.get("adapter_image", {}).get("sha256") == self.manifest["expected_image_sha256"],
                  "executed adapter image differs from the pinned target manifest", component="fixture")
        async def ready():
            while True:
                if self.native.endpoint.exists():
                    state = await self.native.call("state")
                    resources = await self.resources()
                    agents = [row for row in resources if row["kind"] == "agent" and row.get("available")]
                    if len(agents) >= self.endpoint_count:
                        self.native.state, self.agents = state, sorted(agents, key=lambda row: row["id"])
                        return
                if bridge and not live(bridge):
                    raise FixtureFailed("adapter exited before fake-native control and registration became ready")
                await asyncio.sleep(.02)
        try:
            await asyncio.wait_for(ready(), 5)
        except asyncio.TimeoutError as error:
            raise FixtureFailed("adapter did not register and start the scripted native fixture in 5 seconds") from error
        check(self.native.state.get("wire") == self.manifest.get("wire", "pi"), "wrong native fixture wire", component="fixture")
        native_process = identity(self.native.state["pid"])
        if native_process:
            self.backend.tracked.append(native_process)
        ready_rows = self.native.records("ready")
        check(ready_rows and not ready_rows[0]["herdr_in_child"] and not ready_rows[0]["bootstrap_in_child"],
              "privileged bootstrap/Herdr variables reached native child")
        snapshot = await self.peer.call("workspace.snapshot")
        self.provider = snapshot["providers"][self.agents[0]["provider"]]
        self.metadata.update({"provider": self.provider, "resource_declarations": [r.get("data", {}).get("agent", {}) for r in self.agents],
                              "fixture_limits": self.native.state.get("limits", {})})
        self.evidence.append({"kind": "native_ledger", "path": str(self.native.path)})
        return self

    async def resources(self, peer=None):
        return (await (peer or self.peer).call("resource.list"))["resources"]

    async def resource(self, rid=None, *, peer=None):
        rid = rid or self.agents[0]["id"]
        return next((r for r in await self.resources(peer) if r["id"] == rid), None)

    async def facts(self, rid=None, *, peer=None):
        row = await self.resource(rid, peer=peer)
        check(row is not None, "endpoint disappeared without inspectable end facts")
        data = row.get("data") or {}
        check(isinstance(data, dict), "structured endpoint data is not an object")
        facts = data.get("agent", {})
        check(isinstance(facts, dict), "data.agent is not an object")
        return facts

    async def wait_facts(self, predicate, rid=None, *, timeout=2):
        async def waiting():
            while True:
                resource = await self.resource(rid)
                data = (resource.get("data") or {}) if resource else {}
                check(isinstance(data, dict), "structured endpoint data is not an object")
                fact = data.get("agent", {})
                check(isinstance(fact, dict), "data.agent is not an object")
                if fact and predicate(fact):
                    return fact
                await asyncio.sleep(.01)
        try:
            return await asyncio.wait_for(waiting(), timeout)
        except asyncio.TimeoutError as error:
            raise CheckFailed("public agent facts did not reach the required state in 2 seconds") from error

    async def invoke(self, op, args=None, *, rid=None, peer=None, deadline_ms=1000, defaults=True, **common):
        if defaults:
            facts = await self.facts(rid)
            args = {"v": 0, "epoch": facts.get("epoch"), "caps_rev": facts.get("caps", {}).get("rev"), **(args or {})}
        try:
            return await (peer or self.peer).call("resource.invoke", {"resource": rid or self.agents[0]["id"],
                "op": op, "args": args or {}, "deadline_ms": deadline_ms, **common}, timeout=2)
        except RpcError as error:
            # Preserve the actual core outcome; an unqualified JSON-RPC error
            # must not be dressed up as a definite provider rejection.
            return {"outcome": error.data.get("outcome"), "error": error.error, "rpc_error": True}

    async def ensure_session(self, rid=None):
        facts = await self.facts(rid)
        if (facts.get("session") or {}).get("id"):
            return facts
        session = facts.get("caps", {}).get("agent.session", {})
        if "new" not in session.get("actions", []):
            raise HarnessGap("send/work fixture needs a session but cannot bootstrap one through declared agent.session:new")
        result = await self.invoke("agent.session", {"action": "new"}, rid=rid)
        check(result.get("outcome") in ("accepted", "observed"), f"fixture session bootstrap failed: {result}")
        fresh = await self.facts(rid)
        check((fresh.get("session") or {}).get("id"), "session result was returned before its facts became visible")
        self.evidence.append({"kind": "session_bootstrap", "resource": rid or self.agents[0]["id"],
                              "session": fresh["session"]["id"], "native_input_sent": False})
        return fresh

    def delivery(self):
        self.serial += 1
        return f"k1-{self.root.parent.name}-{self.root.name}-{self.serial}"

    async def send(self, text, mode=None, *, content=None, rid=None, peer=None, deadline_ms=1000,
                   cause=None, correlation=None, **args):
        facts = await self.facts(rid)
        modes = facts.get("caps", {}).get("agent.send", {}).get("modes", {})
        mode = mode or next((name for name in ("start", "admit") if name in modes), None)
        if mode is None:
            raise HarnessGap("scenario needs a start/admit mode but neither is declared")
        # Principal/actor/initiator are core-bound, not invented fixture identities.
        origin = {"kind": "human"}
        links = {key: value for key, value in (("cause", cause), ("correlation", correlation)) if value is not None}
        return await self.invoke("agent.send", {"mode": mode, "delivery": self.delivery(),
            "content": content if content is not None else [{"type": "text", "text": text}],
            "origin": origin, "render": ["user", "participant", "attributed_user"], **links, **args},
            rid=rid, peer=peer, deadline_ms=deadline_ms, **links)

    async def attach(self, *, rid=None, peer=None, after=None, seed=None):
        peer = peer or self.peer
        try:
            row = await peer.call("resource.attach", {"resource": rid or self.agents[0]["id"],
                "items": {} if after is None else {"after": after}, "deadline_ms": 1500})
        except RpcError as error:
            component = "adapter" if error.data.get("effect") and error.kind != "overloaded" else "core"
            raise CheckFailed(f"mandatory item attach failed: {error.error}", component=component) from error
        reducer = deepcopy(seed) if seed is not None else ItemReducer()
        reducer.offset, reducer.failure, reducer.staged = 0, None, None
        reducer.pending.clear()
        view = View(peer, row["stream"], reducer, row.get("initial"), row.get("epoch"))
        key = (id(peer), view.stream)
        self.views[key] = view
        for method, params, size in self.early.pop(key, []):
            self.early_bytes -= size
            self.early_records -= 1
            self._notice(peer, method, params)
        return view

    async def wait_view(self, view, predicate, *, timeout=2):
        def observed():
            if view.reducer.failure:
                raise CheckFailed(f"item reconstruction gap/error: {view.reducer.failure}")
            return predicate(view.reducer)
        try:
            return await view.peer.wait(observed, timeout=timeout)
        except asyncio.TimeoutError as error:
            raise CheckFailed("item stream did not reach expected state") from error

    async def restart_core(self):
        for peer in self.peers:
            await peer.close()
        self.peers.clear()
        await asyncio.to_thread(self.backend.stop, True)
        await asyncio.to_thread(self.backend.start)
        self.peer = await self.viewer()
        snapshot = await self.peer.call("workspace.snapshot")
        for resource in snapshot.get("resources", []):
            self._event({"type": "resource", "data": resource})
        await self.peer.call("events.subscribe", {"epoch": snapshot["epoch"], "from": snapshot["seq"],
                                                 "types": ["effect", "resource", "resource.removed"]})
        self.evidence.append({"kind": "core_restart", "native_pid": self.native.state["pid"]})

    async def close(self):
        failures = []
        try:
            failures = await asyncio.gather(*(peer.close() for peer in self.peers), return_exceptions=True)
        finally:
            self.peers.clear()
            # A failed/cancelled peer shutdown must never skip owned-child cleanup.
            try:
                await asyncio.to_thread(self.backend.cleanup)
                if self.native.endpoint.exists():
                    try:
                        _, writer = await asyncio.wait_for(asyncio.open_unix_connection(str(self.native.endpoint)), .2)
                    except (ConnectionRefusedError, FileNotFoundError):
                        self.native.endpoint.unlink(missing_ok=True)
                    else:
                        await close_writer(writer)
                        raise FixtureFailed("owned native fixture still listens after backend cleanup")
            finally:
                (self.root / "lifecycle.json").write_text(json.dumps({"log": self.backend.log,
                    "environment": self.backend.env, "network_namespace": os.readlink("/proc/self/ns/net")}, indent=2) + "\n")
        if any(isinstance(error, BaseException) for error in failures):
            raise FixtureFailed("peer shutdown failed; owned backend cleanup was still attempted")
