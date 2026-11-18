"""Pi provider program with socket-pair SDK fake core and child fake Pi; no providers/models."""
import asyncio
from copy import deepcopy
import json
import os
import random
from pathlib import Path
import sys
import tempfile
import threading
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "extensions"))
from pi_bridge.client import Limits as PiLimits
from pi_bridge.display import Fallback, Transcript, agent_data
from pi_bridge.program import Bridge, CAPABILITIES
from prim_provider import Limits, Provider, ProviderError
from test_prim_provider import Core


class BridgeCore(Core):
    def __init__(self, state=None, generation=7):
        self.state = state if state is not None else {"token": "bootstrap-once", "rotations": 0, "entries": {}}
        self.generation = generation
        self.resources = {}
        self.entries = self.state["entries"]
        self.registered = asyncio.Event()
        self.allow_register = asyncio.Event()
        self.allow_register.set()
        self.token = self.state["token"]
        self.drop_registration_reply = False
        self.refusal = None
        self.refusal_data = {}

    async def result(self, row):
        method, params = row["method"], row["params"]
        if method == "provider.register":
            self.registered.set()
            if self.refusal:
                raise ProviderError(self.refusal, "registration refused by fixture", data=self.refusal_data)
            if params.get("role") != "runtime" or not isinstance(params.get("token"), str) or params["token"] not in (self.state["token"], self.state.get("previous")):
                raise ProviderError("forbidden", "invalid or used bridge token")
            await self.allow_register.wait()
            self.name = params["name"]
            self.state["previous"] = params["token"]
            self.state["rotations"] += 1
            self.state["token"] = f"resume-{self.state['rotations']}"
            result = await super().result(row)
            result.update(generation=self.generation, resume_token=self.state["token"])
            if self.drop_registration_reply:
                self.writer.close()  # server rotated, client never observes this result
            return result
        elif method == "provider.sync":
            for facts in params["resources"]:
                self.upsert(facts)
        elif method == "workspace.snapshot":
            snapshot = {"resources": list(self.resources.values()), "entries": deepcopy(self.entries)}
            if len(json.dumps(snapshot).encode()) > 1048576:
                raise ProviderError("too_large", "reply exceeds recovery envelope")
            return snapshot
        elif method == "resource.list":
            return {"resources": [deepcopy(r) for r in self.resources.values()
                if (params.get("provider") is None or r.get("provider") == params["provider"])
                and (params.get("backend_ref") is None or r.get("backend_ref") == params["backend_ref"])]}
        elif method == "workspace.get":
            entry = deepcopy(self.entries.get(params["key"]))
            if len(json.dumps(entry).encode()) > 1048576:
                raise ProviderError("too_large", "reply exceeds recovery envelope")
            return entry
        elif method == "workspace.apply":
            for op in params["ops"]:
                if op.get("expect") == 0 and op["key"] in self.entries:
                    raise ProviderError("conflict", "view exists")
                self.entries[op["key"]] = {"value": deepcopy(op["value"]), "version": 1}
        return await super().result(row)

    def upsert(self, value):
        self.resources["r7"] = {**deepcopy(value), "id": "r7", "backend_ref": value["ref"],
                                "provider": self.name, "host": "r3", "available": True}

    def notification(self, row):
        if row["method"] == "resource.upsert":
            self.upsert(row["params"]["facts"])
        elif row["method"] == "resource.gone":
            self.resources.clear()

    def run_facts(self):
        return self.resources.get("r7", {}).get("data", {}).get("run", {})


class Frames:
    def __init__(self):
        self.frames = []
    def show(self, text):
        self.frames.append(text)
    def close(self):
        pass


class BridgeTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.root = tempfile.TemporaryDirectory(prefix="pi-provider-")
        self.core = BridgeCore()
        self.sock = await self.core.start()
        self.stop = asyncio.Event()
        self.frames = Frames()
        self.bridge = None
        self.task = None
        self.effects = 0
        self.extra_cores = []
        self.extra_sockets = []

    async def start(self, token="bootstrap-once", ready=True, transcript_bytes=16384, connector=None, interval=.1):
        provider = Provider("pi-test", "one-process", CAPABILITIES, role="runtime", token=token,
                            limits=Limits(deadline=1))
        env = {"HOME": self.root.name, "PATH": "/usr/bin:/bin", "LANG": "C.UTF-8",
               "HERDR_SOCKET_PATH": "must-not-inherit", "PRIM_BRIDGE_TOKEN": token,
               "PRIM_EXTENSION_LIFETIME": "stdin", "PRIM_EXTENSION": "wrong-parent"}
        self.bridge = Bridge([sys.executable, str(ROOT / "tests/fixtures/fake_pi.py")],
                             cwd=self.root.name, env=env, token=token, sock=self.sock,
                             provider=provider, fallback=self.frames, interval=interval,
                             transcript_bytes=transcript_bytes, pi_limits=PiLimits(deadline=.5),
                             connector=connector, retry_initial=.05, retry_max=.2)
        self.task = asyncio.create_task(self.bridge.run(self.stop))
        if ready:
            await asyncio.wait_for(self.bridge.ready.wait(), 3)
        return self.bridge

    async def asyncTearDown(self):
        self.stop.set()
        self.core.allow_register.set()
        if self.task is not None:
            await asyncio.wait_for(asyncio.gather(self.task, return_exceptions=True), 5)
            if self.bridge.pi is not None:
                self.assertIsNotNone(self.bridge.pi.process.returncode)
        else:
            self.sock.close()
        await self.core.close()
        self.sock.close()
        for core in self.extra_cores:
            await core.close()
        for sock in self.extra_sockets:
            sock.close()
        self.root.cleanup()

    async def effect(self, op, args=None):
        self.effects += 1
        return await self.core.call("effect.invoke", {"effect": f"e{self.effects}", "op": "agent." + op,
            "ref": "agent", "incarnation": "", "args": args or {}, "deadline_ms": 1000})

    async def test_startup_uses_scoped_reads_when_the_full_workspace_exceeds_provider_cap(self):
        for key in ("doc/unrelated/one", "doc/unrelated/two"):
            self.core.entries[key] = {"version": 1, "value": "x"*650000}
        self.core.resources["r99"] = {"id": "r99", "provider": "unrelated", "backend_ref": "agent", "kind": "agent"}
        bridge = await self.start()
        self.assertTrue(bridge.connected)
        self.assertEqual(bridge.rid, "r7")
        self.assertFalse(self.core.notes("workspace.snapshot"), "the provider wire must never need an aggregate snapshot")
        query = self.core.notes("resource.list")[0]["params"]
        self.assertEqual((query["provider"], query["backend_ref"]), ("pi-test", "agent"))
        self.assertEqual(self.core.notes("workspace.get")[0]["params"]["key"], "view/agent-r7")
        self.assertIsNone(bridge.pi.process.returncode)

    async def test_oversized_foreign_view_is_reported_without_register_sync_retry_storm(self):
        self.core.entries["view/agent-r7"] = {"version": 1, "value": {"kind": "panel", "items": [{"text": "x"*1100000}]}}
        bridge = await self.start()
        self.assertIsNone(bridge.view)
        self.assertIn("too_large", bridge.view_problem)
        self.assertEqual(len(self.core.notes("provider.register")), 1)
        self.assertEqual(len(self.core.notes("provider.sync")), 1)
        self.assertFalse(self.core.notes("view.publish"))
        await self.core.wait(lambda: "view_error" in self.core.resources["r7"]["data"])
        self.assertIn("view_error", self.core.resources["r7"]["data"])
        self.assertIsNone(bridge.pi.process.returncode)

    async def test_view_declaration_race_rechecks_only_that_entry_without_reregistering(self):
        original_entry = {"version": 1, "value": {"kind": "panel", "title": "Foreign", "items": []}}
        self.core.entries["view/agent-r7"] = deepcopy(original_entry)
        original_result = self.core.result
        reads = 0
        async def race(row):
            nonlocal reads
            if row["method"] == "workspace.get":
                reads += 1
                if reads == 1:
                    return None  # It appeared immediately after the first read.
            return await original_result(row)
        self.core.result = race
        bridge = await self.start()
        self.assertEqual(reads, 2)
        self.assertIsNone(bridge.view)
        self.assertEqual(self.core.entries["view/agent-r7"], original_entry)
        self.assertEqual(len(self.core.notes("provider.register")), 1)
        self.assertFalse(self.core.notes("view.publish"))

    async def test_user_effect_wakes_publication_without_waiting_for_sampling_timer(self):
        bridge = await self.start(interval=10)
        result = await self.effect("prompt", {"message": "wake"})
        self.assertEqual(result["result"]["outcome"], "accepted")
        await asyncio.wait_for(self.core.wait(lambda: self.core.run_facts().get("active") is True), .5)
        self.assertTrue(bridge.connected)

    async def test_failed_turn_panel_contains_bounded_error_rows_and_failed_status(self):
        bridge = await self.start()
        await self.effect("prompt", {"message": "fixture-model-error"})
        await self.core.wait(lambda: self.core.resources.get("r7", {}).get("status", {}).get("state") == "error")
        panel = bridge.content()
        errors = [row["text"] for row in panel["items"] if row.get("style") == "error"]
        self.assertTrue(any("ResourceNotFoundException" in row for row in errors), panel)
        self.assertIn("Retry 4/4", json.dumps(panel))
        self.assertIn("Failed", json.dumps(panel))
        self.assertEqual(self.core.resources["r7"]["data"]["retry"]["attempt"], 4)
        await bridge.pi.request("test_error_turn", {"message": "secret-control\x1b]evil\x07"+"x"*100000})
        await bridge.publish()
        encoded = json.dumps(bridge.content(), ensure_ascii=False).encode()
        self.assertLessEqual(len(encoded), 32768)
        self.assertNotIn(b"\\u001b", encoded)
        self.assertLessEqual(len(bridge.pi.snapshot()["failure"]["message"].encode()), 4096)

    async def test_readable_status_nonzero_counts_and_single_request_in_fallback(self):
        bridge = await self.start()
        content = json.dumps(bridge.content())
        self.assertIn("Ready for a prompt", content)
        self.assertNotIn("active=", content)
        self.assertNotIn("Queued steering", content)
        await bridge.pi.request("test_dialog", {"dialog_id": "q", "method": "confirm", "title": "Unique question"})
        bridge._show()
        self.assertEqual(self.frames.frames[-1].count("Unique question"), 1)
        await bridge.pi.invoke("steer", {"message": "queued"})
        self.assertIn("Queued steering: 1", json.dumps(bridge.content()))

    async def test_cli_explicit_argv_and_managed_stdin_eof_are_clean(self):
        await self.cli_lifetime_case(native_exit=False)

    async def test_cli_native_exit_with_open_stdin_is_clean(self):
        await self.cli_lifetime_case(native_exit=True)

    async def cli_lifetime_case(self, *, native_exit):
        core = BridgeCore()
        accepted = asyncio.Event()
        def connection(reader, writer):
            core.attach(reader, writer)
            accepted.set()
        endpoint = str(Path(self.root.name) / "p.sock")
        server = await asyncio.start_unix_server(connection, endpoint)
        env = {"HOME": self.root.name, "PATH": "/usr/bin:/bin", "LANG": "C.UTF-8",
               "PYTHONPATH": str(ROOT / "extensions"), "PRIM_SOCKET": endpoint,
               "PRIM_BRIDGE_TOKEN": "bootstrap-once", "PRIM_EXTENSION_LIFETIME": "stdin"}
        process = await asyncio.create_subprocess_exec(sys.executable, "-m", "pi_bridge", "--",
            sys.executable, str(ROOT / "tests/fixtures/fake_pi.py"), cwd=self.root.name, env=env,
            stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.DEVNULL, stderr=asyncio.subprocess.PIPE)
        try:
            await asyncio.wait_for(accepted.wait(), 2)
            self.extra_cores.append(core)
            await core.wait(lambda: bool(core.notes("view.publish")))
            if native_exit:
                await core.call("effect.invoke", {"effect": "exit-test", "op": "agent.prompt",
                    "ref": "agent", "incarnation": "", "args": {"message": "crash"}, "deadline_ms": 1000})
                # communicate() would close stdin and hide the daemon reader bug.
                await asyncio.wait_for(process.wait(), 4)
                stderr = await process.stderr.read()
            else:
                process.stdin.close()
                _, stderr = await asyncio.wait_for(process.communicate(), 3)
            self.assertEqual(process.returncode, 23 if native_exit else 0, stderr.decode())
            self.assertTrue(core.notes("resource.gone"), "EOF must run graceful bridge cleanup, not prim_ext os._exit")
            self.assertEqual(len(core.notes("provider.register")), 1)
        finally:
            if process.returncode is None:
                process.stdin.close()
                try:
                    await asyncio.wait_for(process.wait(), 2)
                except TimeoutError:
                    process.kill()
                    await process.wait()
            if accepted.is_set():
                await core.close()
            server.close()
            await asyncio.wait_for(server.wait_closed(), 1)

    async def test_register_token_host_view_and_no_secret_in_child_environment(self):
        bridge = await self.start()
        register = self.core.notes("provider.register")[0]["params"]
        self.assertEqual(register["token"], "bootstrap-once")
        self.assertEqual(register["role"], "runtime")
        self.assertEqual(len(self.core.notes("provider.register")), 1)
        resource = self.core.resources["r7"]
        self.assertEqual((resource["kind"], resource["host"]), ("agent", "r3"))
        self.assertEqual(resource["status"]["state"], "idle")
        self.assertIsNone(resource["data"]["run"]["settled"])
        self.assertEqual(self.core.entries["view/agent-r7"]["value"]["kind"], "panel")
        self.assertEqual(self.core.notes("view.publish")[0]["params"]["view"], "agent-r7")
        self.assertNotIn("PRIM_BRIDGE_TOKEN", bridge.env)
        self.assertNotIn("PRIM_EXTENSION_LIFETIME", bridge.env)
        self.assertFalse(any(k.startswith("HERDR_") for k in bridge.env))

    async def test_prompt_accepted_steer_during_run_and_native_settled(self):
        bridge = await self.start()
        result = await self.effect("prompt", {"message": "start"})
        self.assertEqual(result["result"], {"outcome": "accepted", "data": {"disposition": "started"}})
        await self.core.wait(lambda: self.core.run_facts().get("active") is True)
        self.assertFalse(self.core.run_facts()["settled"])
        result = await self.effect("steer", {"message": "new direction"})
        self.assertEqual(result["result"]["data"]["disposition"], "queued")
        await self.core.wait(lambda: self.core.resources["r7"]["data"]["queues"]["steering"] == ["new direction"])
        await bridge.pi.request("test_boundary")
        await bridge.pi.request("test_end")
        self.assertTrue(bridge.pi.snapshot()["run"]["active"])
        await bridge.pi.request("test_settle")
        await self.core.wait(lambda: self.core.run_facts().get("settled") is True)
        self.assertEqual(self.core.resources["r7"]["status"]["state"], "done")

    async def test_dialog_answer_is_dispatched_and_requests_are_complete(self):
        bridge = await self.start()
        await bridge.pi.request("test_dialog", {"dialog_id": "dialog", "method": "confirm", "message": "Full confirmation body"})
        await self.core.wait(lambda: self.core.resources["r7"]["status"]["state"] == "blocked")
        req = self.core.resources["r7"]["data"]["requests"][0]
        self.assertEqual(req["data"]["message"], "Full confirmation body")
        reply = await self.effect("answer", {"id": "dialog", "confirmed": True})
        self.assertEqual(reply["result"]["outcome"], "dispatched")
        await bridge.pi.request("get_state")
        observed = [x for x in bridge.pi.events()["events"] if x["type"] == "test_answer_observed"]
        self.assertEqual(observed[0]["answer"]["confirmed"], True)

    async def test_abort_does_not_silently_clear_native_queue(self):
        bridge = await self.start()
        await self.effect("prompt", {"message": "start"})
        await self.effect("follow_up", {"message": "still queued"})
        await self.effect("abort")
        records = bridge.pi.events()["events"]
        self.assertTrue(any(x.get("message", {}).get("content") == "still queued" for x in records))
        self.assertFalse(any(x.get("command") == "clear_queue" for x in records))
        await self.effect("prompt", {"message": "again"})
        await self.effect("steer", {"message": "remove explicitly"})
        cleared = await self.effect("clear_queue")
        self.assertEqual(cleared["result"]["data"]["steering"], ["remove explicitly"])
        await self.effect("abort")

    async def test_child_crash_is_backend_uncertainty_then_resource_gone(self):
        await self.start()
        result = await self.effect("prompt", {"message": "crash"})
        self.assertIn(result["error"]["data"]["kind"], ("backend", "unavailable"))
        ended = await asyncio.wait_for(self.task, 3)
        self.assertEqual(ended, {"reason": "exited", "exit_code": 23})
        await self.core.wait(lambda: bool(self.core.notes("resource.gone")))
        self.assertEqual(self.core.notes("resource.gone")[0]["params"]["reason"]["exit_code"], 23)
        self.assertFalse(self.core.resources)

    async def test_core_disconnect_preserves_pi_when_endpoint_is_unavailable(self):
        bridge = await self.start()
        self.core.writer.close()
        await asyncio.wait_for(bridge.provider.wait_closed(), 1)
        self.assertIsNone(bridge.pi.process.returncode)
        self.assertFalse(self.task.done())
        await bridge.publish()
        self.assertFalse(bridge.connected)
        self.assertEqual(len(self.core.notes("provider.register")), 1)
        self.assertIn("core unavailable", self.frames.frames[-1])

    async def test_resume_rotates_token_syncs_current_run_and_never_replays_input(self):
        other = BridgeCore(self.core.state, generation=8)
        other_sock = await other.start()
        self.extra_cores.append(other)
        self.extra_sockets.append(other_sock)
        sockets = [self.sock, other_sock]
        async def connector(provider):
            if not sockets:
                raise ConnectionError("core offline")
            await provider.connect(sock=sockets.pop(0))
        bridge = await self.start(connector=connector)
        child = bridge.pi.process.pid
        await self.effect("prompt", {"message": "start"})
        await self.core.wait(lambda: self.core.run_facts().get("active") is True)
        old = bridge.provider
        self.core.writer.close()
        await asyncio.wait_for(old.wait_closed(), 1)
        await other.wait(lambda: bool(other.notes("provider.sync")) and bool(other.notes("view.publish")))
        self.assertEqual(bridge.pi.process.pid, child)
        self.assertIsNone(bridge.pi.process.returncode)
        self.assertTrue(other.run_facts()["active"])
        register = other.notes("provider.register")[0]["params"]
        self.assertEqual(register["token"], "resume-1")
        self.assertEqual(bridge.provider.registration["resume_token"], "resume-2")
        self.assertEqual(register["name"], self.core.name)
        self.assertEqual(register["instance"], "one-process")
        self.assertFalse(other.notes("workspace.apply"), "owned view should be reused, not rewritten")
        accepted = await bridge.pi.request("test_accepted")
        self.assertEqual(accepted["data"]["accepted"], ["start"])
        self.assertNotIn("resume-2", json.dumps(other.resources))
        self.assertNotIn("resume-2", json.dumps(bridge.env))

    async def test_lost_registration_reply_reuses_last_observed_credential_with_grace(self):
        other = BridgeCore(state=self.core.state, generation=8)
        other_sock = await other.start()
        self.extra_cores.append(other)
        self.extra_sockets.append(other_sock)
        self.core.drop_registration_reply = True
        available = [(self.core, self.sock), (other, other_sock)]
        async def connect(provider):
            if not available:
                raise ConnectionError("test endpoint unavailable")
            _, sock = available.pop(0)
            await provider.connect(sock=sock)
        bridge = await self.start(connector=connect)
        self.assertEqual(other.notes("provider.register")[0]["params"]["token"], "bootstrap-once")
        self.assertEqual(bridge.provider.registration["resume_token"], "resume-2")
        self.assertIsNone(bridge.pi.process.returncode)
        self.assertEqual(len(self.core.notes("provider.register")), 1)
        self.assertEqual(len(other.notes("provider.register")), 1)

    async def test_backoff_is_bounded_and_shutdown_interrupts_retry(self):
        attempts = []
        async def offline(provider):
            attempts.append(asyncio.get_running_loop().time())
            raise ConnectionError("offline")
        with mock.patch("pi_bridge.program.random.uniform", side_effect=lambda low, high: high):
            bridge = await self.start(ready=False, connector=offline)
            async def retries():
                while len(attempts) < 3:
                    await asyncio.sleep(.01)
            await asyncio.wait_for(retries(), 1)
        self.assertGreaterEqual(attempts[1] - attempts[0], .045)
        self.assertGreaterEqual(attempts[2] - attempts[1], .09)
        self.assertIsNone(bridge.pi.process.returncode)
        self.stop.set()
        await asyncio.wait_for(self.task, 2)

    async def test_explicit_safe_mode_refusal_preserves_running_pi_then_resumes_without_replay(self):
        blocked = BridgeCore(self.core.state, generation=8)
        blocked.refusal, blocked.refusal_data = "forbidden", {"reason": "safe_mode", "outcome": "rejected"}
        resumed = BridgeCore(self.core.state, generation=9)
        sockets = [self.sock]
        for core in (blocked, resumed):
            sock = await core.start()
            self.extra_cores.append(core); self.extra_sockets.append(sock); sockets.append(sock)
        async def connector(provider):
            await provider.connect(sock=sockets.pop(0))
        bridge = await self.start(connector=connector)
        pid = bridge.pi.process.pid
        await self.effect("prompt", {"message": "start"})
        await self.core.wait(lambda: self.core.run_facts().get("active") is True)
        self.core.writer.close()
        await asyncio.wait_for(blocked.registered.wait(), 2)
        await asyncio.wait_for(resumed.registered.wait(), 2)
        await asyncio.wait_for(bridge.ready.wait(), 2)
        self.assertEqual(bridge.pi.process.pid, pid)
        self.assertIsNone(bridge.pi.process.returncode)
        self.assertTrue(resumed.run_facts()["active"])
        self.assertEqual(len(blocked.notes("provider.register")), 1)
        accepted = await bridge.pi.request("test_accepted")
        self.assertEqual(accepted["data"]["accepted"], ["start"])

    async def test_briefly_accepted_flapping_links_do_not_reset_reconnect_backoff(self):
        others = [BridgeCore(self.core.state, generation=g) for g in (8, 9)]
        sockets = [self.sock]
        for core in others:
            sock = await core.start()
            self.extra_cores.append(core); self.extra_sockets.append(sock); sockets.append(sock)
        attempted = []
        async def connector(provider):
            attempted.append(asyncio.get_running_loop().time())
            await provider.connect(sock=sockets.pop(0))
        with mock.patch("pi_bridge.program.random.uniform", side_effect=lambda low, high: high):
            bridge = await self.start(connector=connector)
            delays = []
            for previous, following in zip([self.core, *others], others):
                closed = asyncio.get_running_loop().time()
                previous.writer.close()
                await asyncio.wait_for(following.registered.wait(), 2)
                await asyncio.wait_for(bridge.ready.wait(), 2)
                delays.append(attempted[-1]-closed)
            self.assertGreaterEqual(delays[0], .045)
            self.assertGreaterEqual(delays[1], .09, "a short successful registration must not restart the fast retry loop")
            self.assertEqual(bridge.connections, 3)
            self.assertIsNone(bridge.pi.process.returncode)

    async def test_lifetime_stdin_eof_closes_child_and_publishes_gone(self):
        from pi_bridge.__main__ import watch_stdin
        bridge = await self.start()
        read_fd, write_fd = os.pipe()
        with os.fdopen(read_fd, "rb", buffering=0) as reader:
            watcher = watch_stdin(reader, asyncio.get_running_loop(), self.stop)
            os.close(write_fd)
            result = await asyncio.wait_for(self.task, 3)
            watcher.join(.1)
        self.assertEqual(result["reason"], "closed")
        self.assertEqual(bridge.pi.process.returncode, 0)
        await self.core.wait(lambda: bool(self.core.notes("resource.gone")))

    async def test_registration_waits_once_without_replaying(self):
        self.core.allow_register.clear()
        bridge = await self.start(ready=False)
        await asyncio.wait_for(self.core.registered.wait(), 1)
        self.assertFalse(bridge.ready.is_set())
        self.assertEqual(len(self.core.notes("provider.register")), 1)
        self.core.allow_register.set()
        await asyncio.wait_for(bridge.ready.wait(), 2)
        self.assertEqual(len(self.core.notes("provider.register")), 1)

    async def refused_bridge_stops_owned_runtime(self, kind):
        self.core.refusal = kind
        bridge = await self.start(ready=False)
        await asyncio.wait_for(self.core.registered.wait(), 1)
        result = await asyncio.wait_for(asyncio.gather(self.task, return_exceptions=True), 2)
        self.assertIsInstance(result[0], ProviderError)
        self.assertIsNotNone(bridge.pi.process.returncode)
        self.assertFalse(bridge.ready.is_set())
        self.assertEqual(len(self.core.notes("provider.register")), 1, "definitive refusal is never retried")
        self.assertFalse(self.core.resources)
        self.assertIn("registration refused", "\n".join(self.frames.frames))
        self.assertFalse(self.core.notes("effect.invoke"))

    async def test_forbidden_registration_exits_and_stops_its_pi(self):
        await self.refused_bridge_stops_owned_runtime("forbidden")

    async def test_registry_overload_exits_and_stops_its_pi(self):
        await self.refused_bridge_stops_owned_runtime("overloaded")

    async def test_explicit_unavailable_reply_is_not_mistaken_for_a_transport_gap(self):
        await self.refused_bridge_stops_owned_runtime("unavailable")

    async def test_missing_resume_credential_cannot_leave_an_unreachable_runtime_alive(self):
        original = self.core.result
        async def omit_resume(row):
            result = await original(row)
            if row["method"] == "provider.register":
                result.pop("resume_token", None)
            return result
        self.core.result = omit_resume
        bridge = await self.start(ready=False)
        result = await asyncio.wait_for(asyncio.gather(self.task, return_exceptions=True), 2)
        self.assertIsInstance(result[0], ProviderError)
        self.assertIsNotNone(bridge.pi.process.returncode)
        self.assertEqual(len(self.core.notes("provider.register")), 1)
        self.assertIn("no resume credential", str(result[0]))

    async def test_view_collision_is_not_overwritten_or_published(self):
        original = {"value": {"kind": "panel", "title": "unrelated"}, "version": 1}
        self.core.entries["view/agent-r7"] = deepcopy(original)
        bridge = await self.start()
        self.assertEqual(self.core.entries["view/agent-r7"], original)
        self.assertFalse(self.core.notes("view.publish"))
        self.assertIsNotNone(bridge.view_problem)
        self.assertTrue(bridge.connected)
        self.stop.set()
        await asyncio.wait_for(self.task, 3)
        await self.core.wait(lambda: bool(self.core.notes("resource.gone")))

    async def test_transcript_is_bounded_sanitized_and_coalesced(self):
        bridge = await self.start(transcript_bytes=512)
        before = len(self.core.notes("view.publish"))
        await bridge.pi.request("test_transcript", {"count": 100, "text": "word\x1b]evil\x07" * 100, "final": "FINAL"})
        await self.core.wait(lambda: any("FINAL" in item["text"] for row in self.core.notes("view.publish")[before:]
                                       for item in row["params"]["content"]["items"]))
        views = self.core.notes("view.publish")[before:]
        self.assertLessEqual(len(views), 3)
        self.assertTrue(views[-1]["params"]["content"]["truncated"])
        self.assertLessEqual(len(json.dumps(views[-1]["params"]["content"], ensure_ascii=False).encode()), 32768)
        self.assertNotIn("\x1b", "".join(item["text"] for item in views[-1]["params"]["content"]["items"]))

    async def test_oversized_dialog_cancelled_once_instead_of_invisible_wait(self):
        bridge = await self.start()
        await bridge.pi.request("test_dialog", {"dialog_id": "large", "method": "confirm", "message": "x" * 70000})
        await bridge.pi.wait_for_facts(lambda f: not f["requests"], timeout=2)
        await bridge.pi.request("get_state")
        observed = [x for x in bridge.pi.events()["events"] if x["type"] == "test_answer_observed"]
        self.assertEqual(len(observed), 1)
        self.assertTrue(observed[0]["answer"]["cancelled"])


class DisplayTests(unittest.TestCase):
    def test_retry_jitter_retains_a_floor_spread_and_strict_cap(self):
        bridge = Bridge(["unused"], cwd="unused", env={}, token="test", fallback=Frames())
        self.assertEqual(bridge.retry_max, 30)
        rng = random.Random(123)
        with mock.patch("pi_bridge.program.random.uniform", side_effect=rng.uniform):
            values = [bridge._retry_delay(bridge.retry_max) for _ in range(100)]
        self.assertTrue(all(15 <= value <= 30 for value in values))
        self.assertGreater(max(values)-min(values), 5, "capped retries must still have jitter")

    def test_projection_marks_unrepresentable_fields_unknown(self):
        snapshot = {"run": {"active": True, "settled": False}, "queues": {"steering": ["x" * 100000], "follow_up": []},
                    "requests": [{"id": "q", "kind": "confirm", "prompt": "question", "data": {"message": "body"}}]}
        data = agent_data(snapshot)
        self.assertIsNone(data["queues"])
        self.assertEqual(data["incomplete"], ["queues"])
        self.assertEqual(data["requests"][0]["data"]["message"], "body")
        self.assertLess(len(json.dumps(data).encode()), 65536)

    def test_empty_error_message_is_visible_and_semantically_styled(self):
        transcript = Transcript(512)
        transcript.feed({"type": "message_end", "message": {"role": "assistant", "content": [],
                         "stopReason": "error", "errorMessage": "Fixture failure"}})
        rows, _, _ = transcript.window(styled=True)
        self.assertTrue(any(row.get("style") == "error" and "Fixture failure" in row["text"] for row in rows))

    def test_event_gap_discards_partial_and_authoritative_message_replaces_delta(self):
        transcript = Transcript(512)
        transcript.feed({"type": "message_update", "assistantMessageEvent": {"type": "text_delta", "delta": "partial"}})
        transcript.gap()
        transcript.feed({"type": "message_end", "message": {"role": "assistant", "content": [{"type": "text", "text": "final"}]}})
        rows, _, lost = transcript.window()
        self.assertTrue(lost)
        self.assertIn("assistant> final", rows)
        self.assertNotIn("assistant> partial", rows)

    def test_stalled_fallback_is_bounded_and_does_not_block_caller(self):
        read_fd, write_fd = os.pipe()
        fallback = Fallback(write_fd)
        try:
            for _ in range(100):
                fallback.show("x" * 4000)
            self.assertLessEqual(fallback.frames.qsize(), 1)
            fallback.close()
        finally:
            os.close(read_fd)
            fallback.thread.join(.2)
            os.close(write_fd)


if __name__ == "__main__":
    unittest.main()
