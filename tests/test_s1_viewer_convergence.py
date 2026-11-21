# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
import asyncio
from copy import deepcopy
import json
from types import SimpleNamespace
import unittest
from unittest.mock import AsyncMock

from agent_conformance.part_b.viewer_convergence import converge_s1_viewers, same_state, stalled_closure
from agent_conformance.results import CheckFailed
from agent_conformance.wire import ItemReducer


def view(peer=None):
    reducer = ItemReducer()
    reducer.accept({'op': 'snapshot', 'phase': 'begin', 'epoch': 'e1', 'coverage': 'tail'})
    reducer.accept({'op': 'put', 'kind': 'message', 'id': 'm1', 'rev': 1, 'value': {'text': 'final'}})
    reducer.accept({'op': 'snapshot', 'phase': 'end', 'seq': 1})
    return SimpleNamespace(peer=peer or SimpleNamespace(failure=None), stream=1, reducer=reducer,
                           closed=None, close_notice=None, initial='continue')


def slow(value):
    value.closed = 'slow_consumer'
    value.close_notice = {'method': 'stream.closed', 'reason': {'reason': 'slow_consumer'}}
    value.reducer.ended('slow_consumer')
    return value


def reader(reason='slow_consumer', stream=19, method='stream.closed'):
    value = asyncio.StreamReader(limit=128 << 10)
    frames = [{'id': 2, 'result': {'stream': stream}},
              {'method': method, 'params': {'stream': stream, 'reason': {'reason': reason}}}]
    value.feed_data(b''.join(json.dumps(row).encode() + b'\n' for row in frames))
    value.feed_eof()
    return value


class Context:
    def __init__(self):
        self.evidence = []
        self.attach = AsyncMock(side_effect=lambda **kw: view(kw['peer']))
    def check(self, value, detail, **extra):
        if not value: raise CheckFailed(detail)
    async def wait(self, predicate, *, label):
        # Logical scheduling, not a wall-clock timeout that flakes under load.
        for _ in range(5):
            if await predicate(): return
            await asyncio.sleep(0)
        raise CheckFailed(label)


class ViewerConvergenceTests(unittest.IsolatedAsyncioTestCase):
    def setup_case(self):
        peers = [SimpleNamespace(failure=None) for _ in range(3)]
        slots = [('r' + str(agent), view(peer)) for peer in peers for agent in range(200)]
        return Context(), slots, [(reader(), None), (reader(), None)]

    async def test_600_healthy_converge_without_new_population(self):
        ctx, slots, stalled = self.setup_case()
        fresh = await converge_s1_viewers(ctx, slots, stalled)
        self.assertEqual(len(fresh), 600)
        ctx.attach.assert_not_awaited()
        self.assertEqual(len(ctx.evidence), 2)

    async def test_slow_consumer_reattaches_actual_cursor_seed_and_same_peer_once(self):
        ctx, slots, stalled = self.setup_case()
        old = slow(slots[0][1])
        cursor = deepcopy(old.reducer.cursor)
        fresh = await converge_s1_viewers(ctx, slots, stalled)
        ctx.attach.assert_awaited_once()
        args = ctx.attach.call_args.kwargs
        self.assertEqual(args['after'], cursor)
        self.assertIs(args['peer'], old.peer)
        self.assertEqual(args['seed'].entities, old.reducer.entities)
        self.assertIsNot(args['seed'], old.reducer)
        self.assertEqual(old.reducer.failure, 'slow_consumer')
        self.assertIsNot(fresh[0], old)
        self.assertEqual(ctx.evidence[0]['kind'], 's1_healthy_reattach')

    async def test_uninstalled_snapshot_uses_no_invented_cursor_or_seed(self):
        ctx, slots, stalled = self.setup_case()
        old = slots[0][1]
        old.reducer = ItemReducer()
        slow(old)
        await converge_s1_viewers(ctx, slots, stalled)
        self.assertIsNone(ctx.attach.call_args.kwargs['after'])
        self.assertIsNone(ctx.attach.call_args.kwargs['seed'])

    async def test_unknown_closure_or_prior_integrity_failure_never_reattaches(self):
        for failure in ('retention', 'append base is a gap', 'bad_record'):
            with self.subTest(failure=failure):
                ctx, slots, stalled = self.setup_case()
                old = slow(slots[0][1]); old.reducer.failure = failure
                with self.assertRaises(CheckFailed): await converge_s1_viewers(ctx, slots, stalled)
                ctx.attach.assert_not_awaited()

    async def test_gap_notice_cannot_masquerade_as_budget_close(self):
        ctx, slots, stalled = self.setup_case()
        old = slow(slots[0][1]); old.close_notice['method'] = 'stream.gap'
        with self.assertRaises(CheckFailed): await converge_s1_viewers(ctx, slots, stalled)
        ctx.attach.assert_not_awaited()

    async def test_second_slow_close_fails_instead_of_tight_retry(self):
        ctx, slots, stalled = self.setup_case(); slow(slots[0][1])
        ctx.attach.side_effect = lambda **kw: slow(view(kw['peer']))
        with self.assertRaises(CheckFailed): await converge_s1_viewers(ctx, slots, stalled)
        ctx.attach.assert_awaited_once()

    async def test_healthy_connection_failure_cannot_be_hidden(self):
        ctx, slots, stalled = self.setup_case(); slots[0][1].peer.failure = 'socket lost'
        with self.assertRaises(CheckFailed): await converge_s1_viewers(ctx, slots, stalled)
        ctx.attach.assert_not_awaited()

    async def test_different_final_state_cannot_converge(self):
        ctx, slots, stalled = self.setup_case()
        slots[0][1].reducer.entities[('message', 'm1')]['value']['text'] = 'wrong'
        with self.assertRaises(CheckFailed): await converge_s1_viewers(ctx, slots, stalled)
        ctx.attach.assert_not_awaited()

    async def test_incomplete_snapshot_cannot_converge(self):
        ctx, slots, stalled = self.setup_case()
        slots[0][1].reducer.staged = {}
        with self.assertRaises(CheckFailed): await converge_s1_viewers(ctx, slots, stalled)

    async def test_population_and_missing_stalled_viewer_are_not_waived(self):
        for population, stalled_count in ((599, 2), (600, 0), (600, 1)):
            ctx, slots, stalled = self.setup_case()
            with self.assertRaises(CheckFailed): await converge_s1_viewers(ctx, slots[:population], stalled[:stalled_count])

    async def test_unclosed_stalled_viewer_fails_even_when_healthy_views_match(self):
        ctx, slots, stalled = self.setup_case()
        empty = asyncio.StreamReader(); empty.feed_eof(); stalled[0] = (empty, None)
        with self.assertRaises(ValueError): await converge_s1_viewers(ctx, slots, stalled)

    async def test_stalled_close_requires_actual_attach_and_strict_reason(self):
        for reason, stream, method in (('unknown', 19, 'stream.closed'), ('slow_consumer', True, 'stream.closed'),
                                       ('slow_consumer', 19.0, 'stream.closed'), ('slow_consumer', 19, 'stream.gap')):
            with self.subTest(reason=reason, stream=stream, method=method):
                with self.assertRaises(ValueError): await stalled_closure(reader(reason, stream, method))

    def test_state_equality_includes_cursor_session_coverage(self):
        for attribute, value in (('sequence', 0), ('session', 'other'), ('coverage', 'state_only')):
            views = [view(), view(), view()]
            setattr(views[0].reducer, attribute, value)
            self.assertFalse(same_state(views))
