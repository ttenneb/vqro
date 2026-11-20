# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Fact-wait and window-probe regressions: no core, runtime, model or regrade."""
import asyncio
import inspect
from types import SimpleNamespace
import unittest
from unittest.mock import AsyncMock, patch

from agent_conformance.part_b.context import PublicCoordination
from agent_conformance.part_b.cases import settled
from agent_conformance.part_b.inbox_cases import age_out_history
from agent_conformance.part_b.scale_cases import observed_setup_grant, roll_operation_window
from agent_conformance.results import CheckFailed, HarnessGap, check
from agent_conformance.wire import RpcError


def refusal(kind):
    return RpcError({'message': kind, 'data': {'kind': kind}})


class ObservationContext:
    def __init__(self):
        self.evidence = []
        self.wait_bounds = []

    async def wait(self, predicate, *, label, timeout):
        self.wait_bounds.append(timeout)
        # A logical clock permits one polling tick, then expires. The old 50ms
        # wall-clock clamp could expire before the second read under parallel load.
        ticks = 0

        async def tick(_delay):
            nonlocal ticks
            ticks += 1
            if ticks > 1:
                raise asyncio.TimeoutError

        async def deadline(awaitable, actual_timeout):
            assert actual_timeout == timeout  # Exercise the production bound forwarding.
            return await awaitable

        with patch('agent_conformance.part_b.context.asyncio.sleep', side_effect=tick), \
             patch('agent_conformance.part_b.context.asyncio.wait_for', side_effect=deadline):
            return await PublicCoordination.wait(self, predicate, label=label, timeout=timeout)


class WindowContext:
    def __init__(self, replies, epochs):
        self.replies, self.epochs = iter(replies), iter(epochs)
        self.evidence, self.sends = [], []

    async def command(self, name):
        assert name == 'inbox.window'
        return {'epoch': next(self.epochs)}

    async def send(self, **args):
        self.sends.append(args)
        reply = next(self.replies)
        if isinstance(reply, Exception):
            raise reply
        return reply

    def check(self, condition, message, **evidence):
        check(condition, message, component='coordination')


class ObservedWaits(unittest.IsolatedAsyncioTestCase):
    async def test_scope_inspect_waits_for_actual_cache_visibility(self):
        ctx = ObservationContext()
        ctx.command = AsyncMock(side_effect=[{'targets': []}, {'targets': ['r2']}])
        row = await PublicCoordination.wait_command(ctx, 'scope', lambda row: 'r2' in row['targets'],
            args={'action': 'inspect', 'scope': 'team:r1'})
        self.assertEqual(row['targets'], ['r2'])
        self.assertEqual(ctx.command.await_count, 2)
        self.assertEqual(ctx.wait_bounds, [8])
        self.assertTrue(all(call.kwargs == {'timeout': 8} for call in ctx.command.await_args_list))

    async def test_scope_visibility_that_never_arrives_still_fails(self):
        ctx = ObservationContext()
        ctx.command = AsyncMock(return_value={'targets': []})
        with self.assertRaises(CheckFailed):
            await PublicCoordination.wait_command(ctx, 'scope', lambda row: 'r2' in row['targets'])
        self.assertEqual(ctx.evidence[-1]['kind'], 'unmet_public_observation')

    async def test_history_wait_changes_only_explicit_observation_budget(self):
        ctx = SimpleNamespace(wait_command=AsyncMock(return_value={'state': 'accepted'}))
        await settled(ctx, 'd1', timeout=60)
        call = ctx.wait_command.await_args
        self.assertEqual(call.kwargs, {'args': {'delivery': 'd1'}, 'timeout': 60})
        predicate = call.args[1]
        for state in ('held', 'recorded', 'claimed'):
            self.assertFalse(predicate({'state': state}))
        self.assertTrue(predicate({'state': 'accepted'}))
        await settled(ctx, 'd2')
        self.assertEqual(ctx.wait_command.await_args.kwargs['timeout'], 8)
        self.assertEqual(inspect.signature(age_out_history).parameters['settle_timeout'].default, 8)

    async def test_command_grant_defaults_and_explicit_setup_bound_do_not_replay(self):
        ctx = ObservationContext()
        ctx.serial = 0
        ctx.peer = SimpleNamespace(call=AsyncMock(side_effect=[{'epoch': 'e1'}, {'state': 'accepted'}]))
        ctx.command = lambda *args, **kwargs: PublicCoordination.command(ctx, *args, **kwargs)
        ctx.operation = lambda *args, **kwargs: PublicCoordination.operation(ctx, *args, **kwargs)
        await PublicCoordination.grant(ctx, 'grant', 'worker', ['send:admit'], timeout=60)
        self.assertEqual(ctx.peer.call.await_count, 2)  # one read and exactly one mutation
        self.assertTrue(all(call.kwargs['timeout'] == 60 for call in ctx.peer.call.await_args_list))
        self.assertEqual(ctx.peer.call.await_args.args[1]['args']['expect'], 0)
        self.assertEqual(inspect.signature(PublicCoordination.command).parameters['timeout'].default, 8)
        self.assertEqual(inspect.signature(PublicCoordination.grant).parameters['timeout'].default, 8)

    async def test_setup_grant_is_issued_once_then_observed_as_public_config(self):
        ctx = ObservationContext()
        ctx.grant = AsyncMock(return_value={'state': 'accepted'})
        value = {'who': 'worker', 'can': ['send:admit'], 'on': '*'}
        ctx.peer = SimpleNamespace(call=AsyncMock(side_effect=[{'value': None}, {'value': value, 'version': 7}]))
        row = await observed_setup_grant(ctx, 'grant', 'worker', ['send:admit'])
        self.assertEqual(row['version'], 7)
        ctx.grant.assert_awaited_once_with('grant', 'worker', ['send:admit'], '*', timeout=60)
        self.assertEqual(ctx.wait_bounds, [60])
        self.assertTrue(all(call.args[0] == 'workspace.get' and call.kwargs['timeout'] == 60
                            for call in ctx.peer.call.await_args_list))
        self.assertEqual(ctx.evidence[-1]['kind'], 'observed_setup_grant')

    async def test_setup_never_observed_or_unknown_write_is_not_reissued(self):
        ctx = ObservationContext()
        ctx.grant = AsyncMock(return_value={'state': 'accepted'})
        ctx.peer = SimpleNamespace(call=AsyncMock(return_value={'value': {'who': 'other'}}))
        with self.assertRaises(CheckFailed):
            await observed_setup_grant(ctx, 'grant', 'worker', ['send:admit'])
        self.assertEqual(ctx.grant.await_count, 1)
        ctx.grant = AsyncMock(side_effect=asyncio.TimeoutError)
        with self.assertRaises(asyncio.TimeoutError):
            await observed_setup_grant(ctx, 'grant', 'worker', ['send:admit'])
        self.assertEqual(ctx.grant.await_count, 1)

    async def test_window_roll_records_admission_and_expected_refusals(self):
        ctx = WindowContext([{'delivery': 'd1'}, refusal('pending_exists'), refusal('expired_operation')],
                            ['e1', 'e1', 'e1', 'e2', 'e2'])
        await roll_operation_window(ctx, object(), 'r1', attempts=3, check_every=1)
        evidence = ctx.evidence[-1]
        self.assertEqual(evidence['admitted_deliveries'], ['d1'])
        self.assertEqual(evidence['refusals'], {'pending_exists': 1, 'expired_operation': 1})
        self.assertEqual((evidence['before'], evidence['after']), ('e1', 'e2'))
        self.assertTrue(all('send_new' not in args for args in ctx.sends))
        signature = inspect.signature(roll_operation_window)
        self.assertEqual(signature.parameters['attempts'].default, 2099)
        self.assertEqual(signature.parameters['check_every'].default, 64)

    async def test_refusals_are_not_epoch_change_proof(self):
        ctx = WindowContext([refusal('expired_operation'), refusal('pending_exists')], ['e1'] * 4)
        with self.assertRaises(CheckFailed):
            await roll_operation_window(ctx, object(), 'r1', attempts=2, check_every=1)
        self.assertEqual(ctx.evidence[-1]['after'], 'e1')

    async def test_blank_admitted_identity_fails_even_when_the_epoch_would_change(self):
        ctx = WindowContext([{'delivery': ''}], ['e1', 'e2', 'e2'])
        with self.assertRaises(HarnessGap):
            await roll_operation_window(ctx, object(), 'r1', attempts=1, check_every=1)
        self.assertEqual(ctx.evidence, [])

    async def test_unrelated_refusals_and_unidentified_admissions_still_fail(self):
        ctx = WindowContext([refusal('no_grant')], ['e1'])
        with self.assertRaises(RpcError):
            await roll_operation_window(ctx, object(), 'r1', attempts=1, check_every=1)
        ctx = WindowContext([{'state': 'accepted'}], ['e1'])
        with self.assertRaises(HarnessGap):
            await roll_operation_window(ctx, object(), 'r1', attempts=1, check_every=1)


if __name__ == '__main__':
    unittest.main()
