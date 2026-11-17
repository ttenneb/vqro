"""Standalone Pi runtime regressions. No core, server, provider or credentials."""
import asyncio
import os
from pathlib import Path
import sys
import tempfile
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
from pi_bridge import Limits, PiRpcClient

FIXTURE = Path(__file__).parent / "fixtures" / "fake_pi.py"


class PiBridgeTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.root = tempfile.TemporaryDirectory(prefix="prim-pi-unit-")
        self.clients = []
        self.client = await self.start()

    async def start(self, fixture_args=(), **kwargs):
        env = {"HOME": self.root.name, "PATH": "/usr/bin:/bin", "LANG": "C.UTF-8"}
        c = await PiRpcClient.start([sys.executable, str(FIXTURE), *fixture_args], cwd=self.root.name, env=env, **kwargs)
        self.clients.append(c)
        return c

    async def asyncTearDown(self):
        for client in self.clients:
            await client.close()
            self.assertIsNotNone(client.process.returncode)
        self.root.cleanup()

    async def barrier(self):
        self.assertEqual((await self.client.request("get_state"))["outcome"], "accepted")

    async def test_acceptance_is_not_completion_and_agent_end_is_not_settled(self):
        result = await self.client.invoke("prompt", {"message": "start"})
        self.assertEqual(result["data"], {"disposition": "started"})
        facts = await self.client.wait_for_facts(lambda f: f["run"]["active"] is True)
        self.assertFalse(facts["run"]["settled"])
        await self.client.request("test_end")
        self.assertTrue(self.client.snapshot()["run"]["active"])
        self.assertFalse(self.client.snapshot()["run"]["settled"])
        await self.client.invoke("abort")
        self.assertEqual(self.client.snapshot()["run"], {"active": False, "settled": True, "aborted": True})

    async def test_normal_settled_marks_done_only_after_native_settled_event(self):
        await self.client.invoke("prompt", {"message": "start"})
        await self.client.request("test_end")
        self.assertEqual(self.client.snapshot()["status"], "working")
        await self.client.request("test_settle")
        facts = self.client.snapshot()
        self.assertEqual(facts["status"], "done")
        self.assertEqual(facts["run"], {"active": False, "settled": True, "aborted": False})

    async def test_handled_prompt_does_not_invent_a_run(self):
        result = await self.client.invoke("prompt", {"message": "handled"})
        self.assertEqual(result["data"]["disposition"], "handled")
        self.assertIsNone(self.client.snapshot()["run"]["active"])

    async def test_steer_during_tool_calls_is_queued_then_consumed_at_boundary(self):
        await self.client.invoke("prompt", {"message": "start"})
        result = await self.client.invoke("steer", {"message": "direction"})
        self.assertEqual(result["data"]["disposition"], "queued")
        self.assertEqual(self.client.snapshot()["queues"]["steering"], ["direction"])
        self.assertTrue(self.client.snapshot()["run"]["active"])
        await self.client.request("test_boundary")
        self.assertEqual(self.client.snapshot()["queues"]["steering"], [])

    async def test_abort_preserves_queued_work_and_clear_queue_prevents_continuation(self):
        await self.client.invoke("prompt", {"message": "start"})
        await self.client.invoke("steer", {"message": "steering retained"})
        await self.client.invoke("follow_up", {"message": "follow retained"})
        cursor = self.client.events()["cursor"]
        await self.client.invoke("abort")
        consumed = [x["message"]["content"] for x in self.client.events(cursor)["events"] if x["type"] == "message_end"]
        self.assertEqual(consumed, ["steering retained", "follow retained"])
        await self.client.invoke("prompt", {"message": "again"})
        await self.client.invoke("steer", {"message": "clear this"})
        await self.client.invoke("follow_up", {"message": "also clear"})
        clear = await self.client.invoke("clear_queue")
        self.assertEqual(clear["data"], {"steering": ["clear this"], "followUp": ["also clear"]})
        cursor = self.client.events()["cursor"]
        await self.client.invoke("abort")
        self.assertFalse(any(x["type"] == "message_end" for x in self.client.events(cursor)["events"]))

    async def test_prompt_while_running_without_mode_is_rejected(self):
        await self.client.invoke("prompt", {"message": "start"})
        self.assertEqual((await self.client.invoke("prompt", {"message": "no mode"}))["outcome"], "rejected")
        queued = await self.client.invoke("prompt", {"message": "queued", "streamingBehavior": "followUp"})
        self.assertEqual(queued["data"]["disposition"], "queued")

    async def test_out_of_order_responses_correlate_by_id(self):
        one = asyncio.create_task(self.client.request("test_hold", {"message": "one"}))
        two = asyncio.create_task(self.client.request("test_hold", {"message": "two"}))
        await self.barrier()
        await self.client.request("test_release")
        r1, r2 = await asyncio.gather(one, two)
        self.assertNotEqual(r1["id"], r2["id"])
        self.assertEqual(r1["data"]["echo"], "one")
        self.assertEqual(r2["data"]["echo"], "two")

    async def test_deadline_late_reply_and_no_resend(self):
        result = await self.client.invoke("prompt", {"message": "hang"}, timeout=.05)
        self.assertEqual(result["outcome"], "indeterminate")
        self.assertEqual(result["reason"], "deadline")
        await self.client.request("test_release")
        late = [x for x in self.client.events()["events"] if x["type"] == "late_response"]
        self.assertEqual(late[0]["response"]["id"], result["id"])
        accepted = await self.client.request("test_accepted")
        self.assertEqual(accepted["data"]["accepted"], ["hang"])

    async def test_crash_unblocks_pending_as_indeterminate(self):
        held = asyncio.create_task(self.client.invoke("prompt", {"message": "hang"}))
        await self.barrier()
        crash = await self.client.invoke("prompt", {"message": "crash"})
        self.assertEqual(crash["outcome"], "indeterminate")
        self.assertEqual((await held)["outcome"], "indeterminate")
        await asyncio.wait_for(self.client.wait_closed(), 2)
        facts = self.client.snapshot()
        self.assertFalse(facts["available"])
        self.assertTrue(facts["stale"])
        self.assertEqual(facts["exit_code"], 23)
        self.assertEqual((await self.client.invoke("abort"))["outcome"], "rejected")

    def kill_lingering(self):
        pidfile = Path(self.root.name) / "linger.pid"
        if pidfile.exists():
            try:
                os.kill(int(pidfile.read_text()), 9)
            except ProcessLookupError:
                pass

    async def lingering_exit(self, **kwargs):
        """Start a client whose fake Pi exits while a descendant keeps its stdout/stderr open."""
        client = await self.start(limits=Limits(drain=.2), **kwargs)
        self.addCleanup(self.kill_lingering)
        held = asyncio.create_task(client.invoke("prompt", {"message": "hang"}))
        await asyncio.sleep(0)
        exit_result = await client.invoke("prompt", {"message": "linger"}, timeout=2)
        return client, held, exit_result

    async def test_owned_exit_is_authoritative_despite_inherited_pipes(self):
        client, held, exit_result = await self.lingering_exit()
        self.assertEqual(exit_result["outcome"], "indeterminate")
        self.assertEqual((await asyncio.wait_for(held, 2))["outcome"], "indeterminate")
        await asyncio.wait_for(client.wait_closed(), 2)
        facts = client.snapshot()
        self.assertFalse(facts["available"])
        self.assertEqual(facts["exit_code"], 0)
        self.assertTrue(any(e["type"] == "pipes_abandoned" for e in client.events()["events"]))

    async def test_close_is_bounded_when_a_descendant_keeps_pipes_open(self):
        client, held, _ = await self.lingering_exit()
        loop = asyncio.get_running_loop()
        started = loop.time()
        await asyncio.wait_for(client.close(grace=.05), 2)
        self.assertLess(loop.time() - started, 1)
        self.assertFalse(client.snapshot()["available"])
        self.assertEqual((await held)["outcome"], "indeterminate")

    async def test_dialog_facts_answer_has_dispatched_not_fake_ack(self):
        await self.client.request("test_dialog", {"dialog_id": "dialog-1", "method": "confirm"})
        facts = self.client.snapshot()
        self.assertEqual(facts["status"], "blocked")
        self.assertEqual(facts["requests"][0]["kind"], "confirm")
        self.assertEqual((await self.client.invoke("answer", {"id": "dialog-1", "value": "bad"}))["outcome"], "rejected")
        reply = await self.client.invoke("answer", {"id": "dialog-1", "confirmed": True})
        self.assertEqual(reply["outcome"], "dispatched")
        await self.barrier()
        observed = [x for x in self.client.events()["events"] if x["type"] == "test_answer_observed"]
        self.assertEqual(observed[0]["answer"], {"type": "extension_ui_response", "id": "dialog-1", "confirmed": True})
        self.assertEqual(self.client.snapshot()["requests"], [])
        self.assertEqual(self.client.snapshot()["status"], "idle")

    async def test_dialog_native_timeout_does_not_leave_stale_blocked_facts(self):
        await self.client.request("test_dialog", {"dialog_id": "expires", "method": "input", "timeout": 20})
        await self.client.wait_for_facts(lambda f: not f["requests"])
        self.assertNotEqual(self.client.snapshot()["status"], "blocked")
        self.assertTrue(any(x["type"] == "dialog_timeout" for x in self.client.events()["events"]))
        self.assertEqual((await self.client.invoke("answer", {"id": "expires", "value": "late"}))["outcome"], "rejected")

    async def test_value_answers_and_host_without_ui_cancel_supported_dialogs(self):
        for kind in ("select", "input", "editor"):
            await self.client.request("test_dialog", {"dialog_id": kind, "method": kind})
            reply = await self.client.invoke("answer", {"id": kind, "value": "one"})
            self.assertEqual(reply["outcome"], "dispatched")
        no_ui = await self.start(supported_dialogs=())
        await no_ui.request("test_dialog", {"dialog_id": "not_rendered", "method": "select"})
        await no_ui.wait_for_facts(lambda f: not f["requests"])
        await no_ui.request("get_state")
        observed = [x for x in no_ui.events()["events"] if x["type"] == "test_answer_observed"]
        self.assertEqual(observed[0]["answer"]["cancelled"], True)

    async def test_unsupported_dialog_is_cancelled_not_left_waiting(self):
        await self.client.request("test_dialog", {"dialog_id": "unsupported", "method": "custom_future_dialog"})
        await self.client.wait_for_facts(lambda f: not f["requests"])
        await self.barrier()
        observed = [x for x in self.client.events()["events"] if x["type"] == "test_answer_observed"]
        self.assertEqual(observed[0]["answer"]["cancelled"], True)
        self.assertEqual(observed[0]["answer"]["id"], "unsupported")

    async def test_lf_only_framing_and_split_utf8(self):
        await self.client.request("test_unicode")
        text = [x["text"] for x in self.client.events()["events"] if x["type"] == "fixture_text"]
        self.assertEqual(text, ["before\u2028middle\u2029after"])

    async def test_event_and_stderr_buffers_are_bounded_and_gap_is_explicit(self):
        client = await self.start(limits=Limits(event_bytes=1024, stderr_bytes=64))
        self.assertEqual((await client.request("test_flood", {"count": 10000}))["outcome"], "accepted")
        # Drain completion is fenced by child EOF, not an arbitrary sleep.
        await client.close()
        diagnostics = client.diagnostics()
        self.assertLessEqual(diagnostics["event_bytes"], 1024)
        self.assertEqual(len(diagnostics["stderr"]), 64)
        self.assertEqual(diagnostics["stderr_total"], 100000)
        self.assertTrue(client.events()["gap"])

    async def test_oversized_record_fails_closed_without_unbounded_read(self):
        result = await self.client.request("test_oversize")
        self.assertEqual(result["outcome"], "indeterminate")
        await asyncio.wait_for(self.client.wait_closed(), 2)
        self.assertIn("protocol error", self.client.diagnostics()["failure"])

    async def test_truncated_record_and_mismatched_response_are_indeterminate(self):
        for command in ["test_truncated", "test_bad_response"]:
            client = await self.start()
            self.assertEqual((await client.request(command))["outcome"], "indeterminate")
            await asyncio.wait_for(client.wait_closed(), 2)
            self.assertFalse(client.snapshot()["available"])

    async def test_pending_limit_and_outbound_size_reject_without_sending(self):
        client = await self.start(limits=Limits(pending=1, record_bytes=1024))
        held = asyncio.create_task(client.request("test_hold"))
        # Yield until the pending slot exists, without assuming child startup time.
        while client.diagnostics()["pending"] == 0:
            await asyncio.sleep(0)
        self.assertEqual((await client.invoke("abort"))["outcome"], "rejected")
        await client.close()
        self.assertEqual((await held)["outcome"], "indeterminate")
        self.assertEqual((await self.client.invoke("prompt", {"message": "x" * 300000}))["outcome"], "rejected")

    async def test_stalled_child_stdin_deadline_closes_write_side_without_buffer_growth(self):
        client = await self.start(fixture_args=["--no-read"])
        result = await client.invoke("prompt", {"message": "x" * 200000}, timeout=.03)
        self.assertEqual(result["outcome"], "indeterminate")
        self.assertFalse(client.snapshot()["available"])
        before = client.diagnostics()["stdin_buffer_bytes"]
        self.assertLessEqual(before, client.limits.record_bytes + 65536)
        for _ in range(50):
            self.assertEqual((await client.invoke("prompt", {"message": "x" * 200000}))["outcome"], "rejected")
        self.assertEqual(client.diagnostics()["stdin_buffer_bytes"], before)
        await client.close(grace=.03)

    async def test_cancelled_request_is_indeterminate_and_not_resent(self):
        pending = asyncio.create_task(self.client.invoke("prompt", {"message": "hang"}))
        await self.barrier()
        pending.cancel()
        self.assertEqual((await pending)["outcome"], "indeterminate")
        await self.client.request("test_release")
        observed = await self.client.request("test_accepted")
        self.assertEqual(observed["data"]["accepted"], ["hang"])

    async def test_unknown_ui_notices_do_not_block_and_malformed_method_fails_closed(self):
        await self.client.request("test_dialog", {"dialog_id": "notice", "method": "notify"})
        self.assertEqual(self.client.snapshot()["requests"], [])
        result = await self.client.request("test_dialog", {"dialog_id": "bad", "method": []})
        self.assertEqual(result["outcome"], "indeterminate")
        await asyncio.wait_for(self.client.wait_closed(), 2)

    async def test_snapshot_is_detached_and_metadata_idle_does_not_invent_settled(self):
        await self.barrier()
        facts = self.client.snapshot()
        self.assertEqual(facts["status"], "idle")
        self.assertIsNone(facts["run"]["settled"])
        facts["queues"]["steering"].append("not real")
        self.assertEqual(self.client.snapshot()["queues"]["steering"], [])


if __name__ == "__main__":
    unittest.main()
