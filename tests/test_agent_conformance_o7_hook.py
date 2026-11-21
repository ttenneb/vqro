"""O7's optional observation boundary must not move or duplicate its one recovery."""
import asyncio
import unittest
from types import SimpleNamespace
from unittest.mock import AsyncMock, patch

from agent_conformance import cases
from agent_conformance.results import CheckFailed, HarnessGap


class BeforeRecoveryHookTests(unittest.IsolatedAsyncioTestCase):
    def fixture(self, *, reason='slow_consumer', retirement_error=None):
        order = []
        views = []
        reading = [True]

        def pause():
            reading[0] = False

        def resume():
            reading[0] = True
            views[2].closed = reason
            views[2].reducer.failure = reason

        transport = SimpleNamespace(pause_reading=pause, resume_reading=resume,
                                    is_reading=lambda: reading[0])

        async def rpc(method):
            self.assertEqual(method, 'workspace.snapshot')
            order.append('rpc')
            return {'epoch': 'epoch'}

        async def retirement(predicate, *, timeout):
            order.append('retirement')
            self.assertEqual(timeout, 5)
            if retirement_error is not None:
                raise retirement_error
            self.assertTrue(predicate())

        for _ in range(4):
            reducer = SimpleNamespace(cursor={'epoch': 'epoch', 'seq': 1}, epoch='epoch',
                                      sequence=1, offset=0, entities={'same': {}}, failure=None)
            peer = SimpleNamespace(failure=None, writer=SimpleNamespace(transport=transport),
                                   call=AsyncMock(side_effect=rpc), wait=AsyncMock(side_effect=retirement))
            views.append(SimpleNamespace(reducer=reducer, peer=peer, closed=None,
                                         close_notice={'reason': reason}, initial='continue'))

        async def attach(ctx, **kwargs):
            index = len(attached)
            if index == 3:
                order.append('reattach')
                self.assertIs(kwargs['peer'], views[2].peer)
                self.assertEqual(kwargs['after'], views[2].reducer.cursor)
                self.assertEqual(kwargs['seed'].entities, views[2].reducer.entities)
            attached.append(views[index])
            return views[index]

        ctx = SimpleNamespace(viewer=AsyncMock(), wait_view=AsyncMock(), evidence=[])
        attached = []
        return SimpleNamespace(ctx=ctx, views=views, order=order, attached=attached,
                               attach=attach, transport=transport)

    async def execute(self, fixture, **kwargs):
        with patch.object(cases, 'fresh_view', side_effect=fixture.attach), \
                patch.object(cases, 'flood_retained', new_callable=AsyncMock, return_value='needle') as flood:
            await cases.stalled_viewer_isolation(fixture.ctx, **kwargs)
            self.assertEqual(flood.await_count, 52)
            for call in flood.await_args_list:
                self.assertEqual(call.kwargs, {'count': 8})

    async def test_default_omission_and_explicit_none_keep_one_recovery(self):
        for kwargs in ({}, {'before_recovery': None}):
            with self.subTest(kwargs=kwargs):
                fixture = self.fixture()
                await self.execute(fixture, **kwargs)
                self.assertEqual(fixture.order, ['retirement', 'rpc', 'reattach'])
                self.assertEqual(len(fixture.attached), 4)
                fixture.views[2].peer.call.assert_awaited_once_with('workspace.snapshot')
                self.assertEqual(fixture.ctx.evidence[-1]['native_records_requested'], 416)
                self.assertTrue(fixture.ctx.evidence[-1]['exact_fast_viewer_reconstruction'])

    async def test_hook_is_awaited_once_after_isolation_before_rpc_and_reattach(self):
        fixture = self.fixture()

        async def observe():
            self.assertEqual(fixture.order, ['retirement'])
            self.assertEqual(fixture.views[2].closed, 'slow_consumer')
            self.assertTrue(fixture.transport.is_reading())
            cases.stalled_attachment_witness(*fixture.views[:3])
            fixture.order.append('hook_enter')
            await asyncio.sleep(0)
            fixture.order.append('hook_done')

        hook = AsyncMock(side_effect=observe)
        await self.execute(fixture, before_recovery=hook)
        hook.assert_awaited_once_with()
        self.assertEqual(fixture.order, ['retirement', 'hook_enter', 'hook_done', 'rpc', 'reattach'])
        self.assertEqual(len(fixture.attached), 4)

    async def test_hook_failure_or_cancellation_does_not_begin_recovery(self):
        for error in (HarnessGap('missing retention witness'), asyncio.CancelledError()):
            with self.subTest(error=type(error).__name__):
                fixture = self.fixture()
                hook = AsyncMock(side_effect=error)
                with self.assertRaises(type(error)):
                    await self.execute(fixture, before_recovery=hook)
                hook.assert_awaited_once_with()
                self.assertEqual(fixture.order, ['retirement'])
                fixture.views[2].peer.call.assert_not_awaited()
                self.assertEqual(len(fixture.attached), 3)
                self.assertTrue(fixture.transport.is_reading())

    async def test_invalid_isolation_witness_never_calls_hook(self):
        fixture = self.fixture(reason='retention')
        hook = AsyncMock()
        with self.assertRaises(CheckFailed):
            await self.execute(fixture, before_recovery=hook)
        hook.assert_not_awaited()
        fixture.views[2].peer.call.assert_not_awaited()
        self.assertEqual(len(fixture.attached), 3)

    async def test_missing_retirement_never_calls_hook(self):
        fixture = self.fixture(retirement_error=asyncio.TimeoutError())
        hook = AsyncMock()
        with self.assertRaises(HarnessGap):
            await self.execute(fixture, before_recovery=hook)
        hook.assert_not_awaited()
        fixture.views[2].peer.call.assert_not_awaited()
        self.assertEqual(len(fixture.attached), 3)


if __name__ == '__main__':
    unittest.main()
