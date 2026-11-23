#!/usr/bin/env python3
"""Private public-wire gear-popup benchmark; no native agents or inherited endpoints.

Compare two agents.py versions with 30/200 synthetic agents and 128 warm rows each.
Measures command replies, not terminal input-to-paint latency.
"""
from python_interpreter import PYTHON
import argparse
import json
from pathlib import Path
import shutil
import statistics
import subprocess
import tempfile
import time

from e2e_sidebar_scale import Client, wait

ROOT = Path(__file__).resolve().parents[1]


def run(args, count, before):
    root = Path(tempfile.mkdtemp(prefix='popup-', dir=args.scratch))
    for name in ('ext', 'state', 'run', 'home'):
        (root / name).mkdir()
    for source in (ROOT / 'extensions').glob('*.py'):
        shutil.copy2(source, root / 'ext' / source.name)
    if before:
        shutil.copy2(args.before_agents, root / 'ext/agents.py')
    # Instrument only the disposable extension copy; the production patch has no timing hooks.
    source = root / 'ext/agents.py'
    instrumentation = '''
import functools as _bench_functools
_bench_state = threading.local()
def _bench_wrap(cls, name):
    original = getattr(cls, name)
    @_bench_functools.wraps(original)
    def timed(*args, **kwargs):
        started = time.perf_counter()
        try:
            return original(*args, **kwargs)
        finally:
            active = getattr(_bench_state, 'active', None)
            if active is not None:
                active.setdefault(name, []).append((time.perf_counter() - started) * 1000)
    setattr(cls, name, timed)
for _cls, _name in ((Extension, 'fresh'), (Extension, 'publish'), (Extension, 'invoke'), (Agents, 'gear_panel')):
    _bench_wrap(_cls, _name)
_bench_original = Agents.pane_menu
def _bench_pane(self, args):
    _bench_state.active = {}
    try:
        return _bench_original(self, args)
    finally:
        with open(__file__ + '.timings', 'a') as output:
            output.write(json.dumps(_bench_state.active) + '\\n')
        _bench_state.active = None
Agents.pane_menu = _bench_pane
'''
    source.write_text(source.read_text().replace('if __name__ == "__main__":', instrumentation + '\nif __name__ == "__main__":'))
    (root / 'ext/defaults.json').write_text(json.dumps({'version': 1, 'extensions': [
        {'name': 'agents', 'argv': [PYTHON, str(root / 'ext/agents.py')]}]}))
    env = {'HOME': str(root / 'home'), 'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8',
           'VQRO_SOCKET': str(root / 'p.sock'), 'VQRO_STATE_DIR': str(root / 'state'),
           'VQRO_EXTENSIONS_DIR': str(root / 'ext'), 'VQRO_HERDR': '["/bin/false"]',
           'XDG_RUNTIME_DIR': str(root / 'run'), 'HC_SOCKET': str(root / 'h.sock'),
           'HC_SESSION': 'popup-latency'}
    clients = []
    with (root / 'serve.log').open('w') as log:
        core = subprocess.Popen([str(args.vqro), 'serve'], env=env, stdout=log, stderr=log)
    try:
        wait(lambda: (root / 'p.sock').exists())
        control, provider = [Client(str(root / 'p.sock')) for _ in range(2)]
        clients.extend((control, provider))
        gen = provider.call('provider.register', {'name': 'popup-fixture', 'instance': 'private',
            'role': 'backend', 'software': {'name': 'fixture', 'version': '1'},
            'provider_version': '0.1.0', 'capabilities': {},
            'limits': {'max_streams': 1, 'stream_window_bytes': 65536}})['generation']
        provider.call('provider.sync', {'generation': gen, 'resources': [
            {'ref': str(i), 'kind': 'agent', 'incarnation': 'one', 'state': 'live',
             'name': 'Fixture', 'data': {}} for i in range(count)], 'final': True})
        resources = control.call('resource.list')['resources']
        for resource in resources:
            provider.call('view.publish', {'generation': gen, 'view': 'warm-' + resource['id'],
                'content': {'items': [{'type': 'text', 'text': 'x' * 128} for _ in range(128)]}})
        rid = resources[0]['id']
        wait(lambda: 'view/agents-pane-' + rid in control.call('workspace.snapshot')['entries'])
        samples = []
        for i in range(args.samples + 3):
            started = time.perf_counter()
            reply = control.call('command.invoke', {'name': 'agents.pane', 'args': {'resource': rid}})
            elapsed = (time.perf_counter() - started) * 1000
            assert reply['client']['popup']['view'] == 'agents-pane-' + rid, reply
            if i >= 3:
                samples.append(elapsed)
        snap = control.call('workspace.snapshot')
        timings = [json.loads(row) for row in (root / 'ext/agents.py.timings').read_text().splitlines()][3:]
        phases = {name: {'calls': sum(len(row.get(name, [])) for row in timings),
                         'median_ms': statistics.median(sum(row.get(name, [])) for row in timings)}
                  for name in ('fresh', 'gear_panel', 'publish', 'invoke')}
        result = {'agents': count, 'before': before, 'samples': len(samples), 'phases': phases,
                  'snapshot_bytes': len(json.dumps(snap).encode()),
                  'popup_bytes': len(json.dumps(snap['live']['agents-pane-' + rid]).encode()),
                  'median_ms': statistics.median(samples),
                  'p95_ms': sorted(samples)[int((len(samples) - 1) * .95)],
                  'root': str(root)}
        (root / 'receipt.json').write_text(json.dumps(result, indent=2) + '\n')
        return result
    finally:
        if clients:
            try:
                clients[0].call('core.shutdown')
            except Exception:
                core.terminate()
        else:
            core.terminate()
        try:
            core.wait(timeout=10)
        except subprocess.TimeoutExpired:
            core.kill()
            core.wait()
        for client in clients:
            client.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('vqro', type=Path)
    parser.add_argument('--before-agents', type=Path, required=True)
    parser.add_argument('--scratch', type=Path, required=True)
    parser.add_argument('--samples', type=int, default=20)
    parser.add_argument('--report', type=Path, required=True)
    args = parser.parse_args()
    args.scratch.mkdir(parents=True, exist_ok=True)
    results = [run(args, count, before) for count in (30, 200) for before in (True, False)]
    args.report.write_text(json.dumps(results, indent=2) + '\n')
    print(json.dumps(results, indent=2))
    # Generous scheduler headroom, but still detects the multi-second whole-workspace reload.
    assert all(row['p95_ms'] < 100 for row in results if not row['before']), results


if __name__ == '__main__':
    main()
