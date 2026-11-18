#!/usr/bin/env python3
"""Private public-protocol sidebar load: 600 terminal + 200 agent facts, 350 workspaces.

One synthetic backend publishes facts, not processes/models; this isolates extension bandwidth from backend
execution and the separately tested runtime registry. No live prim/hc/tmux or inherited HERDR environment.
"""
import argparse
from collections import Counter
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import tempfile
import threading
import time

REPO = Path(__file__).resolve().parents[1]


class Client:
    def __init__(self, endpoint):
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.connect(endpoint)
        self.reader = self.sock.makefile('rb')
        self.writer = self.sock.makefile('wb', buffering=0)
        self.lock, self.cv = threading.Lock(), threading.Condition()
        self.ident, self.replies, self.counts, self.bytes, self.last = 0, {}, Counter(), Counter(), {}
        self.sidebar_event = self.previous_sidebar_event = None
        self.thread = threading.Thread(target=self.read, daemon=True)
        self.thread.start()
        self.call('hello', {'client': 'sidebar-scale'})

    def read(self):
        for wire in self.reader:
            row = json.loads(wire)
            with self.cv:
                if 'method' not in row:
                    self.replies[row['id']] = row
                elif row['method'] == 'event' and row['params']['type'] == 'view.content':
                    self.view_event(row['params'], len(wire))
                self.cv.notify_all()

    def view_event(self, params, wire_bytes):
        """Called under cv; retain two sidebar frames, not an unbounded event journal."""
        value = params['data']; view = value['view']
        self.counts[view] += 1
        self.bytes[view] += wire_bytes
        self.last[view] = value['content']
        if view == 'sidebar':
            self.previous_sidebar_event = self.sidebar_event
            self.sidebar_event = {'seq': params['seq'], 'bytes': wire_bytes, 'content': value['content']}

    def wait_workspace(self, path, name, after=0):
        def contains(rows):
            return any((row.get('id') == 'ws:'+path and row.get('text') == name) or contains(row.get('children', [])) for row in rows)
        with self.cv:
            assert self.cv.wait_for(lambda: self.sidebar_event and self.sidebar_event['seq'] > after and
                contains((self.sidebar_event['content'] or {}).get('items', [])), 30), ('sidebar fence', name)
            return dict(self.counts), dict(self.bytes), self.sidebar_event, self.previous_sidebar_event

    def call(self, method, params=None):
        with self.lock:
            self.ident += 1; ident = self.ident
            self.writer.write((json.dumps({'jsonrpc': '2.0', 'id': ident, 'method': method, 'params': params or {}})+'\n').encode())
        with self.cv:
            assert self.cv.wait_for(lambda: ident in self.replies, 30), method
            row = self.replies.pop(ident)
        assert 'error' not in row, (method, row)
        return row['result']

    def notify(self, method, params):
        with self.lock:
            self.writer.write((json.dumps({'jsonrpc': '2.0', 'method': method, 'params': params})+'\n').encode())

    def stats(self):
        with self.cv:
            return dict(self.counts), dict(self.bytes)

    def close(self):
        try:
            self.sock.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        self.thread.join(2)
        self.reader.close(); self.writer.close(); self.sock.close()


def wait(predicate, timeout=30):
    end = time.monotonic()+timeout
    while time.monotonic() < end:
        value = predicate()
        if value:
            return value
        time.sleep(.05)
    raise AssertionError('private sidebar load did not converge')


def normalized_sidebar(content, path, name):
    """Undo only the fence label, preserving every resource/detail field for comparison."""
    if content is None:
        return None
    def rows(items):
        result = []
        for row in items:
            kept = dict(row)
            if row.get('id') == 'ws:'+path:
                kept['text'] = name
            if 'children' in row:
                kept['children'] = rows(row['children'])
            result.append(kept)
        return result
    return dict(content, items=rows(content.get('items', [])))


def sidebar_fence(control, observer, workspaces, purpose):
    """A public visible change and restore acknowledge both derivation and delivery.

    A snapshot on another connection is not a notification-delivery fence. A
    nonempty sidebar also need not reflect the complete startup inventory yet.
    Only these two explicit label changes may be excluded from load traffic;
    if either frame coalesces a real detail change, count that entire frame.
    """
    started = time.monotonic()
    before_counts, before_bytes = observer.stats()
    path, original = workspaces[0]['path'], workspaces[0]['name']
    marker = 'Measurement fence: '+purpose
    marked = [dict(row, name=marker) if row['path'] == path else row for row in workspaces]
    control.call('view.publish', {'view': 'workspaces', 'content': {'data': marked}})
    _, _, changed, previous = observer.wait_workspace(path, marker)
    changed_normal = normalized_sidebar(changed['content'], path, original)
    marker_only = changed_normal == normalized_sidebar(previous['content'] if previous else None, path, original)
    control.call('view.publish', {'view': 'workspaces', 'content': {'data': workspaces}})
    counts, sizes, restored, previous = observer.wait_workspace(path, original, after=changed['seq'])
    restore_only = restored['content'] == normalized_sidebar(previous['content'] if previous else None, path, original)
    natural_count = counts.get('sidebar', 0)-before_counts.get('sidebar', 0)-int(marker_only)-int(restore_only)
    natural_bytes = sizes.get('sidebar', 0)-before_bytes.get('sidebar', 0)-(changed['bytes'] if marker_only else 0)-(restored['bytes'] if restore_only else 0)
    return {'counts': counts, 'bytes': sizes, 'content': restored['content'], 'receipt': {
        'seconds': time.monotonic()-started, 'marker_seq': changed['seq'], 'restored_seq': restored['seq'],
        'natural_publications': natural_count, 'natural_bytes': natural_bytes,
        'marker_coalesced_detail': not marker_only, 'restore_coalesced_detail': not restore_only}}


def run(args, root):
    for name in ('home', 'state', 'config', 'run', 'ext'):
        (root/name).mkdir()
    for source in (REPO/'extensions').glob('*.py'):
        shutil.copy2(source, root/'ext'/source.name)
    if args.baseline_sidebar:
        shutil.copy2(args.baseline_sidebar, root/'ext/sidebar.py')
    names = ('sidebar', 'agents', 'settings', 'tabs')
    (root/'ext/defaults.json').write_text(json.dumps({'version': 1, 'extensions': [
        {'name': name, 'argv': ['/usr/bin/python3', str(root/'ext'/(name+'.py'))]} for name in names]}))
    env = {'HOME': str(root/'home'), 'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8',
           'PRIM_STATE_DIR': str(root/'state'), 'PRIM_SOCKET': str(root/'p.sock'), 'PRIM_EXTENSIONS_DIR': str(root/'ext'),
           'XDG_CONFIG_HOME': str(root/'config'), 'XDG_STATE_HOME': str(root/'state'), 'XDG_RUNTIME_DIR': str(root/'run'),
           'HC_SOCKET': str(root/'h.sock'), 'HC_SESSION': 'sidebar-scale', 'PRIM_HERDR': '["/bin/false"]'}
    assert len(env['PRIM_SOCKET'].encode()) < 103
    clients, core = [], None
    receipt = {'population': {'terminals': 600, 'agents': 200, 'workspaces': 350}, 'synthetic_facts_only': True,
               'baseline_sidebar': bool(args.baseline_sidebar), 'root': str(root)}
    try:
        with (root/'serve.log').open('w') as log:
            core = subprocess.Popen([str(args.prim), 'serve'], env=env, cwd=root, stdout=log, stderr=subprocess.STDOUT)
        wait(lambda: Path(env['PRIM_SOCKET']).exists())
        def connect():
            client = Client(env['PRIM_SOCKET']); clients.append(client); return client
        control, provider, observer, filtered = connect(), connect(), connect(), connect()
        observer.call('events.subscribe', {'types': ['view.content']})
        filtered.call('events.subscribe', {'types': ['view.content'], 'view_content': {'names': ['themes', 'icons', 'keys'], 'prefixes': ['agent-']}})
        registration = provider.call('provider.register', {'name': 'sidebar-scale', 'instance': 'one-private-inventory',
            'role': 'backend', 'software': {'name': 'synthetic-sidebar-load', 'version': '1'}, 'provider_version': '0.1.0',
            'capabilities': {}, 'limits': {'max_streams': 1, 'stream_window_bytes': 65536}})
        generation = registration['generation']
        workspaces = [{'path': str(root/'work'/f'w{i}'), 'name': f'Workspace {i}', 'worktrees': []} for i in range(350)]
        control.call('view.publish', {'view': 'workspaces', 'content': {'data': workspaces}})
        facts = [{'ref': f't{i}', 'incarnation': 'one', 'kind': 'terminal', 'name': f'Terminal {i}', 'argv': ['sh'],
                  'cwd': workspaces[i % 350]['path'], 'state': 'live', 'status': {'state': 'idle'}} for i in range(600)]
        facts += [{'ref': f'a{i}', 'incarnation': 'one', 'kind': 'agent', 'name': f'Agent {i}',
                   'cwd': workspaces[i % 350]['path'], 'state': 'live', 'status': {'state': 'idle'},
                   'data': {'requests': [], 'queues': {'steering': [], 'follow_up': []}}} for i in range(200)]
        for index in range(0, len(facts), 100):
            provider.call('provider.sync', {'generation': generation, 'resources': facts[index:index+100], 'final': index+100 == len(facts)})
        def ready():
            snap = control.call('workspace.snapshot')
            panels = [name for name in snap.get('live', {}) if name.startswith('agents-r')]
            return snap if len(snap['resources']) == 800 and len(panels) == 200 and snap.get('live', {}).get('sidebar', {}).get('items') else None
        snap = wait(ready)
        fence = sidebar_fence(control, observer, workspaces, 'startup')
        initial = fence['content']
        receipt['startup_fence'] = fence['receipt']
        receipt['initial_sidebar_bytes'] = len(json.dumps(initial).encode())
        receipt['pages'] = initial.get('page')
        if not args.baseline_sidebar:
            assert initial.get('page', {}).get('rows') == 1151, 'startup fence must include all 800 resources and 350 folders'
        for phase, fact in [('off_page', facts[150]), ('on_page', facts[0])]:
            before_count, before_bytes = fence['counts'], fence['bytes']
            before_content = fence['content']
            started = time.monotonic()
            iterations = int(args.seconds*20)
            for tick in range(iterations):
                value = dict(fact, status={'state': 'working' if tick % 2 else 'idle', 'progress': tick % 100})
                provider.notify('resource.upsert', {'generation': generation, 'facts': value})
                if tick % 5 == 0:
                    agent = next(r for r in snap['resources'] if r.get('backend_ref') == 'a150')
                    provider.call('view.publish', {'generation': generation, 'view': 'agent-'+agent['id'], 'content': {'items': [{'text': f'transcript {phase} {tick}'}]}})
                time.sleep(max(0, started+(tick+1)/20-time.monotonic()))
            time.sleep(.8)  # Sample-window drain; the fence below also accounts for any trailing publication.
            counts, sizes = observer.stats()
            elapsed = time.monotonic()-started
            fence = sidebar_fence(control, observer, workspaces, phase)
            settling = fence['receipt']
            receipt[phase] = {'seconds': elapsed, 'updates': iterations, 'settling': settling,
                'sidebar_publications': counts.get('sidebar', 0)-before_count.get('sidebar', 0)+settling['natural_publications'],
                'sidebar_bytes': sizes.get('sidebar', 0)-before_bytes.get('sidebar', 0)+settling['natural_bytes']}
            if phase == 'off_page' and not args.baseline_sidebar:
                assert fence['content'] == before_content, 'an off-page change altered visible sidebar content, even across the fence'
        assert filtered.counts.get('sidebar', 0) == 0, 'view allowlist must exclude the large sidebar before delivery'
        assert filtered.counts.get('agent-'+agent['id'], 0) > 0, 'matching native agent views must still arrive'
        shell = observer.last['agents-'+agent['id']]
        assert any(row.get('type') == 'view' and row.get('view') == 'agent-'+agent['id'] and row.get('readonly') for row in shell['items'])
        assert not any('transcript ' in row.get('text', '') for row in shell['items']), 'neutral panel mirrored native output'
        final = control.call('workspace.snapshot')
        receipt['extensions'] = {name: {key: final['extensions'][name][key] for key in ('display_status', 'crashes')} for name in names}
        assert all(value['display_status'] == 'running' and value['crashes'] == 0 for value in receipt['extensions'].values())
        if not args.baseline_sidebar:
            assert receipt['initial_sidebar_bytes'] < 65536, receipt
            assert receipt['off_page']['sidebar_publications'] == 0, receipt
            assert receipt['on_page']['sidebar_publications'] <= 2*args.seconds+2, receipt
            assert initial['page']['count'] > 1
            control.call('command.invoke', {'name': 'sidebar.page', 'args': {'text': '2'}})
            wait(lambda: control.call('workspace.snapshot')['live']['sidebar'].get('page', {}).get('number') == 2)
        receipt['filtered_sidebar_bytes'] = filtered.bytes.get('sidebar', 0)
        receipt['result'] = 'pass'
    finally:
        if core and core.poll() is None:
            try:
                if clients:
                    clients[0].call('core.shutdown')
                else:
                    core.terminate()
            except Exception as error:
                receipt['shutdown_error'] = str(error)
                core.terminate()  # Only this harness's owned core.
            try:
                core.wait(timeout=8)
            except subprocess.TimeoutExpired:
                core.kill(); core.wait(timeout=5)
        for client in clients:
            client.close()
        receipt['cleanup'] = 'pass' if core and core.poll() is not None else 'failed'
        (root/'receipt.json').write_text(json.dumps(receipt, indent=2)+'\n')
        if args.receipt:
            args.receipt.write_text(json.dumps(receipt, indent=2)+'\n')
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('prim', type=Path)
    parser.add_argument('--seconds', type=int, default=5, help='seconds per phase at 20 resource updates/second')
    parser.add_argument('--scratch-parent', type=Path, default=Path.home()/'.local/share/herdr/scratch/prim-sidebar-scale')
    parser.add_argument('--baseline-sidebar', type=Path)
    parser.add_argument('--receipt', type=Path)
    args = parser.parse_args()
    assert not any(key.startswith('HERDR_') for key in os.environ), 'run under env -i'
    args.prim = args.prim.resolve()
    args.scratch_parent.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix='ss-', dir=args.scratch_parent))
    receipt = run(args, root)
    print(json.dumps(receipt, indent=2))


if __name__ == '__main__':
    main()
