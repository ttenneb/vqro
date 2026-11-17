"""Provider-v0 section 9 tests: in-test socket-pair core, no real core or hc."""
import asyncio
import base64
import importlib.util
import json
from pathlib import Path
import socket
import sys
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "extensions"))
from prim_provider import Limits, Provider, ProviderError, facts
spec = importlib.util.spec_from_file_location("fake_provider", ROOT / "tests/fixtures/fake_provider.py")
fixture = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fixture)


class Core:
    async def start(self):
        left, right = socket.socketpair()
        left.setblocking(False)
        right.setblocking(False)
        reader, writer = await asyncio.open_connection(sock=right, limit=1048577)
        self.attach(reader, writer)
        return left

    def attach(self, reader, writer):
        self.reader, self.writer = reader, writer
        self.rows, self.pending = [], {}
        self.changed = asyncio.Event()
        self.serial = 0
        self.hold = set()
        self.task = asyncio.create_task(self.read())

    async def send(self, row):
        self.writer.write(json.dumps(row, ensure_ascii=False).encode() + b"\r\n")
        await self.writer.drain()

    async def read(self):
        try:
            while wire := await self.reader.readline():
                row = json.loads(wire)
                self.rows.append(row)
                self.changed.set()
                self.changed = asyncio.Event()
                if "method" not in row:
                    future = self.pending.pop(row.get("id"), None)
                    if future is not None:
                        future.set_result(row)
                    continue
                if "id" not in row:
                    self.notification(row)
                    continue
                if row["method"] in self.hold:
                    continue
                try:
                    result = await self.result(row)
                    response = {"jsonrpc": "2.0", "id": row["id"], "result": result}
                except ProviderError as error:
                    response = {"jsonrpc": "2.0", "id": row["id"], "error": {
                        "code": -32000, "message": str(error), "data": {"kind": error.kind}}}
                await self.send(response)
        except (ConnectionError, asyncio.CancelledError):
            pass

    def notification(self, row):
        pass

    async def result(self, row):
        if row["method"] == "provider.register":
            return {"provider": row["params"]["name"], "generation": 7, "limits": {
                "stream_window_bytes": min(row["params"]["limits"]["stream_window_bytes"], 262144),
                "max_record_bytes": 1048576}}
        return row["params"] if row["method"] == "probe" else {}

    async def call(self, method, params=None, generation=7):
        self.serial += 1
        future = asyncio.get_running_loop().create_future()
        self.pending[self.serial] = future
        await self.send({"jsonrpc": "2.0", "id": self.serial, "method": method,
                         "params": {**(params or {}), "generation": generation}})
        return await asyncio.wait_for(future, 2)

    async def note(self, method, params, generation=7):
        await self.send({"jsonrpc": "2.0", "method": method,
                         "params": {**params, "generation": generation}})

    async def wait(self, predicate):
        async def waiting():
            while not predicate():
                changed = self.changed
                await changed.wait()
        await asyncio.wait_for(waiting(), 2)

    def notes(self, method, stream=None):
        return [r for r in self.rows if r.get("method") == method and
                (stream is None or r["params"].get("stream") == stream)]

    async def close(self):
        self.writer.close()
        await self.writer.wait_closed()
        self.task.cancel()
        await asyncio.gather(self.task, return_exceptions=True)


class ProviderTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.core = Core()
        sock = await self.core.start()
        self.provider = Provider("fake", "instance", fixture.CAPABILITIES)
        self.fake = fixture.Fake(self.provider)
        await self.provider.connect(sock=sock)
        await self.provider.full_sync([])
        self.effect_n = 0

    async def asyncTearDown(self):
        await self.fake.close()
        await self.provider.close()
        await self.core.close()

    async def effect(self, op, args=None, ref=None, effect=None, deadline_ms=30000):
        self.effect_n += 1
        params = {"effect": effect or f"e{self.effect_n}", "op": op, "args": args or {}, "deadline_ms": deadline_ms}
        if ref is not None:
            params.update(ref=ref, incarnation="")
        return await self.core.call("effect.invoke", params)

    async def spawn(self, argv=None):
        result = await self.effect("terminal.spawn", {"argv": argv or [], "name": "fixture"})
        self.assertEqual(result["result"]["outcome"], "observed")
        return result["result"]["data"]["facts"]["ref"]

    async def attach(self, ref, stream=1, window=262144):
        return await self.core.call("stream.open", {"stream": stream, "ref": ref, "incarnation": "",
            "cols": 80, "rows": 24, "history_lines": 0, "window_bytes": window})

    async def test_registration_sync_and_generation(self):
        register = self.core.notes("provider.register")[0]["params"]
        self.assertEqual(register["instance"], "instance")
        self.assertEqual(register["capabilities"], fixture.CAPABILITIES)
        self.assertEqual(self.provider.generation, 7)
        self.assertEqual(self.core.notes("provider.sync")[0]["params"], {"resources": [], "final": True, "generation": 7})
        await self.provider.upsert({"ref": "one", "kind": "terminal", "state": "live"})
        await self.core.wait(lambda: bool(self.core.notes("resource.upsert")))
        self.assertEqual(self.core.notes("resource.upsert")[-1]["params"]["generation"], 7)
        await self.provider.gone("one", why={"reason": "closed"})
        await self.core.wait(lambda: bool(self.core.notes("resource.gone")))

    async def test_spawn_echo_kill_and_open_before_data(self):
        ref = await self.spawn()
        opened = await self.attach(ref)
        self.assertEqual(opened["result"], {"initial": "none"})
        await self.core.wait(lambda: bool(self.core.notes("stream.data", 1)))
        rows = self.core.rows
        reply_index = next(i for i, r in enumerate(rows) if r.get("id") == opened["id"] and "result" in r)
        data_index = next(i for i, r in enumerate(rows) if r.get("method") == "stream.data")
        self.assertLess(reply_index, data_index)
        greeting = self.core.notes("stream.data", 1)[0]["params"]
        self.assertEqual(base64.b64decode(greeting["data"]), b"fake f1\r\n")
        self.assertEqual(greeting["offset"], 0)
        await self.core.note("stream.input", {"stream": 1, "data": base64.b64encode(b"hello\x00\xff").decode()})
        await self.core.wait(lambda: len(self.core.notes("stream.data", 1)) == 2)
        echo = self.core.notes("stream.data", 1)[1]["params"]
        self.assertEqual(echo["offset"], 9)
        self.assertEqual(base64.b64decode(echo["data"]), b"hello\x00\xff")
        killed = await self.effect("terminal.kill", ref=ref)
        self.assertEqual(killed["result"]["data"]["facts"]["state"], "finished")
        await self.core.wait(lambda: bool(self.core.notes("stream.closed", 1)))

    async def test_credit_is_decoded_and_returned_to_backend_only_on_core_credit(self):
        ref = await self.spawn()
        await self.attach(ref, window=4)
        await self.core.wait(lambda: len(self.core.notes("stream.data", 1)) == 1)
        stream = self.provider.streams[1]
        returned = []
        stream.on_credit = lambda n: returned.append(n)
        self.assertEqual((stream.credit, stream.outstanding), (0, 4))
        self.assertEqual(returned, [])
        await self.core.note("stream.credit", {"stream": 1, "bytes": 2})
        await self.core.wait(lambda: len(self.core.notes("stream.data", 1)) == 2)
        self.assertEqual(base64.b64decode(self.core.notes("stream.data", 1)[1]["params"]["data"]), b" f")
        await self.core.note("stream.credit", {"stream": 1, "bytes": 999})
        await self.core.wait(lambda: len(self.core.notes("stream.data", 1)) == 3)
        self.assertLessEqual(stream.credit + stream.outstanding, 4)
        self.assertEqual(sum(returned), 6)

    async def test_slow_stream_isolated_and_input_resize_close_ordered(self):
        ref = await self.spawn()
        await self.attach(ref, stream=1, window=1)
        await self.attach(ref, stream=2, window=64)
        await self.core.wait(lambda: bool(self.core.notes("stream.data", 2)))
        applied = []
        stream = self.provider.streams[2]
        stream.on_input = lambda value: applied.append(("input", value))
        stream.on_resize = lambda cols, rows: applied.append(("resize", cols, rows))
        stream.on_close = lambda: applied.append(("close",))
        await self.core.note("stream.input", {"stream": 2, "data": "QQ=="})
        await self.core.note("stream.resize", {"stream": 2, "cols": 120, "rows": 40})
        await self.core.note("stream.input", {"stream": 2, "data": "Qg=="})
        await self.core.note("stream.close", {"stream": 2})
        await self.core.wait(lambda: bool(self.core.notes("stream.closed", 2)))
        self.assertEqual(applied, [("input", b"A"), ("resize", 120, 40), ("input", b"B"), ("close",)])
        self.assertEqual(len(self.core.notes("stream.data", 1)), 1)

    async def test_gap_final_offsets_and_no_reuse_unknown_stream_ignored(self):
        ref = await self.spawn()
        await self.attach(ref)
        await self.core.wait(lambda: bool(self.core.notes("stream.data", 1)))
        await self.provider.streams[1].gap("paused")
        await self.core.wait(lambda: bool(self.core.notes("stream.gap", 1)))
        gap = self.core.notes("stream.gap", 1)[0]["params"]
        self.assertEqual(gap["offset"], 9)
        self.assertEqual(gap["reason"], {"reason": "gap", "gap": "paused"})
        self.assertEqual((await self.attach(ref))["error"]["data"]["kind"], "conflict")
        await self.core.note("stream.input", {"stream": 1, "data": "not base64"})
        self.assertEqual((await self.provider.call("probe", {}))["generation"], 7)

    async def test_stale_request_rejected_stale_notification_disconnects(self):
        row = await self.core.call("effect.invoke", {}, generation=6)
        self.assertEqual(row["error"]["data"]["kind"], "stale_generation")
        self.assertEqual(self.fake.counter, 0)
        await self.core.note("stream.close", {"stream": 1}, generation=6)
        await asyncio.wait_for(self.provider.wait_closed(), 1)
        self.assertIn("generation", self.provider.failure)

    async def test_deadline_one_eventual_reply_no_cancel_or_duplicate_execution(self):
        entered, release = asyncio.Event(), asyncio.Event()
        calls = []
        async def delayed(ctx, args):
            calls.append(ctx)
            entered.set()
            await release.wait()
            return {"outcome": "accepted", "data": {"late": ctx.expired}}
        self.provider.effects["terminal.spawn"] = delayed
        pending = asyncio.create_task(self.effect("terminal.spawn", effect="once", deadline_ms=1))
        await entered.wait()
        await asyncio.sleep(.01)  # deliberately expire the forwarded deadline
        duplicate = await self.effect("terminal.spawn", effect="once")
        self.assertEqual(duplicate["error"]["data"]["kind"], "conflict")
        self.assertFalse(pending.done())
        release.set()
        self.assertEqual((await pending)["result"], {"outcome": "accepted", "data": {"late": True}})
        self.assertEqual(len(calls), 1)

    async def test_unacknowledged_native_dispatch_is_not_acceptance_or_uncertainty(self):
        self.provider.effects["terminal.spawn"] = lambda ctx, args: {"outcome": "dispatched"}
        row = await self.effect("terminal.spawn")
        self.assertEqual(row["result"], {"outcome": "dispatched"})

    async def test_no_indeterminate_result_or_false_rejection_on_backend_ambiguity(self):
        self.provider.effects["terminal.spawn"] = lambda ctx, args: {"outcome": "indeterminate"}
        row = await self.effect("terminal.spawn")
        self.assertEqual(row["error"]["data"]["kind"], "backend")
        self.assertNotIn("result", row)

    async def test_bad_input_error_and_unicode_lf_framing(self):
        ref = await self.spawn()
        await self.attach(ref)
        await self.core.note("stream.input", {"stream": 1, "data": "!bad!"})
        await self.core.wait(lambda: bool(self.core.notes("stream.error", 1)))
        error = self.core.notes("stream.error", 1)[0]["params"]
        self.assertEqual((error["outcome"], error["error"]["kind"]), ("rejected", "invalid"))
        text = "before\u2028middle\u2029after"
        self.assertEqual((await self.provider.call("probe", {"text": text}))["text"], text)

    async def test_call_deadline_removes_pending_without_resend(self):
        self.core.hold.add("hang")
        with self.assertRaises(ProviderError) as caught:
            await self.provider.call("hang", {}, timeout=.01)
        self.assertEqual(caught.exception.kind, "timeout")
        self.assertEqual(self.provider._pending, {})
        self.assertEqual(len(self.core.notes("hang")), 1)

    async def test_load_profile_status_phase_and_flood(self):
        ref = await self.spawn(["agent", "--status-every-ms", "100", "--status-phase-ms", "1", "--output-bytes-per-sec", "10000"])
        await self.attach(ref)
        await self.core.wait(lambda: len(self.core.notes("stream.data", 1)) >= 2)
        await self.core.wait(lambda: any(r["params"]["facts"].get("status") for r in self.core.notes("resource.upsert")))
        status = [r["params"]["facts"]["status"] for r in self.core.notes("resource.upsert") if r["params"]["facts"].get("status")][-1]
        self.assertEqual(status["n"], 1)
        self.assertGreater(status["at_ms"], 0)
        self.assertTrue(any(b"flood output" in base64.b64decode(r["params"]["data"]) for r in self.core.notes("stream.data", 1)))
        await self.effect("terminal.kill", ref=ref)

    async def test_full_sync_chunks_and_final_flag(self):
        self.provider.max_record = 512
        before = len(self.core.notes("provider.sync"))
        await self.provider.full_sync({"ref": f"r{i}", "kind": "terminal", "state": "live", "data": {"pad": "x" * 250}} for i in range(3))
        chunks = self.core.notes("provider.sync")[before:]
        self.assertEqual([r["params"]["final"] for r in chunks], [False, False, True])
        self.assertEqual([r["params"]["resources"][0]["ref"] for r in chunks], ["r0", "r1", "r2"])

    async def test_effect_ledger_and_concurrent_stream_limits(self):
        self.provider.limits = Limits(identities=2, max_streams=1)
        ref = await self.spawn()
        await self.attach(ref, stream=1)
        blocked = await self.attach(ref, stream=2)
        self.assertEqual(blocked["error"]["data"]["kind"], "overloaded")
        await self.core.note("stream.close", {"stream": 1})
        await self.core.wait(lambda: bool(self.core.notes("stream.closed", 1)))
        self.assertIn("result", await self.attach(ref, stream=2))
        await self.effect("terminal.kill", ref=ref)
        full = await self.effect("terminal.spawn")
        self.assertEqual(full["error"]["data"]["kind"], "overloaded")
        self.assertEqual(self.fake.counter, 1)

    async def test_input_bound_error_and_reserved_ordered_close(self):
        ref = await self.spawn()
        await self.attach(ref)
        await self.core.wait(lambda: bool(self.core.notes("stream.data", 1)))
        self.provider.limits = Limits(stream_queue_bytes=16, stream_queue_items=2)
        stream = self.provider.streams[1]
        entered, release = asyncio.Event(), asyncio.Event()
        consumed = []
        async def blocked(data):
            consumed.append(data)
            entered.set()
            await release.wait()
        stream.on_input = blocked
        value = base64.b64encode(b"12345678").decode()
        await self.core.note("stream.input", {"stream": 1, "data": value})
        await asyncio.wait_for(entered.wait(), 1)
        await self.core.note("stream.input", {"stream": 1, "data": value})
        await self.core.note("stream.input", {"stream": 1, "data": value})
        await self.core.note("stream.close", {"stream": 1})
        await self.core.wait(lambda: bool(self.core.notes("stream.error", 1)))
        self.assertEqual(self.core.notes("stream.error", 1)[0]["params"]["error"]["kind"], "overloaded")
        release.set()
        await self.core.wait(lambda: bool(self.core.notes("stream.closed", 1)))
        self.assertEqual(consumed, [b"12345678", b"12345678"])

    async def test_output_bound_and_zero_credit_close_do_not_hang(self):
        ref = await self.spawn()
        await self.attach(ref, window=1)
        await self.core.wait(lambda: bool(self.core.notes("stream.data", 1)))
        stream = self.provider.streams[1]
        self.provider.limits = Limits(stream_queue_bytes=16)
        with self.assertRaises(ProviderError):
            stream.offer(b"x" * 17)
        with self.assertRaises(ProviderError):
            stream.offer(b"x" * 65537)
        await self.core.note("stream.close", {"stream": 1})
        await self.core.wait(lambda: bool(self.core.notes("stream.closed", 1)))
        self.assertEqual(len(self.core.notes("stream.data", 1)), 1)

    async def test_disconnect_resolves_pending_and_never_reconnects(self):
        self.core.hold.add("probe")
        pending = asyncio.create_task(self.provider.call("probe", {}))
        await self.core.wait(lambda: bool(self.core.notes("probe")))
        self.core.writer.close()
        with self.assertRaises(ProviderError) as caught:
            await pending
        self.assertEqual(caught.exception.kind, "unavailable")
        await asyncio.wait_for(self.provider.wait_closed(), 1)
        with self.assertRaises(ProviderError):
            await self.provider.connect(sock=None)
        self.assertEqual(len(self.core.notes("probe")), 1)

    async def test_handler_capacity_and_no_effect_for_unsupported_op(self):
        self.provider.limits = Limits(pending=1)
        entered, release = asyncio.Event(), asyncio.Event()
        async def held(ctx, args):
            entered.set()
            await release.wait()
            return {"outcome": "accepted"}
        self.provider.effects["terminal.spawn"] = held
        first = asyncio.create_task(self.effect("terminal.spawn"))
        await entered.wait()
        other = await self.effect("terminal.spawn")
        self.assertEqual(other["error"]["data"]["kind"], "overloaded")
        release.set()
        self.assertEqual((await first)["result"]["outcome"], "accepted")
        unsupported = await self.effect("agent.prompt")
        self.assertEqual(unsupported["error"]["data"]["kind"], "unsupported")

    async def test_oversize_lf_record_disconnects_without_unbounded_buffer(self):
        self.core.writer.write(b"x" * (1048576 + 2))
        try:
            await self.core.writer.drain()
        except ConnectionError:
            pass
        await asyncio.wait_for(self.provider.wait_closed(), 1)
        self.assertTrue(self.provider._closed.is_set())

    def test_facts_and_capability_validation(self):
        with self.assertRaises(ProviderError):
            facts({"ref": "x", "kind": "agent", "state": "live", "data": {"huge": "x" * 65536}})
        with self.assertRaises(ValueError):
            Provider("bad", "i", {"terminal.spawn": False})
        value = {"ref": "x", "kind": "custom", "state": "live", "data": {"unknown": "preserved"}}
        self.assertEqual(facts(value), value)


if __name__ == "__main__":
    unittest.main()
