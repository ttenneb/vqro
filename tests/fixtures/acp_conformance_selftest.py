#!/usr/bin/env python3
"""Independent native-process checks only; not Part A adapter certification."""
import asyncio
import hashlib
import json
import os
from pathlib import Path
import tempfile
import unittest

import acp_conformance_model as p6

HERE = Path(__file__).resolve().parent


class VendorTests(unittest.TestCase):
    def test_exact_p6_source_prefix_and_schema_projection(self):
        raw = (HERE / "acp_conformance_model.py").read_bytes()
        prefix = raw.split(b"\n# ACP_CONFORMANCE_VOCABULARY:", 1)[0]
        self.assertEqual(hashlib.sha256(prefix).hexdigest(), p6.P6_MODEL_SHA256)
        self.assertEqual(p6.P6_MODEL_SHA256, "661df1e8e754e2608719a4eda80ea2cb6761390abf8820beb38013968ad1d5a9")
        codec = p6.Codec(p6.SCHEMA_VOCABULARY)
        params = {"sessionId": "s1", "prompt": [{"type": "text", "text": "go"}]}
        self.assertEqual(codec.encode(2, "session/prompt", codec.decode(2, "session/prompt", params))["params"], params)
        with self.assertRaises(p6.Unsupported):
            codec.decode(1, "session/new", {"cwd": "/private", "mcpServers": [{"name": "not-implemented"}]})


class NativeTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        base = Path.home() / ".local/share/herdr/scratch"
        base.mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(prefix="af-", dir=base)
        self.root = Path(self.temp.name)
        (self.root / "home").mkdir()
        self.control_path = self.root / "c"
        self.ledger = self.root / "l"
        self.proc = await asyncio.create_subprocess_exec(
            "/usr/bin/python3", str(HERE / "acp_conformance.py"), "--control", str(self.control_path), "--ledger", str(self.ledger),
            stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE,
            env={"HOME": str(self.root / "home"), "PATH": "/usr/bin:/bin", "PYTHONDONTWRITEBYTECODE": "1"})
        self.rows = []
        self.changed = asyncio.Condition()
        self.pump = asyncio.create_task(self.read_outputs())
        self.control_id = 0
        for _ in range(200):
            if self.control_path.exists():
                await self.control("state")
                break
            if self.proc.returncode is not None:
                self.fail((await self.proc.stderr.read()).decode())
            await asyncio.sleep(.01)
        else:
            self.fail("private fixture did not bind its control socket")

    async def asyncTearDown(self):
        try:
            if self.proc.returncode is None:
                try:
                    await self.control("exit")
                    await asyncio.wait_for(self.proc.wait(), 2)
                except (OSError, asyncio.TimeoutError):
                    self.proc.kill()
                    await self.proc.wait()
            await asyncio.wait_for(self.pump, 2)
            error = (await self.proc.stderr.read()).decode()
            self.assertEqual(self.proc.returncode, 0, error)
            self.assertFalse(self.control_path.exists(), "owned control socket not cleaned")
        finally:
            if self.proc.returncode is None:
                self.proc.kill()
                await self.proc.wait()
            self.temp.cleanup()

    async def read_outputs(self):
        while raw := await self.proc.stdout.readline():
            row = json.loads(raw)
            async with self.changed:
                self.rows.append(row)
                self.changed.notify_all()

    async def wait_row(self, predicate):
        async def waiting():
            async with self.changed:
                while True:
                    for row in self.rows:
                        if predicate(row):
                            return row
                    await self.changed.wait()
        return await asyncio.wait_for(waiting(), 2)

    async def control(self, verb, **params):
        reader, writer = await asyncio.wait_for(asyncio.open_unix_connection(str(self.control_path)), 2)
        self.control_id += 1
        ident = self.control_id
        try:
            writer.write(json.dumps({"id": ident, "verb": verb, **params}).encode() + b"\n")
            await writer.drain()
            row = json.loads(await asyncio.wait_for(reader.readline(), 2))
            self.assertEqual(row["id"], ident)
            return row
        finally:
            writer.close()
            await writer.wait_closed()

    async def send(self, ident, method=None, params=None, **extra):
        row = {"jsonrpc": "2.0", "id": ident, **extra}
        if method is not None:
            row.update(method=method, params=params or {})
        self.proc.stdin.write(json.dumps(row).encode() + b"\n")
        await self.proc.stdin.drain()

    async def rpc(self, ident, method, params=None):
        await self.send(ident, method, params)
        return await self.wait_row(lambda row: row.get("id") == ident and "method" not in row)

    async def session(self, version=2):
        hello = await self.rpc(1, "initialize", {"protocolVersion": version, "info" if version == 2 else "clientInfo": {"name": "selftest", "version": "1"}})
        self.assertEqual(hello["result"]["protocolVersion"], version)
        created = await self.rpc(2, "session/new", {"cwd": str(self.root), "mcpServers": []})
        return created["result"]["sessionId"]

    def records(self, kind=None):
        rows = [json.loads(raw) for raw in self.ledger.read_bytes().split(b"\n")[:-1]]
        return [r for r in rows if kind is None or r["kind"] == kind]

    async def wait_ledger(self, kind, count=1):
        for _ in range(200):
            records = self.records(kind)
            if len(records) >= count:
                return records
            await asyncio.sleep(.01)
        self.fail(f"missing completed ledger barrier {kind}")

    async def test_native_handshake_prompt_and_ordered_model_envelope(self):
        session = await self.session()
        content = [{"type": "text", "text": "first"}, {"type": "text", "text": "second"}]
        result = await self.rpc(3, "session/prompt", {"sessionId": session, "prompt": content})
        self.assertIn("messageId", result["result"])
        await self.control("model_call", id_ref="model-1")
        self.assertEqual(self.records("model_call")[-1]["messages"], [{"role": "user", "content": content}])
        state = (await self.control("state"))["result"]
        self.assertTrue(state["active"])
        self.assertEqual(state["messages"], [{"role": "user", "content": content}])
        self.assertEqual(len(self.records("input_received")), 1)
        self.assertTrue(all(set(("seq", "kind", "ns", "pid")) <= row.keys() for row in self.records()))
        self.assertFalse(self.records("ready")[0]["herdr_in_child"])

    async def test_admission_is_not_insertion_and_cancel_does_not_insert(self):
        session = await self.session()
        await self.control("pause_before", phase="insertion")
        await self.send(3, "session/prompt", {"sessionId": session, "prompt": [{"type": "text", "text": "held"}]})
        await self.wait_ledger("paused")
        self.assertEqual(self.records("inserted"), [])
        self.assertFalse(any(r.get("id") == 3 for r in self.rows))
        await self.control("model_call")
        self.assertEqual(self.records("model_call")[-1]["messages"], [])
        await self.send(4, "session/cancel", {"sessionId": session})
        result = await self.wait_row(lambda r: r.get("id") == 3)
        self.assertEqual(result["error"]["code"], -32800)
        await self.control("release", phase="insertion")
        await self.wait_row(lambda r: r.get("id") == 4)
        self.assertEqual(self.records("inserted"), [])

    async def test_dropped_send_reply_has_one_effect_and_reconnect_never_resends(self):
        session = await self.session()
        await self.control("drop_reply", operation="send")
        await self.send(3, "session/prompt", {"sessionId": session, "prompt": [{"type": "text", "text": "once"}]})
        await self.wait_ledger("reply_dropped")
        for _ in range(3):
            await self.control("reconnect")
        self.assertEqual(len(self.records("input_received")), 1)
        self.assertEqual(len(self.records("inserted")), 1)
        self.assertFalse(any(r.get("id") == 3 for r in self.rows))

    async def test_v1_waits_for_completion_v2_does_not(self):
        session = await self.session(1)
        await self.send(3, "session/prompt", {"sessionId": session, "prompt": [{"type": "text", "text": "v1"}]})
        await self.wait_ledger("inserted")
        self.assertFalse(any(r.get("id") == 3 for r in self.rows))
        await self.control("turn", state="settled")
        result = await self.wait_row(lambda r: r.get("id") == 3)
        self.assertEqual(result["result"], {"stopReason": "end_turn"})
        replay = await self.rpc(4, "session/load", {"sessionId": session, "cwd": str(self.root), "mcpServers": []})
        self.assertIn("result", replay)
        self.assertEqual([r["phase"] for r in self.records("replay")], ["begin", "end"])

    async def test_permission_valid_invalid_and_cancel_are_not_auto_approval(self):
        await self.session()
        await self.control("turn", state="start")
        for request, outcome, valid, approved in [
            ("bad", {"outcome": "selected", "optionId": "invented"}, False, False),
            ("good", {"outcome": "selected", "optionId": "allow"}, True, True),
            ("cancel", {"outcome": "cancelled"}, True, False)]:
            controlled = await self.control("request", request=request, method="permission", title="May I?")
            self.assertIn("result", controlled)
            await self.wait_row(lambda r: r.get("id") == request and r.get("method") == "session/request_permission")
            count = len(self.records("answer"))
            await self.send(request, result={"outcome": outcome})
            await self.wait_ledger("answer", count=count + 1)
            answer = self.records("answer")[-1]
            self.assertEqual((answer["valid"], answer["approved"]), (valid, approved))
        before = len(self.records("native_reply"))
        await self.control("state")
        self.assertEqual(len(self.records("native_reply")), before, "JSON-RPC responses receive no fake acknowledgement")

    async def test_key_retention_replay_and_config(self):
        session = await self.session()
        params = {"sessionId": session, "prompt": [{"type": "text", "text": "keyed"}], "idempotencyKey": "key-1"}
        first = await self.rpc(3, "session/prompt", params)
        again = await self.rpc(4, "session/prompt", params)
        self.assertEqual(first["result"], again["result"])
        self.assertEqual(len(self.records("input_received")), 1)
        changed = await self.rpc(5, "session/set_config_option", {"sessionId": session, "configId": "fixture.auto", "type": "boolean", "value": True})
        self.assertTrue(changed["result"]["configOptions"][0]["currentValue"])
        rejected = await self.rpc(6, "session/set_config_option", {"sessionId": session, "configId": "fixture.auto", "type": "boolean", "value": "not-a-bool"})
        self.assertIn("error", rejected)
        await self.control("retain", action="prune", key="key-1")
        await self.control("turn", state="settled")
        await self.control("reconnect")
        await self.rpc(7, "session/resume", {"sessionId": session, "cwd": str(self.root), "replayFrom": {"type": "start"}})
        self.assertEqual((await self.control("state"))["result"]["messages"], [])
        self.assertEqual(len(self.records("input_received")), 1)
        self.assertEqual([r["phase"] for r in self.records("replay")], ["begin", "end"])
        fence = next(i for i, r in enumerate(self.rows) if r.get("params", {}).get("update", {}).get("sessionUpdate") == "replay_complete")
        reply = next(i for i, r in enumerate(self.rows) if r.get("id") == 7)
        self.assertLess(fence, reply, "replay fence must precede the native resume reply")
        self.assertIn("error", await self.control("queue"))
        bad = await self.rpc(8, "session/new", {"cwd": str(self.root), "mcpServers": [{"name": "MCP-is-a-gap"}]})
        self.assertIn("error", bad)

    async def test_bounded_state_pages_and_oversized_prompt_refusal(self):
        session = await self.session()
        for ident in (3, 4):
            await self.rpc(ident, "session/prompt", {"sessionId": session, "prompt": [{"type": "text", "text": str(ident) * 22000}]})
        first = (await self.control("state", limit=1))["result"]
        self.assertEqual((len(first["messages"]), first["next"]), (1, 1))
        second = (await self.control("state", after=first["next"], limit=1))["result"]
        self.assertEqual((len(second["messages"]), second["next"]), (1, None))
        self.assertLess(len(json.dumps(first).encode()), 65536)
        self.assertIn("error", await self.control("state", limit=0))
        before = len(self.records("input_received"))
        rejected = await self.rpc(5, "session/prompt", {"sessionId": session, "prompt": [{"type": "text", "text": "x" * 40000}]})
        self.assertIn("error", rejected)
        self.assertEqual(len(self.records("input_received")), before)
        self.assertIn("error", await self.control("tool_call", id_ref="unsupported", state="start"))
        self.assertIn("error", await self.rpc(6, "session/fork", {"sessionId": session}))

    async def test_v1_permission_native_framing(self):
        await self.session(1)
        await self.control("turn", state="start")
        await self.control("request", request="q-v1", method="permission")
        request = await self.wait_row(lambda r: r.get("id") == "q-v1")
        self.assertIn("toolCall", request["params"])
        self.assertNotIn("title", request["params"])
        await self.send("q-v1", result={"outcome": {"outcome": "selected", "optionId": "allow"}})
        self.assertTrue((await self.wait_ledger("answer"))[0]["approved"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
