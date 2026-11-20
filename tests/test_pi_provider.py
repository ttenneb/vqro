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
from vqro_provider import Limits, Provider, ProviderError
from test_vqro_provider import Core


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

    async def start(self, token="bootstrap-once", ready=True, transcript_bytes=16384, connector=None, interval=.1,
                    env_extra=None, fixture_args=()):
        provider = Provider("pi-test", "one-process", CAPABILITIES, role="runtime", token=token,
                            principal=(env_extra or {}).get("VQRO_PRINCIPAL"), limits=Limits(deadline=1))
        env = {"HOME": self.root.name, "PATH": "/usr/bin:/bin", "LANG": "C.UTF-8",
               "HERDR_SOCKET_PATH": "must-not-inherit", "VQRO_BRIDGE_TOKEN": token,
               "VQRO_EXTENSION_LIFETIME": "stdin", "VQRO_EXTENSION": "wrong-parent"}
        env.update(env_extra or {})
        self.bridge = Bridge([sys.executable, str(ROOT / "tests/fixtures/fake_pi.py"), *fixture_args],
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

    async def test_default_bridge_bounds_recent_effect_history_without_changing_sdk_default(self):
        bridge = Bridge(["not-executed"], cwd=self.root.name, env={}, token="test", fallback=self.frames)
        self.assertEqual(bridge.provider.limits.identities, 2048)
        self.assertEqual(Limits().identities, 65536)
        ledger = bridge.provider._effect_ids
        for i in range(12000):
            ledger.add(str(i), bridge.provider.limits.identities)
            ledger.finish(str(i))
        self.assertEqual(len(ledger), 2048)
        self.assertIn("11999", ledger)
        self.assertNotIn("0", ledger)
        with self.assertRaises(ProviderError):
            ledger.add("11999", bridge.provider.limits.identities)

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
        self.assertFalse([row for row in self.core.notes("view.publish") if row["params"]["view"] == "agent-r7"])
        self.assertEqual(bridge.status_view, "agent-status-r7")
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
        self.assertEqual(reads, 3)  # two collision reads for output, one independent status entry
        self.assertIsNone(bridge.view)
        self.assertEqual(self.core.entries["view/agent-r7"], original_entry)
        self.assertEqual(len(self.core.notes("provider.register")), 1)
        self.assertFalse([row for row in self.core.notes("view.publish") if row["params"]["view"] == "agent-r7"])

    async def test_queue_status_changes_do_not_republish_the_transcript_view(self):
        bridge = await self.start()
        await bridge.publish()
        before = len([r for r in self.core.notes("view.publish") if r["params"]["view"] == bridge.view])
        result = await self.effect("steer", {"message": "queued status only"})
        self.assertEqual(result["result"]["outcome"], "accepted")
        await bridge.publish()
        self.assertEqual(len([r for r in self.core.notes("view.publish") if r["params"]["view"] == bridge.view]), before)
        self.assertIn("queued status only", json.dumps(bridge._status_content))
        self.assertNotIn("queued status only", json.dumps(bridge._content))
        self.assertEqual(self.core.resources["r7"]["data"]["views"], ["agent-status-r7", "agent-r7"])

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
               "VQRO_SOCKET": endpoint,
               "VQRO_BRIDGE_TOKEN": "bootstrap-once", "VQRO_EXTENSION_LIFETIME": "stdin"}
        process = await asyncio.create_subprocess_exec(sys.executable, "-I", str(ROOT / "extensions/pi_bridge/launch.py"), "--",
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
            self.assertTrue(core.notes("resource.gone"), "EOF must run graceful bridge cleanup, not vqro_ext os._exit")
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

    async def test_runtime_environment_preserves_user_path_but_not_bridge_imports_or_supervision(self):
        user=Path(self.root.name)/"user-modules";user.mkdir()
        capture=Path(self.root.name)/"child-env.json"
        bridge=await self.start(env_extra={"PYTHONPATH":os.pathsep.join([str(ROOT/"extensions"),str(user)]),
            "VQRO_SOCKET":"private-terminal-endpoint","VQRO_PRINCIPAL":"alice","VQRO_STATE_DIR":"core-only","VQRO_EXTENSIONS_DIR":"core-only",
            "VQRO_EXTENSION_TOKEN":"supervised-secret","VQRO_OTHER_INTERNAL":"core-only"},fixture_args=("--env-file",str(capture)))
        record=json.loads(capture.read_text());child=record["env"]
        self.assertEqual(child.get("PYTHONPATH"),str(user))
        self.assertIsNone(record["settings"],"agent tools must not discover vqro's settings extension")
        self.assertEqual({k for k in child if k.startswith("VQRO_")},{"VQRO_SOCKET","VQRO_SOCKET_SOURCE","VQRO_PRINCIPAL"})
        self.assertEqual(child["VQRO_PRINCIPAL"],"alice")
        self.assertEqual(self.core.notes("hello")[0]["params"]["identity"],{"kind":"principal","principal":"alice"})
        self.assertEqual(child["VQRO_SOCKET_SOURCE"],"terminal")
        self.assertEqual(child["VQRO_SOCKET"],"private-terminal-endpoint")
        self.assertFalse(any(k.startswith("HERDR_") for k in child))

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
        self.assertNotIn("VQRO_BRIDGE_TOKEN", bridge.env)
        self.assertNotIn("VQRO_EXTENSION_LIFETIME", bridge.env)
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

    async def test_real_pi_metadata_sizes_keep_the_runtime_alive(self):
        """Regression (real pi1 1.5.2): get_available_models is ~300 KB with a real configuration. A 256 KiB
        record cap made the client treat it as a protocol error and terminate Pi (exit 143) 0.5 s after start."""
        recorded = ROOT / "tests/fixtures/pi_metadata_1.5.2.json"
        models = json.loads(recorded.read_text())["get_available_models"]
        self.assertGreater(len(json.dumps({"type": "response", "data": models}).encode()), 262144, "the fixture must exercise a >256 KiB record")
        bridge = await self.start(fixture_args=("--metadata", str(recorded)))
        agent = lambda: self.core.resources.get("r7", {}).get("data", {}).get("agent") or {}
        await self.core.wait(lambda: agent().get("config"))
        options = {c["id"]: c for c in agent()["config"]}
        self.assertEqual(options["model"]["value"], "openai-codex/gpt-6-luna")
        self.assertEqual(len(options["model"]["choices"]), 64, "bounded choices from the full list")
        self.assertEqual(len(options["thinking_level"]["choices"]), 7)
        self.assertGreater(len(agent()["commands"]), 10, "Pi's own commands are advertised")
        self.assertTrue(bridge.pi.snapshot()["available"], bridge.pi.diagnostics()["failure"])
        self.assertIsNone(bridge.pi.process.returncode, "Pi must not be terminated by metadata")
        self.assertLess(len(json.dumps(self.core.resources["r7"]["data"]).encode()), 65536, "facts stay within the 64 KiB data bound")
        self.assertEqual((await self.effect("prompt", {"message": "still works"}))["result"]["outcome"], "accepted")

    async def test_commands_and_config_follow_agent_v0_section_6_3(self):
        bridge = await self.start()
        agent = lambda: self.core.resources.get("r7", {}).get("data", {}).get("agent") or {}
        options = lambda: {c["id"]: c for c in agent().get("config") or []}
        await self.core.wait(lambda: options() and len(agent()["commands"]) > 3)
        self.assertEqual(agent()["v"], 0)
        self.assertNotIn("config", self.core.resources["r7"]["data"], "the lists live under data.agent only")
        self.assertEqual([c["name"] for c in agent()["commands"]], ["new_session", "compact", "cycle_model", "fixture-template", "skill:fixture"])
        self.assertTrue(all(set(c) <= {"name", "description", "input"} for c in agent()["commands"]))
        model = options()["model"]
        self.assertEqual((model["category"], model["type"], model["value"], model["rev"]), ("model", "select", "fixture/fake-small", 1))
        self.assertEqual([c["value"] for c in model["choices"]], ["fixture/fake-small", "fixture/fake-large"])
        self.assertEqual(options()["thinking_level"]["category"], "thought_level")
        self.assertEqual((options()["auto_compaction"]["type"], options()["auto_compaction"]["value"]), ("boolean", True))

        # Stale rev or a value outside the choices: rejected before any native write.
        for args, kind in (({"v": 0, "id": "model", "value": "fixture/fake-large", "rev": 0}, "stale"),
                           ({"v": 0, "id": "model", "value": "nobody/unknown"}, "unsupported"),
                           ({"v": 0, "id": "temperature", "value": 1}, "unsupported"),
                           ({"v": 0, "id": "auto_compaction", "value": "yes"}, "unsupported"),
                           ({"v": 1, "id": "auto_compaction", "value": False}, "unsupported")):
            result = (await self.effect("configure", args))["result"]
            self.assertEqual((result["outcome"], result["error"]["kind"]), ("rejected", kind), args)
        self.assertEqual(options()["model"]["value"], "fixture/fake-small", "nothing changed")

        result = (await self.effect("configure", {"v": 0, "id": "model", "value": "fixture/fake-large", "rev": 1}))["result"]
        self.assertEqual(result, {"outcome": "observed", "data": {"id": "model", "value": "fixture/fake-large"}})
        await self.core.wait(lambda: options()["model"]["value"] == "fixture/fake-large")
        self.assertEqual(options()["model"]["rev"], 2, "rev bumps when the value changes")
        self.assertEqual([c["value"] for c in options()["thinking_level"]["choices"]], ["off", "low", "high"], "choices follow the model")
        stale = (await self.effect("configure", {"v": 0, "id": "model", "value": "fixture/fake-small", "rev": 1}))["result"]
        self.assertEqual(stale["error"]["kind"], "stale")
        self.assertEqual((await self.effect("configure", {"v": 0, "id": "auto_compaction", "value": False}))["result"]["outcome"], "observed")

        result = (await self.effect("command", {"v": 0, "name": "rm_rf"}))["result"]
        self.assertEqual((result["outcome"], result["error"]["kind"]), ("rejected", "unsupported"))
        result = (await self.effect("command", {"v": 0, "name": "compact", "args": {"not": "a string"}}))["result"]
        self.assertEqual(result["error"]["kind"], "unsupported", "args is a string")
        self.assertEqual((await self.effect("command", {"v": 0, "name": "compact", "args": "keep the plan"}))["result"]["outcome"], "accepted")
        self.assertEqual((await self.effect("command", {"v": 0, "name": "cycle_model"}))["result"], {"outcome": "accepted", "data": {"model": "fixture/fake-small"}})
        # Pi's own commands run as prompt text only through agent.command; a plain send stays literal.
        self.assertEqual((await self.effect("command", {"v": 0, "name": "fixture-template", "args": "for tests"}))["result"]["outcome"], "accepted")
        accepted = (await bridge.pi.request("test_accepted"))["data"]["accepted"]
        self.assertEqual(accepted[-1], "/fixture-template for tests")
        await bridge.pi.request("test_settle")
        for message in ("/fixture-template sneaky", "/skill:anything"):
            result = (await self.effect("prompt", {"message": message}))["result"]
            self.assertEqual((result["outcome"], result["error"]["kind"]), ("rejected", "unsupported"), message)
        self.assertEqual((await bridge.pi.request("test_accepted"))["data"]["accepted"], accepted, "nothing sent")
        self.assertEqual((await self.effect("prompt", {"message": "/usr/bin/env is a path, not a command"}))["result"]["outcome"], "accepted")
        self.assertEqual((await self.effect("command", {"v": 0, "name": "new_session"}))["result"]["outcome"], "accepted")

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

    async def test_pi_exit_retires_the_agent_before_cancelling_an_in_flight_publication(self):
        # The publisher is mid-publication (its view.publish is unanswered) when Pi exits. Cleanup
        # must send resource.gone first and only then cancel the publisher, so a busy core can never
        # turn the publisher's cancellation into a lost retirement (C2 gate e2e-agents-ui).
        bridge = await self.start(interval=10)
        seen = {}
        original = self.core.notification
        def notification(row):
            if row["method"] == "resource.gone":
                # The publisher's unanswered view.publish is still pending: it was not cancelled first.
                seen["publisher_running"] = any(m == "view.publish" for m, _ in bridge.provider._pending.values())
            original(row)
        self.core.notification = notification
        self.core.hold.add("view.publish")
        held = len(self.core.notes("view.publish"))
        await self.effect("prompt", {"message": "start"})
        await self.core.wait(lambda: len(self.core.notes("view.publish")) > held)
        bridge.pi.process.kill()
        ended = await asyncio.wait_for(self.task, 3)
        self.assertEqual(ended["reason"], "exited")
        await self.core.wait(lambda: bool(self.core.notes("resource.gone")))
        self.assertEqual(seen, {"publisher_running": True})
        self.assertFalse(self.core.resources)
        self.assertIsNone(bridge.retirement_problem)

    async def test_pi_exit_during_registration_retires_without_publishing_a_live_agent(self):
        # Resuming after a core restart: vqro still caches the agent (unavailable) while the bridge
        # re-registers. Pi exits before the registration reply. Shutdown must wait for setup, never
        # let setup sync a live agent once closing, and retire the cached agent on the registered
        # connection (it used to skip retirement silently because the generation was unknown).
        self.core.resources["r7"] = {"ref": "agent", "kind": "agent", "state": "live", "id": "r7",
                                     "backend_ref": "agent", "provider": "pi-test", "available": False}
        self.core.allow_register.clear()
        bridge = await self.start(ready=False)
        await asyncio.wait_for(self.core.registered.wait(), 3)
        bridge.pi.process.kill()
        await asyncio.sleep(.3)  # cleanup is now waiting for setup to reach a checkpoint
        self.core.allow_register.set()
        ended = await asyncio.wait_for(self.task, 5)
        self.assertEqual(ended["reason"], "exited")
        await self.core.wait(lambda: bool(self.core.notes("resource.gone")))
        self.assertEqual(self.core.notes("provider.sync"), [], "no live agent published while closing")
        self.assertEqual(self.core.notes("resource.upsert"), [])
        self.assertFalse(self.core.resources)
        self.assertIsNone(bridge.retirement_problem)

    async def test_a_close_during_a_stalled_publication_does_not_stop_the_bridge_or_pi(self):
        # A core stalled past a write deadline: control's finally closes the provider while the publisher's
        # write is still flushing. The publisher must see an unavailable connection and the bridge reconnect;
        # it must not end as cancelled, which run() treats as "bridge cancelled" and closes Pi.
        bridge = await self.start()
        provider = bridge.provider
        stalled = asyncio.get_running_loop().create_future()
        async def never_drains():
            await stalled
        provider.writer.drain = never_drains
        bridge._facts = None  # the next publication upserts
        bridge._publish_now.set()
        await asyncio.sleep(.3)
        self.assertTrue(provider._flushes, "the publisher must be blocked in a flush for this test to mean anything")
        await provider.close()
        await asyncio.sleep(.5)
        self.assertFalse(bridge._publishing.done(), "publisher survived the closed connection")
        self.assertFalse(self.task.done(), "bridge still running")
        self.assertIsNone(bridge.pi.process.returncode, "Pi was not stopped")
        self.assertFalse(bridge.connected)  # reported as a lost connection; control retries with backoff

    async def test_undeliverable_retirement_is_retried_once_then_reported(self):
        class Stub:
            def __init__(self, failures):
                self.generation, self.failure, self.calls, self.failures = 7, None, 0, failures
            async def gone(self, ref, why=None):
                self.calls += 1
                if self.calls <= self.failures:
                    raise ProviderError("timeout", "answer deadline; delivery unknown")
        def bare(provider, rid="r7"):
            bridge = Bridge.__new__(Bridge)
            bridge.provider, bridge._setup, bridge.rid, bridge.connections = provider, asyncio.Lock(), rid, 1
            bridge.failure = bridge.retirement_problem = None
            return bridge
        for failures, calls, reported in ((1, 2, False), (2, 2, True)):
            bridge = bare(Stub(failures))
            problem = await Bridge._retire(bridge, {"reason": "exited", "exit_code": 23})
            self.assertEqual(bridge.provider.calls, calls)
            self.assertEqual(problem is not None, reported)
            if reported:
                # A timed-out write may have reached the core: unconfirmed, never "not delivered".
                self.assertIn("delivery unconfirmed", bridge.failure)
                self.assertEqual(bridge.retirement_problem, problem)
        # A disconnected provider is reported at once, without pretending to send.
        stub = Stub(0)
        stub.failure = "provider connection closed"
        bridge = bare(stub)
        problem = await Bridge._retire(bridge, {"reason": "exited", "exit_code": 23})
        self.assertEqual(bridge.provider.calls, 0)
        self.assertIn("provider connection closed", problem)
        self.assertIn("not delivered", bridge.failure)
        # A published agent whose provider never registered (again) is reported, not skipped.
        stub = Stub(0)
        stub.generation = None
        bridge = bare(stub)
        self.assertIn("not registered", await Bridge._retire(bridge, {"reason": "exited"}))
        bridge = bare(Stub(0), rid=None)
        bridge.provider.generation, bridge.connections = None, 0
        self.assertIsNone(await Bridge._retire(bridge, {"reason": "exited"}), "nothing was ever published")

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
        self.assertFalse([row for row in self.core.notes("view.publish") if row["params"]["view"] == "agent-r7"])
        self.assertIsNotNone(bridge.view_problem)
        self.assertTrue(bridge.connected)
        self.stop.set()
        await asyncio.wait_for(self.task, 3)
        await self.core.wait(lambda: bool(self.core.notes("resource.gone")))

    async def test_idle_bridge_does_not_rebuild_unchanged_content(self):
        bridge=await self.start()
        with mock.patch.object(bridge,"content",wraps=bridge.content) as content:
            await asyncio.sleep(.35)
            content.assert_not_called()

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

    def test_idle_fallback_blocks_without_a_poll_timeout(self):
        import queue
        read_fd,write_fd=os.pipe()
        frames=queue.Queue(maxsize=1)
        entered=threading.Event();get=frames.get
        def blocking(*args,**kwargs):
            entered.set()
            return get(*args,**kwargs)
        frames.get=mock.Mock(side_effect=blocking)
        with mock.patch("pi_bridge.display.queue.Queue",return_value=frames):
            fallback=Fallback(write_fd)
        try:
            self.assertTrue(entered.wait(1))
            self.assertEqual(frames.get.call_args,mock.call())
            fallback.close()
            self.assertFalse(fallback.thread.is_alive())
        finally:
            fallback.close();os.close(read_fd);os.close(write_fd)

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
