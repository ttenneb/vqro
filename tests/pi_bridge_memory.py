#!/usr/bin/env python3
"""Accelerated sustained bridge memory gate: real fake-Pi child, bounded socket-pair core.

No model/provider installation, HC, prim endpoint, or user session. The mock core and fallback retain
at most eight/one frames; this measures bridge retention, not an unbounded test observer's history.
"""
import argparse
import asyncio
from collections import deque
import gc
import json
from pathlib import Path
import sys
import tempfile
import tracemalloc

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_pi_provider import BridgeCore, ROOT
from pi_bridge.program import Bridge


class Screen:
    def __init__(self):
        self.frames = deque(maxlen=1)
    def show(self, text):
        self.frames.append(text)
    def close(self):
        pass


def rss():
    try:
        for line in Path('/proc/self/status').read_text().splitlines():
            if line.startswith('VmRSS:'):
                return int(line.split()[1]) * 1024
    except OSError:
        pass
    return None


async def run(args):
    core = BridgeCore()
    sock = await core.start()
    core.rows = deque(maxlen=8)
    stop = asyncio.Event()
    samples = []
    with tempfile.TemporaryDirectory(prefix='bridge-memory-', dir=args.scratch) as root:
        bridge = Bridge([sys.executable, str(ROOT / 'tests/fixtures/fake_pi.py')], cwd=root,
                        env={'HOME': root, 'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8'},
                        token='bootstrap-once', sock=sock, fallback=Screen(), interval=.1)
        tracemalloc.start(8)
        task = asyncio.create_task(bridge.run(stop))
        try:
            await asyncio.wait_for(bridge.ready.wait(), 3)
            count = 0
            baseline = None
            for amount in (args.warmup, args.cycles, args.cycles):
                for _ in range(amount):
                    count += 1
                    for op, payload in [('prompt', {'message': 'memory fixture'}), ('abort', {})]:
                        result = await core.call('effect.invoke', {'effect': f'e{count:09}-{op}',
                            'op': 'agent.'+op, 'ref': 'agent', 'incarnation': '', 'args': payload,
                            'deadline_ms': 2000})
                        assert 'error' not in result, result
                await bridge.publish()
                await asyncio.sleep(.05)
                gc.collect()
                current, peak = tracemalloc.get_traced_memory()
                snap = tracemalloc.take_snapshot()
                if baseline is None:
                    baseline = snap
                sample = {'cycles': count, 'current': current, 'peak': peak, 'rss': rss(),
                          'event_rows': len(bridge.pi._events), 'event_bytes': bridge.pi._event_bytes,
                          'identities': len(bridge.provider._effect_ids),
                          'transcript_rows': len(bridge.transcript.lines),
                          'top_growth': [str(row) for row in snap.compare_to(baseline, 'lineno')[:8]]}
                samples.append(sample)
                print(json.dumps(sample), flush=True)
            first, last = samples[0], samples[-1]
            result = {'samples': samples, 'growth': last['current']-first['current'],
                      'peak_growth': last['peak']-first['peak'],
                      'rss_growth': last['rss']-first['rss'] if last['rss'] is not None and first['rss'] is not None else None}
            if args.report:
                Path(args.report).write_text(json.dumps(result, indent=2)+'\n')
            assert last['event_rows'] <= 512, result
            assert last['identities'] <= 2048, result
            assert result['growth'] < 1048576, result
            assert result['peak_growth'] < 2097152, result
            print('PASS: bounded journal, identity history and sustained traced bridge allocations', flush=True)
        finally:
            stop.set()
            await asyncio.wait_for(asyncio.gather(task, return_exceptions=True), 5)
            await core.close()
            sock.close()
            tracemalloc.stop()
            assert bridge.pi.process.returncode is not None, 'owned fake Pi still running'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--warmup', type=int, default=256)
    parser.add_argument('--cycles', type=int, default=1536)
    parser.add_argument('--scratch')
    parser.add_argument('--report')
    args = parser.parse_args()
    asyncio.run(run(args))


if __name__ == '__main__':
    main()
