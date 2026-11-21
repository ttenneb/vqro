# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""S1 viewer policy: own-budget closure is recoverable; lost state is not."""
import asyncio
from collections import defaultdict
from copy import deepcopy
import json


async def stalled_closure(reader, *, attaches=100):
    """Drain only AFTER load ends, without applying items or granting extra load."""
    streams, size = set(), 0
    while True:
        line = await reader.readline()
        if not line:
            raise ValueError('stalled viewer ended without its slow_consumer stream closure')
        size += len(line)
        if len(line) > 128 << 10 or size > 32 << 20:
            raise ValueError('stalled-viewer closure observation exceeded its byte bound')
        row = json.loads(line)
        ident = row.get('id')
        if type(ident) is int and 2 <= ident < 2 + attaches:
            if 'error' in row:
                raise ValueError('stalled resource.attach was refused')
            stream = (row.get('result') or {}).get('stream')
            if type(stream) is not int or stream <= 0:
                raise ValueError('stalled attach lacks its actual stream identifier')
            streams.add(stream)
        if row.get('method') in ('stream.closed', 'stream.gap'):
            params = row.get('params') or {}
            reason = params.get('reason')
            if isinstance(reason, dict): reason = reason.get('reason')
            if (row['method'] != 'stream.closed' or reason != 'slow_consumer'
                    or type(params.get('stream')) is not int or params['stream'] not in streams):
                raise ValueError('stalled stream did not close with its own slow_consumer witness')
            return {'kind': 's1_stalled_closure', 'stream': params['stream'],
                    'reason': reason, 'bytes_observed_after_load': size}


def same_state(views):
    first = views[0].reducer
    return (first.cursor is not None and all(
        view.closed is None and view.peer.failure is None and view.reducer.failure is None
        and view.reducer.staged is None and not view.reducer.pending
        and view.reducer.cursor == first.cursor and view.reducer.entities == first.entities
        and view.reducer.session == first.session and view.reducer.coverage == first.coverage
        for view in views))


async def converge_s1_viewers(ctx, slots, stalled):
    """One reattach per healthy slot, within the existing default observation bound.

    The original 600 slots remain on their original three connections. No bound
    relaxation, invented cursor, extra reference population or retry loop.
    """
    groups = defaultdict(list)
    for index, (rid, _) in enumerate(slots): groups[rid].append(index)
    ctx.check(len(slots) == 600 and len(groups) == 200 and len(stalled) == 2
              and all(len(v) == 3 for v in groups.values()),
              'S1 retains exactly three healthy viewers for each of 200 agents')
    retired = set()
    closures = [asyncio.create_task(stalled_closure(reader)) for reader, _ in stalled]
    try:
        async def ready():
            for index, (rid, view) in enumerate(slots):
                if view.closed is None and view.reducer.failure is None:
                    if view.peer.failure is not None:
                        ctx.check(False, 'healthy S1 viewer RPC connection remains usable')
                    continue
                ctx.check(index not in retired and view.closed == 'slow_consumer'
                          and view.reducer.failure == 'slow_consumer' and view.peer.failure is None
                          and (view.close_notice or {}).get('method') == 'stream.closed',
                          'only a first own-budget slow_consumer closure may reattach',
                          resource=rid, reason=view.closed, failure=view.reducer.failure)
                cursor = deepcopy(view.reducer.cursor)
                # No installed snapshot means no applied cursor: request reset,
                # never manufacture a continue cursor from a partial snapshot.
                seed = deepcopy(view.reducer) if cursor is not None else None
                recovered = await ctx.attach(rid=rid, peer=view.peer, after=cursor, seed=seed)
                retired.add(index)
                slots[index] = (rid, recovered)
                ctx.evidence.append({'kind': 's1_healthy_reattach', 'resource': rid,
                    'old_stream': view.stream, 'new_stream': recovered.stream,
                    'reason': view.closed, 'last_good_cursor': cursor, 'initial': recovered.initial})
            for task in closures:
                if task.done(): task.result()  # a wrong/missing stalled witness must not be ignored
            return (all(task.done() for task in closures)
                    and all(same_state([slots[i][1] for i in indices]) for indices in groups.values()))
        await ctx.wait(ready, label='all 600 S1 healthy streams converge and stalled viewers close slow_consumer')
        ctx.evidence.extend(task.result() for task in closures)
        ctx.check(all(same_state([slots[i][1] for i in indices]) for indices in groups.values()),
                  'all 600 healthy streams converge to their exact healthy reference state', recovered=len(retired))
        return [view for _, view in slots]
    finally:
        for task in closures:
            if not task.done(): task.cancel()
        await asyncio.gather(*closures, return_exceptions=True)
