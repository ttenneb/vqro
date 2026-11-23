"""Harness self-tests, not adapter certification. No real services or models."""
import asyncio
import base64
import io
import json
import socket
import struct
import wave
import zlib
from pathlib import Path
import sys
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import AsyncMock, patch

from agent_conformance.core import PublicImplementation
from agent_conformance.runtime import Runtime
from agent_conformance.inventory import Contract, main_contract, part_a_ids
from agent_conformance.cases import declarations, restriction, schema_example
from agent_conformance.content import compare_content, examples, png, wav
from agent_conformance.results import CASES, BY_ID, Case, CheckFailed, FixtureFailed, HarnessGap, NotApplicable, Result, markdown, report, select_cases
from agent_conformance.runner import run_case
from agent_conformance.wire import ItemReducer, Peer, RpcError, WireFailure, close_writer

RUNTIME = Path(__file__).parent / "agent_conformance/runtime.py"


def line(row):
    return json.dumps(row, ensure_ascii=False, separators=(",", ":")).encode() + b"\n"


def begin(epoch="e1", coverage="all_retained"):
    return {"op": "snapshot", "phase": "begin", "epoch": epoch, "coverage": coverage}


def end(seq=0):
    return {"op": "snapshot", "phase": "end", "seq": seq}


def put(seq=None, ident="m1", rev=1, text="hi", position=0):
    row = {"op": "put", "kind": "message", "id": ident, "rev": rev,
           "value": {"position": position, "role": "assistant", "content": [{"type": "text", "text": text}], "final": False}}
    if seq is not None:
        row["seq"] = seq
    return row


def append(seq=1, base=1, rev=2, text=" there", field="/content/0/text"):
    return {"op": "append", "seq": seq, "kind": "message", "id": "m1", "base": base, "rev": rev,
            "field": field, "text": text}


def seeded(**kwargs):
    reducer = ItemReducer(**kwargs)
    for row in (begin(), put(), end()):
        reducer.accept(row)
    return reducer


class ItemOracleTests(unittest.TestCase):
    def test_snapshot_installs_only_at_end(self):
        reducer = seeded()
        reducer.accept(begin("e2", "tail"))
        reducer.accept(put(text="new"))
        self.assertEqual(reducer.entities[("message", "m1")]["value"]["content"][0]["text"], "hi")
        self.assertEqual(reducer.cursor, {"epoch": "e1", "seq": 0})
        self.assertEqual(reducer.coverage, "all_retained")
        reducer.accept(end(500))
        self.assertEqual(reducer.cursor, {"epoch": "e2", "seq": 500})
        self.assertEqual(reducer.coverage, "tail")
        self.assertEqual(reducer.entities[("message", "m1")]["value"]["content"][0]["text"], "new")

    def test_new_epoch_does_not_ignore_small_sequence(self):
        reducer = seeded()
        reducer.accept(begin("old"))
        reducer.accept(end(500))
        reducer.accept(begin("new"))
        reducer.accept(put())
        reducer.accept(end())
        reducer.accept(append())
        self.assertEqual(reducer.sequence, 1)
        self.assertEqual(reducer.entities[("message", "m1")]["value"]["content"][0]["text"], "hi there")

    def test_append_remove_and_idempotent_old_sequence(self):
        reducer = seeded()
        reducer.accept(append())
        reducer.accept(append())
        self.assertEqual(reducer.entities[("message", "m1")]["rev"], 2)
        reducer.accept({"op": "remove", "seq": 2, "kind": "message", "id": "m1", "rev": 3})
        self.assertFalse(reducer.entities)
        # A partial-coverage viewer can receive removal of an unretained entity.
        reducer.accept({"op": "remove", "seq": 3, "kind": "message", "id": "older", "rev": 4})
        self.assertEqual(reducer.sequence, 3)

    def test_wrong_append_base_does_not_mutate(self):
        reducer = seeded()
        before = json.dumps(reducer.entities[("message", "m1")])
        with self.assertRaisesRegex(WireFailure, "base"):
            reducer.accept(append(base=2))
        self.assertEqual(json.dumps(reducer.entities[("message", "m1")]), before)
        self.assertEqual(reducer.sequence, 0)

    def test_sequence_hole_is_not_a_silent_join(self):
        with self.assertRaisesRegex(WireFailure, "noncontiguous"):
            seeded().accept(append(seq=2))

    def test_replacement_cannot_move_timeline_position(self):
        with self.assertRaisesRegex(WireFailure, "position"):
            seeded().accept(put(seq=1, rev=2, position=99))

    def test_split_lf_and_utf8_not_unicode_line_separators(self):
        reducer = ItemReducer()
        rows = [begin(), put(text="snow ☃ and separator \u2028 and CR\r and LF\n"), end()]
        raw = b"".join(line(row) for row in rows)
        for offset, byte in enumerate(raw):
            reducer.feed(offset, bytes([byte]))
        self.assertIsNone(reducer.failure)
        self.assertEqual(reducer.entities[("message", "m1")]["value"]["content"][0]["text"], rows[1]["value"]["content"][0]["text"])

    def test_close_discards_unfinished_snapshot_and_partial_line(self):
        reducer = seeded()
        reducer.accept(begin("e2"))
        reducer.feed(0, b'{"op":"put"')
        reducer.ended("future_gap_reason")
        self.assertEqual(reducer.failure, "future_gap_reason")
        self.assertIsNone(reducer.staged)
        self.assertFalse(reducer.pending)
        self.assertEqual(reducer.epoch, "e1")

    def test_bad_frame_offset_and_base64_are_gaps(self):
        reducer = ItemReducer()
        reducer.feed(1, b"x")
        self.assertIn("offset", reducer.failure)
        reducer = ItemReducer()
        reducer.stream_data({"offset": 0, "data": "!not-base64!"})
        self.assertIsNotNone(reducer.failure)

    def test_record_and_reconstruction_diagnostics_are_bounded(self):
        reducer = ItemReducer(max_record=32)
        reducer.feed(0, b"x" * 33)
        self.assertIn("64 KiB", reducer.failure)
        reducer = seeded(trace_bytes=256, trace_records=3)
        for seq in range(1, 30):
            reducer.accept(append(seq=seq, base=seq, rev=seq+1, text="."))
        self.assertLessEqual(reducer.trace_size, 256)
        self.assertLessEqual(len(reducer.trace), 3)
        self.assertGreater(reducer.trace_dropped, 0)

    def test_legacy_p3_shapes_are_not_accepted(self):
        for row in ({"op": "reset", "epoch": "e", "cut": 0}, {"op": "snapshot_end", "cut": 0},
                    {"type": "snapshot", "phase": "begin", "epoch": "e", "coverage": "tail"}):
            with self.subTest(row=row), self.assertRaises(WireFailure):
                ItemReducer().accept(row)
        reducer = seeded()
        with self.assertRaises(WireFailure):
            reducer.accept({"op": "remove", "seq": 1, "kind": "message", "id": "m1"})

    def test_invalid_pointers_cannot_write_another_element(self):
        for pointer in ("/content/-1/text", "/content/00/text", "/content/0/te~2xt", "content/0/text", "/content/9/text"):
            with self.subTest(pointer=pointer), self.assertRaises(WireFailure):
                seeded().accept(append(field=pointer))

    def test_byte_append_decodes_each_chunk_before_concatenation(self):
        reducer = ItemReducer()
        reducer.accept(begin())
        reducer.accept({"op": "put", "kind": "terminal", "id": "x1", "rev": 1, "value": {"output": ""}})
        reducer.accept(end())
        chunks = [b"\xff", b"\0", b"12", b"345", b"\x80\x81"]
        for seq, chunk in enumerate(chunks, 1):
            reducer.accept({"op": "append", "seq": seq, "kind": "terminal", "id": "x1", "base": seq,
                            "rev": seq+1, "field": "/output", "bytes": base64.b64encode(chunk).decode()})
        output = reducer.entities[("terminal", "x1")]["value"]["output"]
        self.assertEqual(base64.b64decode(output, validate=True), b"".join(chunks))
        self.assertNotIn("/w==AA==", output)
        with self.assertRaisesRegex(WireFailure, "text append"):
            reducer.accept({"op": "append", "seq": 6, "kind": "terminal", "id": "x1", "base": 6,
                            "rev": 7, "field": "/output", "text": "bad"})

    def test_snapshot_session_metadata_is_also_fenced(self):
        reducer = seeded()
        row = begin("e2")
        row["session"] = "s2"
        reducer.accept(row)
        self.assertIsNone(reducer.session)
        wrong = end()
        wrong["session"] = "s1"
        with self.assertRaisesRegex(WireFailure, "different session"):
            reducer.accept(wrong)


class TimelineTests(unittest.IsolatedAsyncioTestCase):
    async def test_canonical_timeline_uses_the_same_native_oracle(self):
        with tempfile.TemporaryDirectory(prefix="vq-k1-") as folder:
            root = Path(folder)
            script = root / "script.json"
            script.write_text(json.dumps({"script": [
                {"on": "input", "match": {"text": "go"}, "do": [{"turn": "start"}, {"tool_call": "tc1", "hold_ms": 0}]},
                {"after": "tc1", "do": [{"model_call": "m2"}, {"turn": "end", "stop": "end_turn"}]}]}))
            runtime = Runtime(SimpleNamespace(script=script, ledger=root / "ledger", wire="pi", no_prompt_templates=True))
            output = []
            async def emit(row):
                output.append(row)
            runtime.emit = emit
            await runtime.native({"id": "p1", "type": "prompt", "message": "go"})
            self.assertEqual(sum(row["type"] == "agent_start" for row in output), 1)
            self.assertTrue(output[0]["success"])
            self.assertNotIn("data", output[0])  # weak native ACK does not prove insertion
            ledger = [json.loads(raw) for raw in (root / "ledger").read_bytes().split(b"\n") if raw]
            tool_end = next(row for row in ledger if row["kind"] == "tool_call" and row["state"] == "end")
            model = next(row for row in ledger if row["kind"] == "model_call")
            self.assertLess(tool_end["seq"], model["seq"])
            self.assertEqual(model["messages"][0]["content"][0]["text"], "go")
            self.assertEqual(output[-1]["type"], "agent_settled")


class PeerTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.root = tempfile.TemporaryDirectory(prefix="vq-k1-")
        self.endpoint = Path(self.root.name) / "s"
        self.peers, self.server_writers = [], []

    async def asyncTearDown(self):
        for peer in self.peers:
            await peer.close()
        for writer in self.server_writers:
            await close_writer(writer)
        if hasattr(self, "server"):
            self.server.close()
            await self.server.wait_closed()
        self.root.cleanup()

    async def connect(self, handler):
        async def accepted(reader, writer):
            self.server_writers.append(writer)
            await handler(reader, writer)
        self.server = await asyncio.start_unix_server(accepted, str(self.endpoint))
        peer = await Peer.connect(self.endpoint)
        self.peers.append(peer)
        return peer

    @unittest.skipIf(sys.platform == "win32", "fixture connects to an asyncio Unix-domain server")
    async def test_stalled_socket_close_is_bounded_and_joins_reader(self):
        async def hold(reader, writer):
            pass  # Deliberately never consume this owned socket's input.
        peer = await self.connect(hold)
        peer.writer.transport.get_extra_info("socket").setsockopt(socket.SOL_SOCKET, socket.SO_SNDBUF, 4096)
        peer.writer.write(b"x" * 500000)
        await asyncio.sleep(.05)
        self.assertGreater(peer.writer.transport.get_write_buffer_size(), 0)
        await asyncio.wait_for(peer.close(), .8)
        self.assertTrue(peer.task.done())
        self.assertTrue(peer.writer.transport.is_closing())
        await peer.close()  # Idempotent even after the flush deadline expired.

    async def test_cancelled_close_still_aborts_transport_and_joins_reader(self):
        waiting, aborted = asyncio.Event(), []
        async def never_closed():
            waiting.set()
            await asyncio.Event().wait()
        writer = SimpleNamespace(close=lambda: None, wait_closed=never_closed,
                                 transport=SimpleNamespace(abort=lambda: aborted.append(True)))
        peer = Peer(asyncio.StreamReader(), writer)
        closing = asyncio.create_task(peer.close())
        await asyncio.wait_for(waiting.wait(), 1)
        closing.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await closing
        self.assertEqual(aborted, [True])
        self.assertTrue(peer.task.done())

    @unittest.skipIf(sys.platform == "win32", "fixture connects to an asyncio Unix-domain server")
    async def test_response_error_and_unknown_late_id(self):
        async def server(reader, writer):
            row = json.loads(await reader.readline())
            writer.write(line({"jsonrpc": "2.0", "id": -1, "result": "late, never replayed"}))
            writer.write(line({"jsonrpc": "2.0", "id": row["id"], "error": {"message": "stale", "data": {"kind": "stale"}}}))
            await writer.drain()
        peer = await self.connect(server)
        with self.assertRaises(RpcError) as caught:
            await peer.call("resource.invoke")
        self.assertEqual(caught.exception.kind, "stale")
        self.assertEqual(peer.serial, 1)
        self.assertFalse(peer.pending)

    @unittest.skipIf(sys.platform == "win32", "fixture connects to an asyncio Unix-domain server")
    async def test_out_of_order_concurrent_replies(self):
        async def server(reader, writer):
            a, b = json.loads(await reader.readline()), json.loads(await reader.readline())
            for row in (b, a):
                writer.write(line({"jsonrpc": "2.0", "id": row["id"], "result": row["method"]}))
            await writer.drain()
        peer = await self.connect(server)
        self.assertEqual(await asyncio.gather(peer.call("a"), peer.call("b")), ["a", "b"])

    @unittest.skipIf(sys.platform == "win32", "fixture connects to an asyncio Unix-domain server")
    async def test_disconnect_fails_pending_without_retry(self):
        seen = []
        async def server(reader, writer):
            seen.append(json.loads(await reader.readline()))
            writer.close()
            await writer.wait_closed()
        peer = await self.connect(server)
        with self.assertRaisesRegex(WireFailure, "not replayed"):
            await peer.call("resource.invoke")
        self.assertEqual(len(seen), 1)

    @unittest.skipIf(sys.platform == "win32", "fixture connects to an asyncio Unix-domain server")
    async def test_callbacks_consume_streams_without_queued_copies(self):
        reducer = ItemReducer()
        async def server(reader, writer):
            await reader.readline()
            payload = b"".join(line(row) for row in (begin(), put(), end()))
            for byte in payload:
                writer.write(line({"jsonrpc": "2.0", "method": "stream.data", "params": {
                    "stream": "s1", "data": base64.b64encode(bytes([byte])).decode()}}))
            await writer.drain()
        peer = await self.connect(server)
        peer.on("stream.data", reducer.stream_data)
        await peer.note("ready")
        await peer.wait(lambda: reducer.snapshots == 1)
        self.assertFalse(peer.notifications)
        self.assertIsNone(reducer.failure)


class BackendCleanupTests(unittest.IsolatedAsyncioTestCase):
    async def test_peer_failure_does_not_skip_owned_backend_cleanup(self):
        cleaned = []
        async def failing_close():
            raise OSError("deliberate peer close failure")
        with tempfile.TemporaryDirectory() as root:
            ctx = object.__new__(PublicImplementation)
            ctx.root = Path(root)
            ctx.peers = [SimpleNamespace(close=failing_close)]
            ctx.backend = SimpleNamespace(cleanup=lambda: cleaned.append(True), log=[], env={})
            ctx.native = SimpleNamespace(endpoint=Path(root) / "absent.sock")
            with self.assertRaises(FixtureFailed):
                await asyncio.wait_for(ctx.close(), 1)
            self.assertEqual(cleaned, [True])
            self.assertEqual(ctx.peers, [])
            self.assertTrue((Path(root) / "lifecycle.json").exists())


class ContentFixtureTests(unittest.TestCase):
    def test_png_has_valid_chunks_crc_and_one_rgba_scanline(self):
        data = png()
        self.assertEqual(data[:8], b"\x89PNG\r\n\x1a\n")
        cursor, tags = 8, []
        while cursor < len(data):
            size = struct.unpack(">I", data[cursor:cursor+4])[0]
            tag, value = data[cursor+4:cursor+8], data[cursor+8:cursor+8+size]
            crc = struct.unpack(">I", data[cursor+8+size:cursor+12+size])[0]
            self.assertEqual(crc, zlib.crc32(tag + value))
            tags.append(tag)
            if tag == b"IDAT":
                self.assertEqual(zlib.decompress(value), b"\x00" * 5)
            cursor += 12 + size
        self.assertEqual(tags, [b"IHDR", b"IDAT", b"IEND"])
        self.assertEqual(cursor, len(data))

    def test_sender_header_does_not_force_a_native_block_layout(self):
        expected = [{"type": "text", "text": "first"}, {"type": "text", "text": "second"}]
        compare_content(expected, [{"type": "text", "text": "sender: A\nfirst"}, expected[1]], sender_header=True)
        compare_content(expected, [{"type": "text", "text": "sender: A"}, *expected], sender_header=True)
        with self.assertRaises(CheckFailed):
            compare_content(expected, [{"type": "text", "text": "sender: A\nfirstsecond"}], sender_header=True)
        with self.assertRaises(CheckFailed):
            compare_content(expected, [{"type": "text", "text": "first changed"}, expected[1]], sender_header=True)
        with self.assertRaises(CheckFailed):
            compare_content(expected, [{"type": "text", "text": "unexpected header"}, *expected])
        image = examples()["image"]
        compare_content([image], [{"type": "text", "text": "sender: A"}, image], sender_header=True)

    def test_audio_is_a_valid_small_pcm_wave_not_fake_base64(self):
        with wave.open(io.BytesIO(wav()), "rb") as source:
            self.assertEqual((source.getnchannels(), source.getsampwidth(), source.getframerate()), (1, 2, 8000))
            self.assertEqual(source.readframes(8), b"\0\0" * 8)
        for kind in ("image", "audio"):
            self.assertTrue(base64.b64decode(examples()[kind]["data"], validate=True))


class ResultTests(unittest.TestCase):
    def test_complete_inventory_has_every_normative_group(self):
        self.assertEqual([case.id for case in CASES], main_contract().ids)
        self.assertEqual(len(CASES), 62)
        self.assertEqual(len(BY_ID), len(CASES))
        self.assertIn("M8", BY_ID)
        self.assertIn("O11", BY_ID)
        with self.assertRaises(ValueError):
            select_cases(["C99"])

    def test_old_catalog_cannot_hide_missing_normative_cases(self):
        missing = {"C11", "C12", "D16", "D17", "O12", "O13", "O14"}
        old = [case for case in CASES if case.id not in missing]
        rows = [Result(case.id, case.title, "pass") for case in old]
        document = report({}, rows, old, inventory=old)
        self.assertFalse(document["complete_inventory"])
        self.assertFalse(document["passed_selection"])
        self.assertEqual(set(document["inventory_missing"]), missing)
        self.assertEqual(set(document["not_selected"]), missing)

    def test_contract_ranges_and_unverified_exports(self):
        text = "## A3. Tests\n**C1–C12** **D1–D17** **O1–O14** **R1–R7** **M1–M8** **L1–L4**\n## A4. Done"
        self.assertEqual(part_a_ids(text), [case.id for case in CASES])
        contract = Contract(text, None, "bundled-unverified")
        rows = [Result(case.id, case.title, "pass") for case in CASES]
        document = report({}, rows, list(CASES), contract=contract)
        self.assertFalse(document["complete_inventory"])
        self.assertFalse(document["normative_contract"]["main_verified"])
        with self.assertRaises(ValueError):
            part_a_ids("no normative test boundaries")
        with self.assertRaises(ValueError):
            part_a_ids(text.replace("C1–C12", "C1–D12"))

    def test_definition_scan_ignores_plain_ids_and_ranges_in_prose(self):
        text = "## A3. Tests\n**C1** core. **D1-D5** modes. **O1** observe. **R1** request. **M1** module. **L1** life.\n"
        text += "C99 is only a reference. See D98–D99, O87, R76, M65 and L54.\n## A4. Done\n**C100** outside A3"
        self.assertEqual(part_a_ids(text), ["C1", "D1", "D2", "D3", "D4", "D5", "O1", "R1", "M1", "L1"])
        with self.assertRaises(ValueError):
            part_a_ids(text.replace("**", ""))

    def test_duplicate_or_overlapping_bold_definitions_are_refused(self):
        text = "## A3. Tests\n**C1** **D1–D5** **O1** **R1** **M1** **L1**\n## A4. Done"
        for duplicate in ("**C1**", "**D3**", "**D4–D6**"):
            with self.subTest(duplicate=duplicate), self.assertRaisesRegex(ValueError, "duplicate"):
                part_a_ids(text.replace("## A4.", duplicate + "\n## A4."))

    def test_each_missing_definition_marker_refuses_before_fixture_effects(self):
        from agent_conformance.runner import run_selection
        text = "## A3. Tests\n" + " ".join("**" + case.id + "** definition." for case in CASES) + "\n## A4. Done"
        with tempfile.TemporaryDirectory(prefix="k1-marker-") as temp:
            root = Path(temp) / "must-not-start"
            for case in CASES:
                broken = text.replace("**" + case.id + "**", case.id)
                with self.subTest(case=case.id), patch("agent_conformance.inventory.main_contract", return_value=Contract(broken, "a" * 40, "git-main")), \
                     patch("agent_conformance.runner.run_case", new_callable=AsyncMock) as execute:
                    with self.assertRaisesRegex(HarnessGap, "inventory differs"):
                        asyncio.run(run_selection(root, {}, [], Path("/unused"), Path("/unused")))
                    execute.assert_not_awaited()
                    self.assertFalse(root.exists())

    def test_partial_pass_never_claims_complete_or_real_certification(self):
        selected = select_cases(["C1"])
        document = report({"name": "test-only"}, [Result("C1", "test", "pass")], selected)
        self.assertTrue(document["passed_selection"])
        self.assertFalse(document["complete_inventory"])
        self.assertFalse(document["real_runtime_certification"])
        self.assertEqual(len(document["not_selected"]), 61)

    def test_part_b_has_its_own_inventory_and_table_heading(self):
        inventory = (Case("I1", "durable claim"), Case("H1", "restart paused"))
        selected = select_cases(["I1"], inventory=inventory)
        document = report({"name": "coord-fixture"}, [Result("I1", "durable claim", "pass")], selected,
                          part="B", inventory=inventory)
        self.assertEqual(document["not_selected"], ["H1"])
        self.assertFalse(document["complete_inventory"])
        self.assertIn("Part B", markdown(document))
        with self.assertRaises(ValueError):
            select_cases(["C1"], inventory=inventory)

    def test_unsupported_requires_a_real_omission_record(self):
        with self.assertRaises(ValueError):
            Result("D1", "test", "unsupported-declared")

    def test_not_applicable_requires_the_named_excluding_declaration(self):
        for declarations in ({}, {"exclusion": {"path": "host.kind"}}):
            with self.assertRaises(ValueError):
                Result("C10", "excluded", "not-applicable", declarations=declarations)

    def test_not_applicable_never_counts_as_a_pass(self):
        result = Result("C10", "excluded", "not-applicable",
                        declarations={"exclusion": {"path": "host.kind", "actual": "server"}})
        document = report({"name": "test-only"}, [result], select_cases(["C10"]))
        self.assertEqual(document["summary"]["not-applicable"], 1)
        self.assertEqual(document["summary"]["pass"], 0)
        self.assertFalse(document["passed_selection"])

    def test_provider_superset_uses_enum_sets_and_numeric_maxima(self):
        upper = {"modes": {"settle": {"batch": ["one", "all"], "idle": ["start"]}}, "retention": 512, "native": True}
        lower = {"modes": {"settle": {"batch": "one", "idle": "start"}}, "retention": 256, "native": False}
        self.assertTrue(restriction(lower, upper))
        lower["modes"]["settle"]["batch"] = "all"
        self.assertTrue(restriction(lower, upper))
        self.assertFalse(restriction(lower, {}))  # empty is not unrestricted
        self.assertFalse(restriction({"retention": 513}, upper))
        self.assertFalse(restriction({"modes": {"start": {}}}, upper))
        self.assertFalse(restriction({"batch": "one"}, {"batch": "one"}))


class SchemaFixtureTests(unittest.TestCase):
    def test_write_only_is_an_annotation_not_an_instance_constraint(self):
        self.assertEqual(schema_example({"type": "string", "minLength": 3, "writeOnly": True}), "xxx")
        with self.assertRaises(CheckFailed):
            schema_example({"type": "string", "writeOnly": "true"})

    def test_valid_simple_form_and_bounded_generation(self):
        self.assertEqual(schema_example({"type": "object", "required": ["a"], "properties": {
            "a": {"type": "string", "enum": ["one", "two"]}}, "additionalProperties": False}), {"a": "one"})
        for schema in ({"type": "string", "minLength": 10**9}, {"type": "array", "minItems": 10**9},
                       {"type": "string", "pattern": "[0-9]+"}):
            with self.subTest(schema=schema), self.assertRaises(CheckFailed):
                schema_example(schema)


class RetentionRecoveryTests(unittest.IsolatedAsyncioTestCase):
    def view(self):
        return SimpleNamespace(reducer=seeded(), closed=None, close_notice=None)

    async def test_gap_kind_and_last_good_cursor_survive_following_closed_notice(self):
        peer, view = object(), self.view()
        ctx = object.__new__(PublicImplementation)
        ctx.views = {(id(peer), "s"): view}
        before = dict(view.reducer.cursor)
        ctx._notice(peer, "stream.gap", {"stream": "s", "reason": {"reason": "gap", "gap": "retention"}})
        ctx._notice(peer, "stream.closed", {"stream": "s", "reason": {"reason": "closed"}})
        self.assertEqual(view.closed, "retention")
        self.assertEqual(view.close_notice["last_good_cursor"], before)
        self.assertEqual(view.close_notice["reason"], {"reason": "gap", "gap": "retention"})
        self.assertEqual(view.reducer.cursor, before)

    async def test_legal_gap_never_overwrites_prior_integrity_failure(self):
        view = self.view()
        view.reducer.failure = "bad_record"
        view.reducer.ended("retention")
        self.assertEqual(view.reducer.failure, "bad_record")

    async def test_retention_replays_once_but_corruption_never_auto_replays(self):
        from agent_conformance.cases import wait_with_retention_replay
        for failure in ("retention", "bad_record"):
            view, recovered = self.view(), self.view()
            view.closed, view.reducer.failure = "retention", failure
            recovered.initial = "reset"
            ctx = SimpleNamespace(evidence=[], viewer=AsyncMock(), attach=AsyncMock(return_value=recovered),
                                  wait_view=AsyncMock(side_effect=[CheckFailed("gap"), None, None]))
            fresh = ctx.attach
            with self.subTest(failure=failure), patch("agent_conformance.cases.fresh_view", fresh):
                if failure == "retention":
                    self.assertIs(await wait_with_retention_replay(ctx, view, lambda r: True), recovered)
                    self.assertEqual(ctx.evidence[-1]["last_good_cursor"], view.reducer.cursor)
                    self.assertEqual(ctx.evidence[-1]["attempts"], 1)
                    self.assertEqual(fresh.await_count, 1)
                else:
                    with self.assertRaises(CheckFailed):
                        await wait_with_retention_replay(ctx, view, lambda r: True)
                    self.assertEqual(fresh.await_count, 0)


class RecoveryCutScopeTests(unittest.IsolatedAsyncioTestCase):
    async def test_pressure_that_invalidates_recovery_is_a_fixture_gap_not_adapter_corruption(self):
        from agent_conformance.cases import wait_with_retention_replay
        reducer = seeded()
        reducer.failure = "retention"
        view = SimpleNamespace(reducer=reducer, closed="retention")
        recovered = SimpleNamespace(reducer=seeded(), closed="retention", initial="reset", close_notice={"reason": "retention"})
        recovered.reducer.failure = "retention"
        ctx = SimpleNamespace(evidence=[], viewer=AsyncMock(), attach=AsyncMock(return_value=recovered),
                              wait_view=AsyncMock(side_effect=[CheckFailed("retention"), CheckFailed("retention")]))
        with self.assertRaises(HarnessGap):
            await wait_with_retention_replay(ctx, view, lambda r: True)
        self.assertEqual(ctx.attach.await_count, 1)


class StalledAttachmentWitnessTests(unittest.TestCase):
    def view(self, reason=None, failure=None):
        return SimpleNamespace(closed=reason, peer=SimpleNamespace(failure=failure), reducer=SimpleNamespace(failure=reason))

    def test_no_retirement_is_not_a_pass_even_when_fast_viewers_advance(self):
        from agent_conformance.cases import stalled_attachment_witness
        with self.assertRaises(HarnessGap):
            stalled_attachment_witness(self.view(), self.view(), self.view())

    def test_only_stalled_stream_closes_socket_and_fast_views_stay_alive(self):
        from agent_conformance.cases import stalled_attachment_witness
        first, second, stalled = self.view(), self.view(), self.view("slow_consumer")
        stalled_attachment_witness(first, second, stalled)
        for bad in ("fast", "socket", "corruption"):
            first, second, stalled = self.view(), self.view(), self.view("slow_consumer")
            if bad == "fast": first.closed = "slow_consumer"
            elif bad == "socket": stalled.peer.failure = WireFailure("EOF")
            else: stalled.reducer.failure = "bad_record"
            with self.subTest(bad=bad), self.assertRaises(CheckFailed):
                stalled_attachment_witness(first, second, stalled)


class CoreOriginBindingTests(unittest.IsolatedAsyncioTestCase):
    async def test_normal_origin_uses_only_its_connection_core_hello_identity(self):
        ctx = object.__new__(PublicImplementation)
        ctx.facts = AsyncMock(return_value={"caps": {"agent.send": {"modes": {"start": {}}}}})
        ctx.delivery, ctx.invoke = lambda: "d1", AsyncMock(return_value={})
        ctx.peer = SimpleNamespace(core_origin={"principal": "p1", "actor": "extension:coord", "initiator": "p2"})
        await ctx.send("text", origin={"kind": "agent", "agent": "claimed-agent"})
        self.assertEqual(ctx.invoke.call_args.args[1]["origin"], {"kind": "agent", "agent": "claimed-agent", "principal": "p1", "actor": "extension:coord", "initiator": "p2"})
        ctx.peer = SimpleNamespace(author="owner", core_origin={})
        await ctx.send("text")
        self.assertEqual(ctx.invoke.call_args.args[1]["origin"], {"kind": "human"})


class CausalItemWitnessTests(unittest.TestCase):
    def test_uncorrelated_inserted_item_must_be_observed_without_attribution(self):
        from agent_conformance.cases import inserted_causality_witness
        item = (("message", "native-m1"), {"role": "user", "content": [], "final": True})
        for strength in ("none", "acceptance"):
            self.assertTrue(inserted_causality_witness(strength, [item], [])["no_attribution_inferred"])
        with self.assertRaises(CheckFailed): inserted_causality_witness("none", [], [])
        with self.assertRaises(CheckFailed):
            inserted_causality_witness("none", [(("message", "m1"), {"role": "user", "content": [], "final": False})], [])

    def test_receipt_echo_cannot_hide_wrong_or_inferred_message_cause(self):
        from agent_conformance.cases import inserted_causality_witness
        for link in ({"cause": {"kind": "effect", "id": "wrong", "provenance": "inferred"}},
                     {"delivery": "guessed-from-text"}, {"correlation": "guessed-from-order"}):
            with self.subTest(link=link), self.assertRaises(CheckFailed):
                inserted_causality_witness("none", [(("message", "m1"), {"role": "user", "content": [], "final": True, **link})], [])

    def test_declared_native_correlation_needs_a_genuine_native_id_fixture(self):
        from agent_conformance.cases import inserted_causality_witness
        with self.assertRaises(HarnessGap):
            inserted_causality_witness("native", [(("message", "m1"), {"delivery": "d1"})], [])

    def test_unrelated_final_item_cannot_witness_this_send(self):
        from agent_conformance.cases import inserted_causality_witness
        expected = [{"type": "text", "text": "our native insertion"}]
        unrelated = (("message", "other"), {"role": "user", "final": True,
                                            "content": [{"type": "text", "text": "another sender"}]})
        with self.assertRaisesRegex(CheckFailed, "matching its native insertion"):
            inserted_causality_witness("none", [unrelated], expected)
        ours = (("message", "ours"), {"role": "user", "final": True, "content": expected})
        result = inserted_causality_witness("none", [unrelated, ours], expected)
        self.assertEqual(result["items"], [["message", "ours"]])
        self.assertTrue(result["native_link_unprovable"])

    def test_partial_or_duplicate_payload_match_is_not_unambiguous(self):
        from agent_conformance.cases import inserted_causality_witness
        expected = [{"type": "text", "text": "exact native body"}]
        partial = (("message", "p"), {"role": "user", "final": False, "content": expected})
        first = (("message", "a"), {"role": "user", "final": True, "content": expected})
        second = (("message", "b"), {"role": "user", "final": True, "content": expected})
        for candidates in ([partial], [first, second]):
            with self.subTest(candidates=candidates), self.assertRaises(CheckFailed):
                inserted_causality_witness("acceptance", candidates, expected)

    def test_native_content_witness_preserves_framing_and_block_order(self):
        from agent_conformance.cases import native_insertion_content, inserted_causality_witness
        expected = [{"type": "text", "text": "sender header"}, {"type": "text", "text": "literal body"}]
        self.assertIs(native_insertion_content({"content": expected}), expected)
        self.assertEqual(native_insertion_content({"text": "framed body"}), [{"type": "text", "text": "framed body"}])
        with self.assertRaises(HarnessGap): native_insertion_content({"texts": ["not the canonical witness"]})
        for wrong in (list(reversed(expected)), [{"type": "text", "text": "sender headerliteral body"}]):
            with self.subTest(wrong=wrong), self.assertRaises(CheckFailed):
                inserted_causality_witness("none", [(("message", "m"), {"role": "user", "final": True, "content": wrong})], expected)


class CommandWitnessTests(unittest.TestCase):
    def proof(self, args="exact instructions"):
        return ([{"command": "compact", "value": {"type": "compact", "id": "n1", "customInstructions": args}}],
                [{"name": "compact", "request": "n1", "custom_instructions": args}],
                [{"command": "compact", "request": "n1", "success": True}])

    def test_exact_native_command_arguments_execution_and_result_are_required(self):
        from agent_conformance.cases import command_witness
        writes, executions, replies = self.proof()
        self.assertEqual(command_witness("pi", "compact", "exact instructions", writes, executions, replies)["native_request"], "n1")
        for part in ("operation", "args", "execution", "result"):
            writes, executions, replies = self.proof()
            if part == "operation": writes = [{"operation": "send", "text": "WRONG - not compact"}]
            elif part == "args": writes[0]["value"]["customInstructions"] = "different"
            elif part == "execution": executions = []
            else: replies[0]["request"] = "another-command"
            with self.subTest(part=part), self.assertRaises(CheckFailed):
                command_witness("pi", "compact", "exact instructions", writes, executions, replies)

    def test_unimplemented_native_command_driver_is_a_harness_gap(self):
        from agent_conformance.cases import native_command_expectation
        with self.assertRaises(HarnessGap): native_command_expectation("acp", "compact", "")


class MemoryWitnessTests(unittest.IsolatedAsyncioTestCase):
    async def test_rss_samples_and_small_entities_cannot_replace_public_gap_witness(self):
        from agent_conformance.cases import public_retention_bounds
        from test_agent_conformance_retention_gap import PressureContext, modeled_shared_B
        # Reuse the current full-handler model, not the retired single-view fixture.
        # B is explicitly modeled here; actual O7 execution is separate evidence.
        ctx = PressureContext(gap=False)
        module = "agent_conformance.retention_gap_cases."
        with patch(module + "sample_snapshot", AsyncMock(side_effect=[
                {"entities": 0, "entity_bytes": 0} for _ in range(4)])), patch(
                module + "process_rss", side_effect=[1 << 20, 100 << 20, 200 << 20, 300 << 20]), patch(
                module + "one_replay", new_callable=AsyncMock) as replay, patch(
                "agent_conformance.cases.stalled_viewer_isolation", modeled_shared_B):
            with self.assertRaisesRegex(HarnessGap, "no actual provider-cursor retention gap"):
                await public_retention_bounds(ctx)
            replay.assert_not_awaited()
        samples = next(row["samples"] for row in ctx.evidence if row.get("kind") == "retention_pressure")
        self.assertEqual(len(samples), 4)
        self.assertEqual([row["adapter_rss_bytes"] for row in samples],
                         [1 << 20, 100 << 20, 200 << 20, 300 << 20])
        self.assertTrue(all(row["entities"] == row["entity_bytes"] == 0 for row in samples))
        subcases = {row["case"]: row["status"] for row in ctx.evidence if row.get("kind") == "subcase"}
        self.assertEqual(subcases, {"O10.A.provider_retention": "fail", "O10.B.slow_viewer": "pass"})
        self.assertEqual(ctx.B_calls, 1)
        self.assertEqual(len(ctx.after), 1)  # Only modeled B recovered; A never replayed.

    async def test_o10_needs_both_public_and_internal_accounting_witnesses(self):
        from agent_conformance.cases import declared_retention_bounds
        for failed in ("public_wire", "producer_accounting"):
            ctx = SimpleNamespace(evidence=[])
            public = AsyncMock(side_effect=HarnessGap("missing public gap") if failed == "public_wire" else None)
            with self.subTest(failed=failed), patch("agent_conformance.cases.public_retention_bounds", public), patch(
                    "agent_conformance.evidence.retained_memory_accounting", side_effect=HarnessGap("missing journal proof") if failed == "producer_accounting" else None):
                with self.assertRaises(HarnessGap):
                    await declared_retention_bounds(ctx)
            self.assertEqual({row["case"]: row["status"] for row in ctx.evidence},
                             {"O10.public_wire": "fail" if failed == "public_wire" else "pass",
                              "O10.producer_accounting": "fail" if failed == "producer_accounting" else "pass"})


class HandledDeclarationTests(unittest.IsolatedAsyncioTestCase):
    def context(self, stage="admitted", consumed=False):
        return SimpleNamespace(evidence=[], facts=AsyncMock(return_value={"caps": {"agent.send": {"handled": "none"}}}),
            send=AsyncMock(return_value={"outcome": "accepted", "data": {"receipt": {"delivery": "d1", "stage": stage}}}),
            native=SimpleNamespace(records=lambda kind: [{}] if consumed and kind == "hook_consumed" else [], call=AsyncMock()),
            wait_facts=AsyncMock())

    async def test_negative_branch_does_not_require_an_invented_native_hook(self):
        from agent_conformance.cases import consumed_input
        ctx = self.context()
        with patch("agent_conformance.cases.fresh_view", AsyncMock(return_value=SimpleNamespace(reducer=SimpleNamespace(entities={})))):
            await consumed_input(ctx)
        self.assertEqual(ctx.evidence[0]["status"], "not-applicable")
        self.assertEqual(ctx.evidence[0]["exclusion"], {"path": "agent.send.handled", "actual": "none"})
        self.assertFalse(ctx.evidence[-1]["native_consumption_witnessed"])

    async def test_none_never_reports_handled_and_consumed_never_reports_inserted(self):
        from agent_conformance.cases import consumed_input
        for stage, consumed in (("handled", False), ("inserted", True)):
            with self.subTest(stage=stage), self.assertRaises(CheckFailed):
                await consumed_input(self.context(stage, consumed))


class DeclarationTests(unittest.IsolatedAsyncioTestCase):
    async def test_valid_single_endpoint_declaration_and_provider_enum_sets_pass(self):
        send = {"modes": {"start": {"busy": "reject", "preflight": "serialized"}},
                "content": {"types": ["text", "image"], "shape": "text_then_media"},
                "guards": {"session": "adapter", "run": "none"}, "receipts": {"retain": 256},
                "correlation": "none", "literal": "controlled_launch", "handled": "none", "keys": None,
                "origin": [{"as": "attributed_user", "role": "user", "frame": "sender_header"}]}
        lower = {"host": {"kind": "terminal", "endpoints": "one"}, "agent.send": send, "items.stream": {}}
        def upper(value):
            if isinstance(value, dict):
                return {key: upper(item) for key, item in value.items()}
            if isinstance(value, list):
                return [upper(item) if isinstance(item, dict) else item for item in value]
            return [value] if isinstance(value, str) else value
        provider = {"host": "r1", "capabilities": {"agent": {"v": 0, "modules": {}}, **upper(lower)}}
        facts = {"v": 0, "epoch": "e1", "state": "idle", "lifecycle": "live", "completion": None, "incomplete": [],
                 "host": {"id": "r1", "kind": "terminal", "owns_lifecycle": True}, "caps": {"rev": 1, **lower}}
        ctx = SimpleNamespace(host="r1", provider=provider, evidence=[],
                              agents=[{"id": "r2", "status": {"state": "idle"}, "data": {"agent": facts}}])
        await declarations(ctx)
        self.assertTrue(ctx.evidence)
        provider["capabilities"]["agent.send"] = {}
        with self.assertRaisesRegex(CheckFailed, "exceeds"):
            await declarations(ctx)


class AuditHistoryTests(unittest.TestCase):
    def test_facts_are_selected_by_commit_order_not_observer_delivery_time(self):
        ctx = PublicImplementation.__new__(PublicImplementation)
        before = {"id": "r1", "data": {"agent": {"session": None}}}
        after = {"id": "r1", "data": {"agent": {"session": {"id": "new-session"}}}}
        ctx.resource_history = [("boot", 1, before, 1), ("boot", 3, after, 1)]
        self.assertIsNone(ctx.facts_at_audit("r1", {"epoch": "boot", "seq": 2})["session"])
        self.assertEqual(ctx.facts_at_audit("r1", {"epoch": "boot", "seq": 4})["session"]["id"], "new-session")

    def test_audit_history_does_not_cross_core_epochs(self):
        ctx = PublicImplementation.__new__(PublicImplementation)
        ctx.resource_history = [("old-boot", 1, {"id": "r1", "data": {"agent": {}}}, 1)]
        with self.assertRaises(HarnessGap):
            ctx.facts_at_audit("r1", {"epoch": "new-boot", "seq": 2})


class SessionBootstrapTests(unittest.IsolatedAsyncioTestCase):
    def context(self, visible=True):
        state = {"session": None, "caps": {"agent.session": {"actions": ["new"]}}}
        calls = []
        async def facts(rid=None):
            return state
        async def invoke(op, args, rid=None):
            calls.append((op, args))
            if visible:
                state["session"] = {"id": "native-s1"}
            return {"outcome": "observed"}
        return SimpleNamespace(facts=facts, invoke=invoke, agents=[{"id": "r2"}], evidence=[]), calls

    async def test_sessionless_fixture_bootstraps_without_a_prompt(self):
        ctx, calls = self.context()
        result = await PublicImplementation.ensure_session(ctx)
        self.assertEqual(result["session"]["id"], "native-s1")
        await PublicImplementation.ensure_session(ctx)
        self.assertEqual(calls, [("agent.session", {"action": "new"})])
        self.assertFalse(ctx.evidence[0]["native_input_sent"])

    async def test_session_effect_must_publish_facts_before_reply(self):
        ctx, _ = self.context(visible=False)
        with self.assertRaisesRegex(CheckFailed, "before its facts"):
            await PublicImplementation.ensure_session(ctx)


class ClassificationTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="vq-k1-")
        self.root = Path(self.temp.name) / "case"
        self.manifest = {"name": "harness-self-test", "argv": ["/fixture"]}

    async def asyncTearDown(self):
        self.temp.cleanup()

    def factory(self, *, declared_send=True, startup_error=None, cleanup_error=None, modules=None):
        class Fake:
            def __init__(self, root, manifest, vqro, hc, script=None):
                root.mkdir()
                self.evidence, self.metadata = [], {"test": True}
                self.provider = {"capabilities": {"agent": {"v": 0}}}
                self.closed = False
            async def start(self):
                if startup_error:
                    raise startup_error
            async def facts(self):
                caps = {"agent.send": {}} if declared_send else {}
                if modules is not None:
                    caps["agent"] = {"modules": modules}
                return {"v": 0, "caps": caps}
            async def ensure_session(self):
                return await self.facts()
            async def close(self):
                self.closed = True
                if cleanup_error:
                    raise cleanup_error
        return Fake

    async def run_one(self, **kwargs):
        return await run_case(BY_ID["D6"], self.root, self.manifest, Path("/vqro"), Path("/hc"), **kwargs)

    async def test_declared_but_unimplemented_is_harness_failure(self):
        result = await self.run_one(factory=self.factory(), handlers={})
        self.assertEqual((result.status, result.component), ("fail", "harness"))
        self.assertEqual(result.reason, "harness_gap")

    async def test_new_normative_rows_stay_visible_as_harness_gaps(self):
        for ident in ("C11", "C12", "D16", "D17", "O12", "O13", "O14"):
            with self.subTest(case=ident):
                result = await run_case(BY_ID[ident], Path(self.temp.name) / ident, self.manifest,
                                        Path("/vqro"), Path("/hc"), factory=self.factory(), handlers={})
                self.assertEqual((result.status, result.component, result.reason), ("fail", "harness", "harness_gap"))

    async def test_absent_observation_runner_remains_a_harness_failure(self):
        result = await run_case(BY_ID["O7"], Path(self.temp.name) / "missing-observer", self.manifest,
                                Path("/vqro"), Path("/hc"), factory=self.factory(), handlers={})
        self.assertEqual((result.status, result.component, result.reason), ("fail", "harness", "harness_gap"))

    async def test_only_absent_optional_declaration_is_unsupported(self):
        result = await self.run_one(factory=self.factory(declared_send=False), handlers={})
        self.assertEqual(result.status, "unsupported-declared")
        self.assertEqual(result.declarations["omission"]["path"], "agent.send")

    async def test_terminal_module_without_invented_operation_requires_handler(self):
        result = await run_case(BY_ID["M7"], self.root, self.manifest, Path("/vqro"), Path("/hc"),
                                factory=self.factory(modules={"terminals": {"v": 0}}), handlers={})
        self.assertEqual((result.status, result.component), ("fail", "harness"))

    async def test_absent_terminal_module_is_explicit_omission(self):
        result = await run_case(BY_ID["M8"], self.root, self.manifest, Path("/vqro"), Path("/hc"),
                                factory=self.factory(), handlers={})
        self.assertEqual(result.status, "unsupported-declared")
        self.assertEqual(result.declarations["omission"]["path"], "agent.modules.terminals")

    async def test_children_module_does_not_require_object_cancel_operation(self):
        result = await run_case(BY_ID["M6"], self.root, self.manifest, Path("/vqro"), Path("/hc"),
                                factory=self.factory(modules={"children": {"v": 0}}), handlers={})
        self.assertEqual((result.status, result.component), ("fail", "harness"))

    async def test_declaration_selection_exclusion_is_not_a_harness_gap(self):
        async def excluded(ctx):
            raise NotApplicable("host.endpoints", "one")
        result = await self.run_one(factory=self.factory(), handlers={"D6": excluded})
        self.assertEqual(result.status, "not-applicable")
        self.assertEqual(result.declarations["exclusion"], {"path": "host.endpoints", "actual": "one"})

    async def test_fixture_failure_is_not_capability_omission(self):
        result = await self.run_one(factory=self.factory(startup_error=FixtureFailed("no fake control")), handlers={})
        self.assertEqual((result.status, result.component), ("fail", "fixture"))

    async def test_adapter_assertion_remains_adapter_failure(self):
        async def overclaims(ctx):
            raise CheckFailed("native success is not insertion")
        result = await self.run_one(factory=self.factory(), handlers={"D6": overclaims})
        self.assertEqual((result.status, result.component), ("fail", "adapter"))

    async def test_cleanup_failure_cannot_be_hidden_by_passing_case(self):
        async def passed(ctx):
            return None
        result = await self.run_one(factory=self.factory(cleanup_error=RuntimeError("owned child remains")), handlers={"D6": passed})
        self.assertEqual((result.status, result.component), ("fail", "cleanup"))
        self.assertEqual(result.evidence[-1]["status"], "pass")


@unittest.skipIf(sys.platform == "win32", "native conformance control fixture uses Unix-domain endpoints")
class NativeFixtureTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="vq-k1-")
        self.root = Path(self.temp.name)
        (self.root / "home").mkdir()
        self.control_path, self.ledger = self.root / "c", self.root / "ledger.jsonl"
        self.process = await asyncio.create_subprocess_exec(sys.executable, str(RUNTIME), "--control", str(self.control_path),
            "--ledger", str(self.ledger), "--mode", "rpc", "--no-extensions", "--no-skills", "--no-prompt-templates",
            stdin=asyncio.subprocess.PIPE, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE,
            env={"HOME": str(self.root / "home"), "PATH": "/usr/bin:/bin", "LANG": "C.UTF-8", "PYTHONDONTWRITEBYTECODE": "1"})
        async def ready():
            while not self.control_path.exists():
                if self.process.returncode is not None:
                    self.fail((await self.process.stderr.read()).decode())
                await asyncio.sleep(.005)
        await asyncio.wait_for(ready(), 2)
        self.command_serial = 0

    async def asyncTearDown(self):
        if self.process.returncode is None:
            self.process.terminate()
        await asyncio.wait_for(self.process.wait(), 2)
        for stream in (self.process.stdout, self.process.stderr):
            await stream.read()
        if self.process.stdin is not None:
            self.process.stdin.close()
        self.temp.cleanup()

    def records(self, kind=None):
        rows = [json.loads(raw) for raw in self.ledger.read_bytes().split(b"\n") if raw]
        return rows if kind is None else [r for r in rows if r["kind"] == kind]

    async def control(self, verb, **params):
        reader, writer = await asyncio.open_unix_connection(str(self.control_path))
        try:
            writer.write(line({"id": 1, "verb": verb, **params}))
            await writer.drain()
            reply = json.loads(await asyncio.wait_for(reader.readline(), 2))
            if "error" in reply:
                raise ValueError(reply["error"]["message"])
            return reply["result"]
        finally:
            writer.close()
            await writer.wait_closed()

    async def send_native(self, command):
        self.command_serial += 1
        command = {"id": str(self.command_serial), **command}
        self.process.stdin.write(line(command))
        await self.process.stdin.drain()
        return command["id"]

    async def response(self, ident):
        async def reading():
            while raw := await self.process.stdout.readline():
                row = json.loads(raw)
                if row.get("type") == "response" and row.get("id") == ident:
                    return row
            raise EOFError("fixture exited before native response")
        return await asyncio.wait_for(reading(), 2)

    async def native(self, **params):
        return await self.response(await self.send_native(params))

    async def wait_ledger(self, predicate):
        async def waiting():
            while True:
                if result := predicate(self.records()):
                    return result
                await asyncio.sleep(.005)
        return await asyncio.wait_for(waiting(), 2)

    async def test_documented_native_cycle_and_compact_commands(self):
        cycled = await self.native(type="cycle_model")
        self.assertTrue(cycled["success"])
        self.assertEqual(cycled["data"]["model"]["id"], "fixture-large")
        state = await self.native(type="get_state")
        self.assertEqual(state["data"]["model"]["id"], "fixture-large")
        catalogue = await self.native(type="get_available_models")
        self.assertEqual([row["id"] for row in catalogue["data"]["models"]], ["fixture-small", "fixture-large"])
        compacted = await self.native(type="compact", customInstructions="fixture instructions")
        self.assertTrue(compacted["success"])
        self.assertEqual(compacted["data"]["summary"], "Fixture-only compaction summary")
        self.assertEqual([row["name"] for row in self.records("command_executed")], ["cycle_model", "compact"])
        self.assertFalse(self.records("input_received"))
        self.assertEqual([row["request"] for row in self.records("command_executed")], [cycled["id"], compacted["id"]])
        before = await self.control("state")
        created = await self.native(type="new_session")
        after = await self.control("state")
        self.assertNotEqual(before["session"], after["session"])
        self.assertEqual(self.records("command_executed")[-1], {**self.records("command_executed")[-1],
            "name": "new_session", "request": created["id"], "session": after["session"]})

    async def test_admission_is_not_insertion_and_native_environment_is_private(self):
        await self.control("pause_before", phase="insertion")
        reply = await self.native(type="prompt", message="/literal\r\n snow ☃ \u2028 end")
        self.assertTrue(reply["success"])
        self.assertEqual(len(self.records("input_received")), 1)
        self.assertEqual(len(self.records("inserted")), 0)
        await self.control("release", phase="insertion")
        await self.wait_ledger(lambda rows: any(r["kind"] == "inserted" for r in rows))
        self.assertEqual(self.records("inserted")[0]["text"], "/literal\r\n snow ☃ \u2028 end")
        ready = self.records("ready")[0]
        self.assertFalse(ready["bootstrap_in_child"])
        self.assertFalse(ready["herdr_in_child"])
        self.assertEqual(len(self.records("command")), 1)

    async def test_native_image_bytes_are_preserved_in_independent_content_oracle(self):
        image = examples()["image"]
        reply = await self.native(type="prompt", message="", images=[
            {"type": "image", "mimeType": image["mime"], "data": image["data"]}])
        self.assertTrue(reply["success"])
        self.assertEqual(self.records("input_received")[0]["content"], [image])

    async def test_idle_steer_is_parked_until_a_model_boundary(self):
        self.assertTrue((await self.native(type="steer", message="focus"))["success"])
        self.assertFalse(self.records("inserted"))
        self.assertEqual((await self.control("state"))["steering"], ["focus"])
        await self.control("tool_call", id_ref="tc1", state="start")
        await self.control("tool_call", id_ref="tc1", state="end")
        await self.control("model_call", id_ref="m2")
        inserted, model = self.records("inserted")[0], self.records("model_call")[0]
        tool_end = self.records("tool_call")[-1]
        self.assertLess(tool_end["seq"], inserted["seq"])
        self.assertLess(inserted["seq"], model["seq"])
        self.assertEqual(inserted["text"], "focus")

    async def test_batch_setting_is_sampled_at_consume(self):
        for index in range(3):
            await self.native(type="steer", message=f"focus-{index}")
        await self.native(type="set_steering_mode", mode="all")
        await self.control("model_call")
        self.assertEqual([r["text"] for r in self.records("inserted")], ["focus-0", "focus-1", "focus-2"])
        self.assertEqual((await self.control("state"))["steering"], [])

    async def test_preflight_reply_is_a_frozen_native_snapshot(self):
        await self.control("pause_after", phase="preflight")
        ident = await self.send_native({"type": "get_state"})
        await self.wait_ledger(lambda rows: any(r["kind"] == "paused" for r in rows))
        await self.control("turn", state="start")
        await self.control("release", phase="preflight")
        reply = await self.response(ident)
        self.assertFalse(reply["data"]["isStreaming"])
        self.assertTrue((await self.control("state"))["active"])

    async def test_unknown_control_is_a_fixture_error_not_an_adapter_skip(self):
        with self.assertRaisesRegex(ValueError, "unimplemented fixture verb"):
            await self.control("nonexistent")
        self.assertEqual((await self.control("state"))["wire"], "pi")

    async def test_neutral_ui_methods_and_cancel_are_not_false(self):
        await self.control("request", request="q-choice", method="choice")
        event = json.loads(await asyncio.wait_for(self.process.stdout.readline(), 2))
        self.assertEqual(event["method"], "select")
        self.assertNotIn("timeout", event)  # absent is not immediate expiry
        await self.send_native({"type": "extension_ui_response", "id": "q-choice", "value": "one"})
        await self.wait_ledger(lambda rows: any(row["kind"] == "answer" for row in rows))
        self.assertEqual(self.records("answer")[-1]["normalized_response"], {"choice": "one"})
        await self.control("request", request="q-false", method="confirm")
        await asyncio.wait_for(self.process.stdout.readline(), 2)
        await self.send_native({"type": "extension_ui_response", "id": "q-false", "confirmed": False})
        await self.wait_ledger(lambda rows: len([row for row in rows if row["kind"] == "answer"]) == 2)
        self.assertEqual(self.records("answer")[-1]["normalized_response"], {"value": False})
        await self.control("request", request="q-cancel", method="confirm")
        await asyncio.wait_for(self.process.stdout.readline(), 2)
        await self.send_native({"type": "extension_ui_response", "id": "q-cancel", "cancelled": True})
        await self.wait_ledger(lambda rows: len([row for row in rows if row["kind"] == "answer"]) == 3)
        self.assertEqual(self.records("answer")[-1]["normalized_response"], {"action": "cancel"})

    async def test_semantic_drop_reply_does_not_drop_or_repeat_native_input(self):
        await self.control("drop_reply", operation="send")
        await self.send_native({"type": "prompt", "message": "one uncertain native input"})
        await self.wait_ledger(lambda rows: any(row["kind"] == "inserted" for row in rows))
        self.assertEqual(len(self.records("reply_dropped")), 1)
        self.assertEqual(len(self.records("input_received")), 1)
        self.assertFalse(self.records("native_reply"))

    async def test_controlled_native_exit_has_no_synthetic_settled_event(self):
        await self.control("exit", code=23)
        self.assertEqual(await asyncio.wait_for(self.process.wait(), 2), 23)
        self.assertFalse(any(r.get("state") == "settled" for r in self.records("turn")))
        self.assertFalse(self.control_path.exists())


if __name__ == "__main__":
    unittest.main()
