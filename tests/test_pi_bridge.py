"""Standalone Pi runtime regressions. No core, server, provider or credentials."""
import asyncio
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "extensions"))
from pi_bridge import Limits, PiRpcClient

FIXTURE = Path(__file__).parent / "fixtures" / "fake_pi.py"


class PiBridgeTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.root = tempfile.TemporaryDirectory(prefix="vqro-pi-unit-")
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

    async def test_small_event_object_count_is_bounded_and_reports_cursor_gap(self):
        client = PiRpcClient(Limits(event_rows=3), set())
        for i in range(20):
            client._publish({"type": "tiny", "number": i})
        batch = client.events()
        self.assertTrue(batch["gap"])
        self.assertEqual([row["seq"] for row in batch["events"]], [18, 19, 20])
        self.assertLess(client._event_bytes, client.limits.event_bytes)
        self.assertFalse(client.events(17)["gap"])
        self.assertEqual(client.events(20)["events"], [])
        with self.assertRaises(ValueError):
            Limits(event_rows=0)

    async def test_agent_start_ends_preflight_even_when_the_prompt_ack_arrived_first(self):
        # The prompt ACK is read before agent_start, so prompt() leaves the clear to _confirm_start,
        # which runs later. A turn that starts and settles in the same read must not report
        # settled-but-working meanwhile (the retry-status race seen in the C2 gate).
        client = PiRpcClient(Limits(), set())
        client._facts["available"] = True
        client._start_pending = True
        client._status()
        self.assertEqual(client.snapshot()["status"], "working")
        for record in ({"type": "agent_start"}, {"type": "turn_start"},
                       {"type": "auto_retry_start", "attempt": 1, "maxAttempts": 1, "delayMs": 0, "errorMessage": "x"},
                       {"type": "message_end", "message": {"role": "assistant", "content": "ok", "stopReason": "stop"}},
                       {"type": "auto_retry_end", "attempt": 1, "success": True},
                       {"type": "agent_end", "messages": [], "willRetry": False}, {"type": "agent_settled"}):
            client._record(record)
        facts = client.snapshot()
        self.assertEqual(facts["run"], {"active": False, "settled": True, "aborted": None})
        self.assertFalse(facts["start_pending"])
        self.assertEqual(facts["status"], "done")

    async def test_owned_exit_uses_native_notification_not_sleep_polling(self):
        with mock.patch("pi_bridge.client.asyncio.sleep",side_effect=AssertionError("owned exit polled")):
            owned=asyncio.create_task(self.client._owned_exit())
            tick=asyncio.get_running_loop().create_future()
            asyncio.get_running_loop().call_soon(tick.set_result,None)
            await tick
            if owned.done():
                await owned  # surfaces a polling attempt before the child has exited
            self.client.process.stdin.close()
            self.assertEqual(await asyncio.wait_for(owned,1),0)
            await asyncio.wait_for(self.client.wait_closed(),1)

    async def test_journal_change_wait_is_edge_triggered_and_cannot_miss_a_revision(self):
        cursor=self.client.events()["cursor"]
        changed=asyncio.create_task(self.client.wait_changed(cursor))
        await self.barrier()
        self.assertGreater(await asyncio.wait_for(changed,1),cursor)
        self.assertGreater(await asyncio.wait_for(self.client.wait_changed(cursor),1),cursor)

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

    async def test_failed_assistant_turn_stays_error_after_settlement_and_exposes_native_retries(self):
        await self.client.invoke("prompt", {"message": "fixture-model-error"})
        facts = await self.client.wait_for_facts(lambda f: f["run"]["settled"])
        self.assertEqual(facts["status"], "error")
        self.assertIn("ResourceNotFoundException", facts["failure"]["message"])
        self.assertEqual((facts["retry"]["attempt"], facts["retry"]["max_attempts"], facts["retry"]["active"]), (4, 4, False))
        accepted = await self.client.request("test_accepted")
        self.assertEqual(accepted["data"]["accepted"], ["fixture-model-error"], "native retries are observed, never bridge-resubmitted")
        await self.client.invoke("prompt", {"message": "new turn"})
        facts = await self.client.wait_for_facts(lambda f: f["run"]["active"])
        self.assertEqual(facts["status"], "working")
        self.assertIsNone(facts["failure"])
        self.assertIsNone(facts["retry"])

    async def test_reported_active_retry_is_working_until_native_settlement(self):
        await self.client.invoke("prompt", {"message": "start"})
        await self.client.request("test_retry_start")
        facts = self.client.snapshot()
        self.assertEqual(facts["status"], "working")
        self.assertEqual(facts["retry"], {"active": True, "attempt": 2, "max_attempts": 4, "delay_ms": 2000})
        self.assertIn("Fixture overloaded", facts["failure"]["message"])
        await self.client.request("test_settle")
        self.assertFalse(self.client.snapshot()["retry"]["active"])
        self.assertEqual(self.client.snapshot()["status"], "error", "settlement cannot turn an unresolved error into success")

    async def test_successful_native_retry_clears_current_failure_without_inventing_a_failed_turn(self):
        await self.client.invoke("prompt", {"message": "fixture-retry-success"})
        # ACK/settled can be observed while the separate native-start gate is still pending.
        # Wait for this turn's retry evidence and gate release, not an earlier settled snapshot.
        facts = await self.client.wait_for_facts(lambda f: f["run"]["settled"] and not f["start_pending"] and (f.get("retry") or {}).get("success") is True)
        self.assertEqual(facts["status"], "done")
        self.assertIsNone(facts["failure"])
        self.assertEqual(facts["retry"]["attempt"], 4)
        self.assertTrue(facts["retry"]["success"])

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

    async def held_preflight(self):
        """Prompt A sits in Pi's start preflight (P4): ACK pending, Pi reports not streaming."""
        a = asyncio.create_task(self.client.invoke("prompt", {"message": "preflight:A"}))
        for _ in range(200):
            if (await self.client.request("test_accepted"))["data"]["preflight_held"]:
                return a
            await asyncio.sleep(.01)
        self.fail("preflight never held")

    async def test_overlapping_prompts_are_serialized_so_none_is_silently_lost(self):
        a = await self.held_preflight()
        state = await self.client.request("get_state")
        self.assertFalse(state["data"]["isStreaming"], "Pi looks idle during preflight")
        self.assertEqual(self.client.snapshot()["status"], "working", "an unresolved start is not idle")
        self.assertTrue(self.client.snapshot()["start_pending"])
        b = asyncio.create_task(self.client.invoke("prompt", {"message": "B", "streamingBehavior": "followUp"}))
        await asyncio.sleep(.3)
        self.assertFalse(b.done(), "B waits for A to start")
        self.assertEqual((await self.client.request("test_accepted"))["data"]["accepted"], [], "B not sent yet")
        await self.client.request("test_release_preflight")
        self.assertEqual((await asyncio.wait_for(a, 2))["data"]["disposition"], "started")
        self.assertEqual((await asyncio.wait_for(b, 2))["data"]["disposition"], "queued", "sent after A started: queued, in order")
        seen = (await self.client.request("test_accepted"))["data"]
        self.assertEqual((seen["accepted"], seen["lost"]), (["preflight:A", "B"], []), "no prompt silently lost")
        self.assertFalse(self.client.snapshot()["start_pending"])

    async def test_abort_rejects_unsent_prompts_and_reports_an_unresolved_preflight(self):
        a = await self.held_preflight()
        c = asyncio.create_task(self.client.invoke("prompt", {"message": "C"}))
        late = asyncio.create_task(self.client.invoke("prompt", {"message": "D"}, timeout=.3))
        self.assertEqual((await asyncio.wait_for(late, 2))["reason"], "an earlier prompt has not started yet; not sent")
        abort = await self.client.invoke("abort")
        self.assertEqual((abort["outcome"], abort["data"]["start_unresolved"]), ("accepted", True), "abort says A may still start")
        await self.client.request("test_release_preflight")
        self.assertEqual((await asyncio.wait_for(a, 2))["data"]["disposition"], "started", "Pi0.87: abort does not cancel preflight")
        rejected = await asyncio.wait_for(c, 2)
        self.assertEqual((rejected["outcome"], rejected["reason"]), ("rejected", "aborted before dispatch; not sent"))
        seen = (await self.client.request("test_accepted"))["data"]
        self.assertEqual((seen["accepted"], seen["lost"]), (["preflight:A"], []), "C and D were never sent")

    async def test_crash_unblocks_pending_as_indeterminate(self):
        held = asyncio.create_task(self.client.invoke("prompt", {"message": "hang"}))
        await self.barrier()
        # Fixture trigger, sent raw: invoke("prompt") would (correctly) wait behind the held start.
        crash = await self.client.request("prompt", {"message": "crash"})
        self.assertEqual(crash["outcome"], "indeterminate")
        self.assertEqual((await held)["outcome"], "indeterminate")
        await asyncio.wait_for(self.client.wait_closed(), 2)
        facts = self.client.snapshot()
        self.assertFalse(facts["available"])
        self.assertTrue(facts["stale"])
        self.assertEqual(facts["exit_code"], 23)
        self.assertEqual((await self.client.invoke("abort"))["outcome"], "rejected")

    @staticmethod
    def kill_pid(pid):
        try:
            os.kill(pid, 9)
        except ProcessLookupError:
            pass

    async def lingering_exit(self, **kwargs):
        """Start a client whose fake Pi exits while a descendant keeps its stdout/stderr open."""
        client = await self.start(limits=Limits(drain=.2), **kwargs)
        held = asyncio.create_task(client.invoke("prompt", {"message": "hang"}))
        await asyncio.sleep(0)
        exit_result = await client.request("prompt", {"message": "linger"}, timeout=2)  # raw: past the start gate
        # The fake Pi wrote the descendant's pid before exiting. Read it now: asyncTearDown removes the
        # root before cleanups run, so a cleanup that reads the file later finds nothing (and leaks it).
        pidfile = Path(self.root.name) / "linger.pid"
        for _ in range(100):
            if pidfile.exists() and pidfile.read_text().strip():
                break
            await asyncio.sleep(0.01)
        self.addCleanup(self.kill_pid, int(pidfile.read_text()))
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
        client = await self.start(limits=Limits(response_bytes=65536))  # the fixture writes 1 MiB, no newline
        result = await client.request("test_oversize")
        self.assertEqual(result["outcome"], "indeterminate")
        await asyncio.wait_for(client.wait_closed(), 2)
        self.assertIn("protocol error", client.diagnostics()["failure"])

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
