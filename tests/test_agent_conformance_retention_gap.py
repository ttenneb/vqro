# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Strict source controls, not public/native execution qualification."""
import asyncio
from copy import deepcopy
from types import SimpleNamespace as NS
import unittest
from unittest.mock import patch

import agent_conformance.cases  # Shared helpers/registry bootstrap before helper import.
from agent_conformance import retention_gap_cases as probe
from agent_conformance.results import CheckFailed, HarnessGap


def reducer(text='before', snapshots=1, epoch='items-e1'):
    return NS(entities={('message', 'm1'): {'value': {'final': True, 'content': [{'type': 'text', 'text': text}]}}},
              snapshots=snapshots, staged=None, pending=b'', epoch=epoch,
              cursor={'epoch': epoch, 'seq': snapshots}, failure=None, offset=100)


async def readonly_snapshot(method):
    if method != 'workspace.snapshot': raise AssertionError('not the bounded read-only probe')
    return {'epoch': 'workspace-e1', 'seq': 7}


def gap_view():
    value = reducer()
    value.failure = 'retention'
    return NS(closed='retention', close_notice={'method': 'stream.gap',
              'reason': {'reason': 'gap', 'gap': 'retention'}, 'last_good_cursor': deepcopy(value.cursor)},
              reducer=value, peer=NS(failure=None, call=readonly_snapshot))


class GapControls(unittest.TestCase):
    def test_actual_gap_cursor_and_notice(self):
        view = gap_view()
        cursor, notice = probe.actual_gap(view)
        self.assertEqual(cursor, view.reducer.cursor)
        self.assertIsNot(cursor, view.reducer.cursor)
        self.assertEqual(notice['method'], 'stream.gap')

    def test_reset_only_or_slow_consumer_is_not_retention(self):
        for reason in (None, 'slow_consumer', 'unknown', 'eof'):
            with self.subTest(reason=reason), self.assertRaises(HarnessGap):
                view = gap_view(); view.closed = reason
                probe.actual_gap(view)

    def test_label_without_typed_gap_cannot_pass(self):
        view = gap_view(); view.close_notice['method'] = 'stream.closed'
        with self.assertRaises(CheckFailed): probe.actual_gap(view)

    def test_wrong_raw_gap_reason_does_not_pass_internal_label(self):
        for reason in (None, 'slow_consumer', {'reason': 'gap', 'gap': 'unknown'}):
            view = gap_view(); view.close_notice['reason'] = reason
            with self.subTest(reason=reason), self.assertRaises(CheckFailed): probe.actual_gap(view)

    def test_gap_cannot_mask_corruption(self):
        view = gap_view(); view.reducer.failure = 'bad_append_base'
        with self.assertRaises(CheckFailed): probe.actual_gap(view)

    def test_rpc_failure_not_a_valid_stream_only_gap(self):
        view = gap_view(); view.peer.failure = 'socket broke'
        with self.assertRaises(CheckFailed): probe.actual_gap(view)

    def test_fabricated_or_wrong_cursor_cannot_pass(self):
        for cursor in (None, {'epoch': 'items-e1', 'seq': True}, {'epoch': '', 'seq': 1}, {'epoch': 'items-e1', 'seq': -1}):
            with self.subTest(cursor=cursor), self.assertRaises(CheckFailed):
                view = gap_view(); view.reducer.cursor = cursor
                probe.actual_gap(view)
        view = gap_view(); view.close_notice['last_good_cursor']['seq'] += 1
        with self.assertRaises(CheckFailed): probe.actual_gap(view)

    def test_entity_count_and_bytes_remain_enforced(self):
        value = reducer('☃' * 20)
        for declaration in ({'entities': 0, 'entity_bytes': 1000}, {'entities': 512, 'entity_bytes': 1}):
            with self.subTest(declaration=declaration), self.assertRaises(CheckFailed):
                probe.bounded_state(value, declaration)


class ReplayContext:
    def __init__(self, *, initial='reset', snapshots=2, epoch='items-e1', failure=None):
        self.evidence = []; self.calls = []
        self.replay = NS(initial=initial, reducer=reducer('511:O10-gap-burst:', snapshots, epoch),
                         epoch='items-e1', closed=None, close_notice=None, peer=NS(failure=None))
        self.replay.reducer.failure = failure

    async def viewer(self): return self.replay.peer

    async def attach(self, **kwargs):
        self.calls.append(kwargs)
        return self.replay

    async def wait_view(self, view, predicate, **kwargs):
        if view.reducer.failure or not predicate(view.reducer):
            raise CheckFailed('modeled public recovery did not satisfy predicate')
        return view


class ReplayControls(unittest.IsolatedAsyncioTestCase):
    bounds = {'entities': 512, 'entity_bytes': 524288}

    async def test_one_replay_from_real_last_applied_cursor(self):
        ctx = ReplayContext(); old = gap_view()
        await probe.one_replay(ctx, old, self.bounds, '511:O10-gap-burst:')
        self.assertEqual(len(ctx.calls), 1)
        self.assertEqual(ctx.calls[0]['after'], old.reducer.cursor)
        self.assertEqual(ctx.evidence[-1]['attempts'], 1)

    async def test_unmarked_dead_old_peer_cannot_hide_behind_new_peer(self):
        ctx = ReplayContext(); old = gap_view()
        async def dead(method): raise ConnectionError('old socket stopped serving RPC')
        old.peer.call = dead
        with self.assertRaises(CheckFailed):
            await probe.one_replay(ctx, old, self.bounds, '511:O10-gap-burst:')
        self.assertIsNone(old.peer.failure)
        self.assertEqual(ctx.calls, [])

    async def test_unmarked_hanging_old_peer_is_bounded_and_never_replays(self):
        ctx = ReplayContext(); old = gap_view()
        async def hangs(method): await asyncio.Future()
        old.peer.call = hangs
        with self.assertRaises(CheckFailed):
            await probe.one_replay(ctx, old, self.bounds, '511:O10-gap-burst:')
        self.assertIsNone(old.peer.failure)
        self.assertEqual(ctx.calls, [])

    async def test_continue_or_unknown_cannot_substitute_for_reset(self):
        for initial in ('continue', 'surprise'):
            ctx = ReplayContext(initial=initial)
            with self.subTest(initial=initial), self.assertRaises(CheckFailed):
                await probe.one_replay(ctx, gap_view(), self.bounds, '511:O10-gap-burst:')
            self.assertEqual(len(ctx.calls), 1)

    async def test_stale_seed_counter_even_with_correct_content_fails(self):
        ctx = ReplayContext(snapshots=1)
        with self.assertRaises(HarnessGap):
            await probe.one_replay(ctx, gap_view(), self.bounds, '511:O10-gap-burst:')
        self.assertEqual(len(ctx.calls), 1)

    async def test_completed_wrong_epoch_fails(self):
        ctx = ReplayContext(epoch='stale-e0')
        with self.assertRaises(CheckFailed):
            await probe.one_replay(ctx, gap_view(), self.bounds, '511:O10-gap-burst:')
        self.assertEqual(len(ctx.calls), 1)

    async def test_new_snapshot_counter_does_not_allow_rewound_cut(self):
        ctx = ReplayContext(); ctx.replay.reducer.cursor['seq'] = 1
        with self.assertRaises(CheckFailed):
            await probe.one_replay(ctx, gap_view(), self.bounds, '511:O10-gap-burst:')
        self.assertEqual(len(ctx.calls), 1)

    async def test_second_gap_cannot_retry(self):
        ctx = ReplayContext(failure='retention')
        ctx.replay.closed = 'retention'
        with self.assertRaises(HarnessGap):
            await probe.one_replay(ctx, gap_view(), self.bounds, '511:O10-gap-burst:')
        self.assertEqual(len(ctx.calls), 1)
        self.assertEqual(ctx.evidence[-1]['attempts'], 1)

    async def test_missing_marker_or_incomplete_snapshot_fails(self):
        for change in ('marker', 'staged', 'pending'):
            ctx = ReplayContext()
            if change == 'marker': ctx.replay.reducer.entities.clear()
            elif change == 'staged': ctx.replay.reducer.staged = {'snapshot': 'unfinished'}
            else: ctx.replay.reducer.pending = b'partial'
            with self.subTest(change=change), self.assertRaises(HarnessGap):
                await probe.one_replay(ctx, gap_view(), self.bounds, '511:O10-gap-burst:')
            self.assertEqual(len(ctx.calls), 1)


class PressureContext:
    """Author-only modeled context; never an implementation descriptor/receipt."""
    def __init__(self, gap=True, settle=True):
        self.evidence = []; self.metadata = {'adapter_image': {'pid': 123}}; self.manifest = {'wire': 'pi'}
        self.native = self; self.run = None; self.completion = None; self.number = 0
        self.phase = ''; self.text = 'before'; self.after = []; self.attaches = []
        self.gap = gap; self.settle = settle; self.target = None
        self.native_calls = []; self.paused = False; self.views = []; self.omit_run_fact = False
        self.omit_state_run = False; self.B_calls = 0; self.max_active = 0; self.viewer_limit = 4
        self.provider = {'limits': {'max_item_streams': 4}}

    async def facts(self):
        value = {'run': self.run, 'completion': self.completion, 'state': 'idle' if not self.run else 'running',
                 'epoch': 'agent-e1', 'caps': {'items.stream': {'max_viewers': self.viewer_limit}},
                 'items': {'entities': 512, 'entity_bytes': 524288, 'retain_bytes': 131072}}
        if self.omit_run_fact: value.pop('run')
        return value

    async def wait_facts(self, predicate, **kwargs):
        facts = await self.facts()
        if not predicate(facts): raise CheckFailed('public native boundary absent')
        return facts

    async def call(self, method, **kwargs):
        self.native_calls.append((method, deepcopy(kwargs)))
        if method == 'turn':
            if kwargs['state'] == 'start':
                self.number += 1; self.run = 'observed-run-' + str(self.number); self.target = None
            elif self.settle:
                self.completion = {'id': self.run}; self.run = None
        elif kwargs.get('count') in (256, 512):
            live = [view for view in self.views if not view.peer.closed and view.closed is None]
            if self.target is None: self.target = live[0]
            self.text = str(kwargs.get('start_index', 0) + kwargs['count'] - 1) + ':' + ''.join(kwargs['chunks'])
            for view in live: view.reducer.entities.update(reducer(self.text).entities)
            if self.gap:
                self.target.closed = 'retention'; self.target.reducer.failure = 'retention'
                self.target.close_notice = {'method': 'stream.gap', 'reason': {'reason': 'gap', 'gap': 'retention'},
                                           'last_good_cursor': deepcopy(self.target.reducer.cursor)}
            else:
                self.target.reducer.entities = reducer(self.text).entities
        else:
            self.text = kwargs['text']
        return {}

    def pause_reading(self): self.paused = True
    def resume_reading(self): self.paused = False
    def is_reading(self): return not self.paused

    async def close(self): pass
    async def wait(self, predicate, **kwargs):
        if not predicate(): raise asyncio.TimeoutError()

    async def viewer(self):
        peer = NS(failure=None, closed=False, wait=self.wait, call=readonly_snapshot)
        async def close(): peer.closed = True
        peer.close = close
        def pause():
            self.pause_reading()
            self.target = next(view for view in self.views if view.peer is peer)
        peer.writer = NS(transport=NS(pause_reading=pause, resume_reading=self.resume_reading, is_reading=self.is_reading))
        return peer

    async def attach(self, **kwargs):
        self.attaches.append(kwargs)
        value = reducer(self.text)
        state = {'state': 'running' if self.run else 'idle', 'run': self.run, 'completion': self.completion}
        if self.omit_state_run: state.pop('run')
        value.entities[('state', 'actual-native-current')] = {'value': state}
        if kwargs.get('after') is not None:
            self.after.append(deepcopy(kwargs['after']))
            self.assert_quiescent = self.run is None
            value.snapshots = kwargs['seed'].snapshots + 1
            value.cursor['seq'] = kwargs['after']['seq'] + 1
        view = NS(initial='reset', reducer=value, epoch='items-e1', peer=kwargs['peer'], closed=None, close_notice=None)
        self.views.append(view)
        self.max_active = max(self.max_active, len([v for v in self.views if not v.peer.closed and v.closed is None]))
        return view

    async def wait_view(self, view, predicate, **kwargs):
        if not predicate(view.reducer): raise CheckFailed('modeled observation incomplete')
        return view


async def modeled_shared_B(ctx, *, before_recovery=None):
    """Author model of the K1 hook contract, NOT execution of actual shared O7."""
    ctx.B_calls += 1
    views = []
    try:
        for _ in range(3): views.append(await ctx.attach(peer=await ctx.viewer()))
        old = views[-1]
        old.peer.writer.transport.pause_reading()
        try:
            for batch in range(52): await probe.flood_retained(ctx, 'O7-stall-' + str(batch), count=8)
        finally:
            old.peer.writer.transport.resume_reading()
        old.closed = old.reducer.failure = 'slow_consumer'
        old.close_notice = {'method': 'stream.closed', 'reason': {'reason': 'slow_consumer'},
                            'last_good_cursor': deepcopy(old.reducer.cursor)}
        if before_recovery is not None: await before_recovery()
        await old.peer.call('workspace.snapshot')
        await ctx.attach(peer=old.peer, after=old.reducer.cursor, seed=deepcopy(old.reducer))
        ctx.evidence.append({'kind': 'stalled_attachment_recovery', 'stream_reason': 'slow_consumer',
                             'close_notice': old.close_notice, 'native_records_requested': 416, 'fast_viewers': 2,
                             'rpc_socket_alive': True, 'exact_fast_viewer_reconstruction': True})
    finally:
        for view in views: await view.peer.close()


class PressureControls(unittest.IsolatedAsyncioTestCase):
    def setUp(self):
        shared = patch('agent_conformance.cases.stalled_viewer_isolation', modeled_shared_B)
        shared.start(); self.addCleanup(shared.stop)
    async def test_full_workload_and_quiescent_exactly_one_replay(self):
        ctx = PressureContext()
        with patch.object(probe, 'process_rss', return_value=1000):
            await probe.public_retention_bounds(ctx)
        retains = [kwargs for method, kwargs in ctx.native_calls if method == 'retain']
        self.assertEqual(len(retains), 4 * 96 + 2 + 416)
        bursts = [row for row in retains if 'chunks' in row]
        self.assertEqual([row['count'] for row in bursts], [256, 256])
        self.assertEqual([row['start_index'] for row in bursts], [0, 256])
        self.assertEqual([row['timeout'] for row in bursts], [5, 5])
        self.assertEqual(bursts[0]['chunks'], bursts[1]['chunks'])
        self.assertEqual(len(bursts[-1]['chunks']), 8)
        self.assertEqual(''.join(bursts[-1]['chunks']), 'O10-gap-burst:' + 'y' * 8192)
        self.assertTrue(all(len(row['text'].split(':', 1)[1]) == 8192 for row in retains if 'text' in row))
        self.assertEqual(sum(row.get('count', 1) for row in retains), 1312)
        self.assertEqual(len(ctx.after), 2)
        self.assertLessEqual(ctx.max_active, 4)
        self.assertTrue(ctx.assert_quiescent)
        self.assertFalse(ctx.paused)
        pressure = next(row for row in ctx.evidence if row['kind'] == 'retention_pressure')
        self.assertEqual(len(pressure['samples']), 4)
        witness = next(row for row in ctx.evidence if row['kind'] == 'o10_public_gap_after_quiescence')
        self.assertEqual(witness['witness_label'], 'fast public observers, provider-cursor retention')

    async def test_native_uncertainty_never_starts_independent_B(self):
        ctx = PressureContext(); original = ctx.call
        async def uncertain(method, **kwargs):
            if method == 'retain' and 'chunks' in kwargs: raise asyncio.TimeoutError('uncertain first control')
            return await original(method, **kwargs)
        ctx.native = NS(call=uncertain)
        with patch.object(probe, 'process_rss', return_value=1000), self.assertRaises(asyncio.TimeoutError):
            await probe.public_retention_bounds(ctx)
        self.assertEqual((ctx.B_calls, len(ctx.after)), (0, 0))

    async def test_fanout_respects_declared_limit(self):
        ctx = PressureContext(); ctx.viewer_limit = 2
        await probe.provider_retention(ctx, (await ctx.facts())['items'])
        self.assertLessEqual(ctx.max_active, 2)
        self.assertEqual(len(ctx.after), 1)

    async def test_invalid_or_missing_fanout_is_FAIL_not_NA(self):
        for limit in (None, True, 0, -1, '4'):
            ctx = PressureContext(); ctx.viewer_limit = limit
            with self.subTest(limit=limit), self.assertRaises(HarnessGap):
                await probe.provider_retention(ctx, (await ctx.facts())['items'])
            self.assertEqual(ctx.native_calls, [])

    async def test_codex_optional_resource_bound_absent_is_unknown_not_failure(self):
        ctx = PressureContext(); original = ctx.facts
        async def no_resource_bound():
            facts = await original(); facts['caps']['items.stream'].pop('max_viewers'); return facts
        ctx.facts = no_resource_bound
        with patch.object(probe, 'process_rss', return_value=1000):
            await probe.public_retention_bounds(ctx)
        self.assertEqual(len(ctx.after), 2)
        observations = next(row for row in ctx.evidence if row.get('kind') == 'o10_provider_observers')
        self.assertTrue(observations['declared_budgets']['resource_bound_absent_is_unknown'])
        self.assertEqual(observations['attached'], 4)

    async def test_global_budget_is_not_exceeded_by_resource_cap(self):
        ctx = PressureContext(); ctx.provider['limits']['max_item_streams'] = 2
        await probe.provider_retention(ctx, (await ctx.facts())['items'])
        self.assertLessEqual(ctx.max_active, 2)

    async def test_no_declared_stream_budget_is_FAIL(self):
        ctx = PressureContext(); ctx.provider = {}
        facts = await ctx.facts(); facts['caps']['items.stream'].pop('max_viewers')
        with self.assertRaises(HarnessGap): probe.viewer_budget(ctx, facts)

    async def test_B_needs_three_declared_viewers(self):
        ctx = PressureContext(); ctx.viewer_limit = 2
        with self.assertRaises(HarnessGap): await probe.slow_viewer_recovery(ctx)
        self.assertEqual(ctx.native_calls, [])

    async def test_missing_A_is_known_quiescent_and_zero_recovery(self):
        ctx = PressureContext(gap=False)
        with self.assertRaises(probe.MissingProviderGap):
            await probe.provider_retention(ctx, (await ctx.facts())['items'])
        self.assertIsNone(ctx.run)
        self.assertEqual(ctx.after, [])
        self.assertEqual(ctx.B_calls, 0)

    async def test_multiple_provider_gaps_still_one_A_recovery(self):
        ctx = PressureContext(); original = ctx.call
        async def all_gaps(method, **kwargs):
            result = await original(method, **kwargs)
            if method == 'retain' and 'chunks' in kwargs:
                for view in ctx.views:
                    if not view.peer.closed:
                        view.closed = view.reducer.failure = 'retention'
                        view.close_notice = {'method': 'stream.gap', 'reason': {'reason': 'gap', 'gap': 'retention'},
                                             'last_good_cursor': deepcopy(view.reducer.cursor)}
            return result
        ctx.native = NS(call=all_gaps)
        await probe.provider_retention(ctx, (await ctx.facts())['items'])
        self.assertEqual(len(ctx.after), 1)
        self.assertLessEqual(ctx.max_active, 4)

    async def test_A_corruption_cannot_become_missing_gap_then_B_pass(self):
        ctx = PressureContext(); original = ctx.call
        async def corrupt(method, **kwargs):
            result = await original(method, **kwargs)
            if method == 'retain' and 'chunks' in kwargs: ctx.target.reducer.failure = 'bad_append_base'
            return result
        ctx.native = NS(call=corrupt)
        with patch.object(probe, 'process_rss', return_value=1000), self.assertRaises(CheckFailed):
            await probe.public_retention_bounds(ctx)
        self.assertEqual(ctx.B_calls, 0)

    async def test_failed_A_recovery_does_not_start_B(self):
        ctx = PressureContext(); original = ctx.attach
        async def wrong_mode(**kwargs):
            view = await original(**kwargs)
            if kwargs.get('after') is not None: view.initial = 'continue'
            return view
        ctx.attach = wrong_mode
        with patch.object(probe, 'process_rss', return_value=1000), self.assertRaises(CheckFailed):
            await probe.public_retention_bounds(ctx)
        self.assertEqual((ctx.B_calls, len(ctx.after)), (0, 1))

    async def test_B_missing_quiescence_hook_cannot_pass(self):
        ctx = PressureContext()
        async def no_hook(ctx, **kwargs): return None
        with patch('agent_conformance.cases.stalled_viewer_isolation', no_hook), self.assertRaises(CheckFailed):
            await probe.slow_viewer_recovery(ctx)
        self.assertEqual(ctx.after, [])

    async def test_B_duplicate_hook_stops_before_second_native_end(self):
        ctx = PressureContext()
        async def twice(ctx, *, before_recovery):
            await before_recovery(); await before_recovery()
        with patch('agent_conformance.cases.stalled_viewer_isolation', twice), self.assertRaises(CheckFailed):
            await probe.slow_viewer_recovery(ctx)
        ends = [row for method, row in ctx.native_calls if method == 'turn' and row['state'] == 'settled']
        self.assertEqual(len(ends), 1)
        self.assertEqual(ctx.after, [])

    async def test_B_retention_does_not_substitute_for_slow_consumer(self):
        ctx = PressureContext()
        async def wrong(ctx, *, before_recovery):
            await modeled_shared_B(ctx, before_recovery=before_recovery)
            ctx.evidence[-1]['stream_reason'] = 'retention'
        with patch('agent_conformance.cases.stalled_viewer_isolation', wrong), self.assertRaises(CheckFailed):
            await probe.slow_viewer_recovery(ctx)
        self.assertEqual(len(ctx.after), 1)

    async def test_B_failed_fence_never_recovers(self):
        ctx = PressureContext(); original = ctx.call
        async def no_end(method, **kwargs):
            if method == 'turn' and kwargs['state'] == 'settled': return {}
            return await original(method, **kwargs)
        ctx.native = NS(call=no_end)
        with self.assertRaises(HarnessGap): await probe.slow_viewer_recovery(ctx)
        self.assertEqual(ctx.after, [])

    async def test_codex_keeps_original_single512_control(self):
        ctx = PressureContext(); ctx.manifest['wire'] = 'codex'
        with patch.object(probe, 'process_rss', return_value=1000):
            await probe.public_retention_bounds(ctx)
        bursts = [row for method, row in ctx.native_calls if method == 'retain' and 'chunks' in row]
        self.assertEqual(len(bursts), 1)
        self.assertEqual(bursts[0]['count'], 512)
        self.assertNotIn('start_index', bursts[0])
        self.assertEqual(len(ctx.after), 2)

    async def test_whole_helper_unmarked_dead_gap_peer_never_replays(self):
        ctx = PressureContext()
        async def dead(method): raise ConnectionError('old gap-bearing peer is unresponsive')
        with patch.object(probe, 'process_rss', return_value=1000), patch(__name__ + '.readonly_snapshot', dead):
            with self.assertRaises(CheckFailed): await probe.public_retention_bounds(ctx)
        self.assertIsNone(ctx.target.peer.failure)
        self.assertEqual(ctx.after, [])
        self.assertFalse(ctx.paused)

    async def test_uncertain_second_segment_never_retries_or_recovers(self):
        ctx = PressureContext(); original = ctx.call; attempts = []
        async def uncertain(method, **kwargs):
            if method == 'retain' and 'chunks' in kwargs:
                attempts.append(kwargs['start_index'])
                if kwargs['start_index'] == 256: raise asyncio.TimeoutError('uncertain fixture control')
            return await original(method, **kwargs)
        ctx.native = NS(call=uncertain)
        with patch.object(probe, 'process_rss', return_value=1000), self.assertRaises(asyncio.TimeoutError):
            await probe.public_retention_bounds(ctx)
        self.assertEqual(attempts, [0, 256])
        self.assertEqual(ctx.after, [])
        self.assertFalse(ctx.paused)

    async def test_raw_slow_consumer_counterexample_precedes_failed_assertion(self):
        ctx = PressureContext(); original = ctx.call
        async def slow(method, **kwargs):
            result = await original(method, **kwargs)
            if method == 'retain' and 'chunks' in kwargs:
                ctx.target.closed = 'slow_consumer'; ctx.target.reducer.failure = 'slow_consumer'
                ctx.target.close_notice = {'method': 'stream.closed', 'reason': {'reason': 'slow_consumer'},
                                           'last_good_cursor': deepcopy(ctx.target.reducer.cursor)}
            return result
        ctx.native = NS(call=slow)
        with patch.object(probe, 'process_rss', return_value=1000), self.assertRaises(HarnessGap):
            await probe.public_retention_bounds(ctx)
        observations = next(row for row in ctx.evidence if row['kind'] == 'o10_provider_observers')['observations']
        self.assertTrue(any(row['notice'] and row['notice']['reason'] == {'reason': 'slow_consumer'} for row in observations))
        self.assertEqual(len(ctx.after), 1)  # B only, never a substitute A replay.
        self.assertEqual(ctx.B_calls, 1)

    async def test_pi_absent_run_fact_uses_actual_public_state(self):
        ctx = PressureContext(); ctx.omit_run_fact = True
        with patch.object(probe, 'process_rss', return_value=1000):
            await probe.public_retention_bounds(ctx)
        self.assertEqual(len(ctx.after), 2)
        starts = [row for row in ctx.evidence if row['kind'] == 'o10_native_started']
        self.assertTrue(all('run' not in row['public_facts'] for row in starts))
        self.assertTrue(all(row['public_state_item']['run'] for row in starts))

    async def test_absent_public_state_run_is_unknown_not_none(self):
        ctx = PressureContext(); ctx.omit_run_fact = True; ctx.omit_state_run = True
        with self.assertRaises(CheckFailed): await probe.begin_native(ctx, 'test')
        self.assertEqual(ctx.after, [])

    async def test_no_actual_gap_does_not_reset_then_pass(self):
        ctx = PressureContext(gap=False)
        with patch.object(probe, 'process_rss', return_value=1000), self.assertRaises(HarnessGap):
            await probe.public_retention_bounds(ctx)
        self.assertEqual(len(ctx.after), 1)
        self.assertEqual(ctx.B_calls, 1)
        subcases = [row for row in ctx.evidence if row.get('kind') == 'subcase']
        self.assertEqual([row['status'] for row in subcases], ['fail', 'pass'])
        self.assertFalse(ctx.paused)

    async def test_control_ack_without_public_ended_boundary_fails(self):
        ctx = PressureContext(settle=False)
        with self.assertRaises(HarnessGap): await probe.public_retention_bounds(ctx)
        self.assertEqual(ctx.after, [])

    async def test_last_pressure_ack_without_ended_boundary_never_replays(self):
        ctx = PressureContext()
        original = ctx.call
        async def last_unsettled(method, **kwargs):
            if ctx.number == 5 and method == 'turn' and kwargs.get('state') == 'settled':
                return {}  # ACK alone, no ended event/idle facts.
            return await original(method, **kwargs)
        ctx.native = NS(call=last_unsettled)
        with patch.object(probe, 'process_rss', return_value=1000), self.assertRaises(HarnessGap):
            await probe.public_retention_bounds(ctx)
        self.assertEqual(ctx.after, [])
        self.assertFalse(ctx.paused)

    async def test_wrong_ended_run_is_not_quiescent_witness(self):
        ctx = PressureContext()
        async def wrong_end(*args, **kwargs):
            ctx.run = None; ctx.completion = {'id': 'unrelated-run'}
            return {}
        ctx.native = NS(call=wrong_end)
        with self.assertRaises(HarnessGap):
            await probe.settle_native(ctx, 'observed-run-1', 'agent-e1', 'test')

    async def test_epoch_switch_does_not_qualify_as_quiescence(self):
        ctx = PressureContext(); ctx.run = 'observed-run-1'
        with self.assertRaises(CheckFailed):
            await probe.settle_native(ctx, 'observed-run-1', 'different-agent-e0', 'test')


if __name__ == '__main__': unittest.main()
