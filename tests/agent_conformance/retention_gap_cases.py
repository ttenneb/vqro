# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""O10 public subprobe: real retention gap, public quiescence, ONE cursor replay.

K1 owns the O10 wrapper/accounting validator and delegates here; no @case override.
Uncursored capacity snapshots are samples, never substitutes for gap/replay proof.
"""
import asyncio
from copy import deepcopy
import json

from .cases import flood_retained, has_message, process_rss
from .results import check, HarnessGap, CheckFailed, FixtureFailed


def cursor_valid(cursor):
    return (isinstance(cursor, dict) and isinstance(cursor.get('epoch'), str) and bool(cursor['epoch'])
            and type(cursor.get('seq')) is int and cursor['seq'] >= 0)


def actual_gap(view):
    if view.closed != 'retention':
        raise HarnessGap('O10 legal pressure did not yield a public retention gap (observed ' + str(view.closed) + ')')
    notice = view.close_notice
    check(isinstance(notice, dict) and notice.get('method') == 'stream.gap',
          'O10 retention is an actual typed public stream.gap, not a reset/label')
    raw = notice.get('reason')
    reason = (raw.get('gap', 'unknown') if raw.get('reason') == 'gap' else raw.get('reason', 'unknown')) if isinstance(raw, dict) else raw
    check(reason == 'retention', 'O10 retention label agrees with the actual public gap payload')
    check(view.reducer.failure == 'retention', 'O10 retention closure must not hide earlier item corruption')
    check(view.peer.failure is None, 'O10 retention gap leaves the public RPC connection usable')
    check(cursor_valid(view.reducer.cursor) and notice.get('last_good_cursor') == view.reducer.cursor,
          'O10 recovery uses the exact last applied public cursor')
    return deepcopy(view.reducer.cursor), deepcopy(notice)


def bounded_state(reducer, declaration):
    size = sum(len(json.dumps(row['value'], ensure_ascii=False).encode())
               for row in reducer.entities.values())
    check(len(reducer.entities) <= declaration['entities'], 'retained entity count exceeded its declaration')
    check(size <= declaration['entity_bytes'], 'retained entity bytes exceeded their declaration')
    return size


def state_value(reducer):
    values = [row['value'] for (kind, _), row in reducer.entities.items() if kind == 'state']
    return values[0] if len(values) == 1 else {}


async def public_state(ctx, predicate):
    # Pi does NOT expose run in data.agent facts. Observe the actual public
    # state item; an absent optional fact is unknown, never inferred as run=None.
    peer = await ctx.viewer()
    try:
        view = await ctx.attach(peer=peer)
        check(view.initial == 'reset', 'O10 boundary observation starts a fresh public snapshot')
        await ctx.wait_view(view, lambda reducer: reducer.snapshots > 0 and reducer.staged is None
                            and not reducer.pending and predicate(state_value(reducer)))
        check(view.reducer.epoch == view.epoch, 'O10 boundary snapshot uses the actual reply epoch')
        return deepcopy(state_value(view.reducer))
    finally:
        await peer.close()


async def begin_native(ctx, label):
    prior = await ctx.facts()
    if prior.get('state') != 'idle':
        raise HarnessGap('O10 controlled pressure needs an idle native run boundary, not unrelated work')
    await ctx.native.call('turn', state='start')
    started = await ctx.wait_facts(lambda facts: facts.get('state') == 'running', timeout=5)
    state = await public_state(ctx, lambda value: value.get('state') == 'running'
                               and isinstance(value.get('run'), str) and bool(value['run']))
    ctx.evidence.append({'kind': 'o10_native_started', 'phase': label, 'public_facts': deepcopy(started),
                         'public_state_item': state})
    return state['run'], started.get('epoch')


async def settle_native(ctx, run, epoch, label):
    # Existing source-valid controls: Pi agent_end/agent_settled; Codex
    # turn/completed/thread-status idle. Default fixture script has no timers.
    await ctx.native.call('turn', state='settled')
    try:
        ended = await ctx.wait_facts(lambda facts: facts.get('state') == 'idle'
            and (facts.get('completion') or {}).get('id') == run, timeout=5)
        state = await public_state(ctx, lambda value: value.get('state') == 'idle' and 'run' in value
                                   and value['run'] is None and (value.get('completion') or {}).get('id') == run)
    except CheckFailed as error:
        raise HarnessGap('O10 native pressure did not reach the matching public quiescent boundary') from error
    check(ended.get('epoch') == epoch, 'O10 pressure does not change session/agent epoch')
    ctx.evidence.append({'kind': 'o10_native_quiescent', 'phase': label, 'run': run,
                         'public_facts': deepcopy(ended), 'public_state_item': state,
                         'basis': 'matching public ended/idle facts AND explicit public state-item run=None, not absent fact/control ACK/sleep'})
    return ended


async def sample_snapshot(ctx, needle, declaration):
    peer = await ctx.viewer()
    try:
        view = await ctx.attach(peer=peer)
        check(view.initial == 'reset', 'O10 uncursored capacity sample starts with a real snapshot')
        await ctx.wait_view(view, lambda reducer: reducer.snapshots > 0 and reducer.staged is None
                            and not reducer.pending and has_message(reducer, needle))
        check(view.reducer.epoch == view.epoch and cursor_valid(view.reducer.cursor),
              'O10 capacity sample installs its actual reply epoch/cut')
        return {'entities': len(view.reducer.entities), 'entity_bytes': bounded_state(view.reducer, declaration),
                'cursor': deepcopy(view.reducer.cursor), 'kind': 'uncursored_capacity_sample_not_replay'}
    finally:
        await peer.close()


async def one_replay(ctx, view, declaration, needle):
    cursor, notice = actual_gap(view)
    # An unset failure flag is not an observed RPC response. Read on THAT same
    # gap-bearing connection, before a fresh peer could hide its failure.
    try:
        reply = await asyncio.wait_for(view.peer.call('workspace.snapshot'), timeout=2)
    except Exception as error:
        ctx.evidence.append({'kind': 'o10_gap_connection_rpc_failed', 'method': 'workspace.snapshot',
                             'error': type(error).__name__, 'detail': str(error)})
        raise CheckFailed('O10 gap-bearing connection did not serve its bounded read-only RPC') from error
    check(isinstance(reply, dict) and cursor_valid({'epoch': reply.get('epoch'), 'seq': reply.get('seq')}),
          'O10 same-connection read-only RPC returned an actual workspace snapshot envelope')
    ctx.evidence.append({'kind': 'o10_gap_connection_rpc', 'method': 'workspace.snapshot',
                         'on_gap_bearing_peer': True, 'reply_epoch': reply['epoch'], 'reply_seq': reply['seq']})
    seed = deepcopy(view.reducer)
    baseline = seed.snapshots
    # Exactly ONE after-cursor attachment. Never restart pressure/retry recovery.
    replay = await ctx.attach(peer=await ctx.viewer(), after=cursor, seed=seed)
    check(replay.initial == 'reset', 'O10 actual retained-away cursor resets, never silently joins')
    try:
        await ctx.wait_view(replay, lambda reducer: reducer.snapshots > baseline and reducer.staged is None
                            and not reducer.pending and has_message(reducer, needle), timeout=5)
    except CheckFailed as error:
        ctx.evidence.append({'kind': 'o10_replay_failed', 'attempts': 1, 'notice': replay.close_notice,
                             'failure': replay.reducer.failure, 'last_good_cursor': deepcopy(replay.reducer.cursor)})
        raise HarnessGap('O10 one bounded replay failed despite the public native quiescent boundary') from error
    check(replay.closed is None and replay.reducer.failure is None and replay.peer.failure is None,
          'O10 completed recovery has no gap/corruption/connection failure')
    check(replay.reducer.epoch == replay.epoch == cursor['epoch'] and cursor_valid(replay.reducer.cursor)
          and replay.reducer.cursor['seq'] > cursor['seq'],
          'O10 recovery installs a NEW later snapshot at the same quiescent native epoch')
    size = bounded_state(replay.reducer, declaration)
    ctx.evidence.append({'kind': 'public_retention_replay', 'last_good_cursor': cursor, 'notice': notice,
                         'live_gap': 'retention', 'replay': replay.initial, 'attempts': 1,
                         'recovered_cursor': deepcopy(replay.reducer.cursor),
                         'replay_entities': len(replay.reducer.entities), 'replay_entity_bytes': size})
    return replay


async def public_retention_bounds(ctx):
    declaration = (await ctx.facts()).get('items', {})
    for field in ('entities', 'entity_bytes', 'retain_bytes'):
        check(type(declaration.get(field)) is int and declaration[field] > 0,
              'missing numeric item bound: ' + field)
    pid = ctx.metadata.get('adapter_image', {}).get('pid')
    if not pid: raise FixtureFailed('owned adapter PID missing from execution provenance')
    samples = []
    # Preserve four 96x8192 capacity phases. Sample only AFTER each public ended
    # boundary so an observation cut cannot race still-queued native pressure.
    # These fresh observations never count as recovery or a retention-gap witness.
    for phase in range(4):
        label = 'O10-window-' + str(phase)
        run, epoch = await begin_native(ctx, label)
        needle = await flood_retained(ctx, label)
        await settle_native(ctx, run, epoch, label)
        sample = await sample_snapshot(ctx, needle, declaration)
        sample.update(records=(phase + 1) * 96, adapter_rss_bytes=process_rss(pid))
        samples.append(sample)
    ctx.evidence.append({'kind': 'retention_pressure', 'declared': declaration, 'samples': samples,
                         'native_payload_bytes': 4 * 96 * 8192, 'rss_is_measurement_not_cross_runtime_slo': True})
    # Establish a live committed target cursor BEFORE the final burst. Pause only
    # its reader; facts/control use different public/private connections.
    peer = await ctx.viewer()
    target = await ctx.attach(peer=peer)
    check(target.initial == 'reset', 'O10 live target begins at a fresh committed snapshot')
    await ctx.wait_view(target, lambda reducer: reducer.snapshots > 0)
    check(cursor_valid(target.reducer.cursor) and target.reducer.epoch == target.epoch,
          'O10 live gap target has an installed public cursor')
    initial_bytes = target.reducer.offset
    transport = peer.writer.transport
    run, epoch = await begin_native(ctx, 'O10-gap-burst')
    transport.pause_reading()
    try:
        # Same 512 final messages and final payload as the old burst. Native
        # deltas add legal journal pressure, not an invented gap/retention budget.
        chunks = ['O10-gap-burst:' + 'y' * 1024] + ['y' * 1024] * 7
        await ctx.native.call('retain', count=512, chunks=chunks, timeout=5)
        await settle_native(ctx, run, epoch, 'O10-gap-burst')
        check(not transport.is_reading(), 'O10 target remained stalled throughout native pressure', component='harness')
    finally:
        transport.resume_reading()
    needle = '511:O10-gap-burst:'
    try:
        await peer.wait(lambda: target.closed is not None or has_message(target.reducer, needle), timeout=5)
    except asyncio.TimeoutError as error:
        raise HarnessGap('O10 public pressure produced no completed retention-gap witness') from error
    # No reset-only, slow_consumer, EOF or corrupted-record substitute qualifies.
    actual_gap(target)
    ctx.evidence.append({'kind': 'o10_public_gap_after_quiescence', 'notice': deepcopy(target.close_notice),
                         'observed_wire_bytes': target.reducer.offset - initial_bytes,
                         'burst_final_messages': 512, 'chunks_per_message_requested': 8,
                         'final_text_bytes_per_message_requested': sum(map(len, chunks)),
                         'request_note': 'requested native delta shape, not a claim that all wire bytes were observed'})
    await one_replay(ctx, target, declaration, needle)
