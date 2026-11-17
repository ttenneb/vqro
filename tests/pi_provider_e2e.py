"""Opt-in public-CLI Pi provider E2E on private prim + herdr-core.

python3 tests/pi_provider_e2e.py --run --prim /absolute/prim --hc /absolute/herdr-core
Add --real-pi-metadata for installed Pi 1.5 metadata-only (no agent effects).
Always re-execs in a new user/network namespace, empty PATH and private HOME,
state/config/runtime/sockets. Never installs or contacts a live/default server.
Retains receipt.json and logs in a unique scratch root. Linux test harness.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import subprocess
import sys
import tempfile
import time
import traceback

REPO = Path(__file__).resolve().parents[1]
PACKAGE = Path('/home/bennettgarcia/.local/share/pi1/node_modules/@earendil-works/pi-coding-agent')


def identity(pid):
    try:
        rest = Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()
        return {'pid': pid, 'birth': rest[19], 'state': rest[0]}
    except FileNotFoundError:
        return None


def live(process):
    now = identity(process['pid']) if process else None
    return now and now['birth'] == process['birth'] and now['state'] != 'Z'


class E2E:
    def __init__(self, args):
        self.args, self.root = args, args.isolated_root
        self.log, self.core, self.hc_process = [], None, None
        self.tracked = []
        self.ui_running = False
        for name in ('home', 'config', 'state', 'run', 'work', 'ext', 'empty', 'prim', 'agent', 'sessions'):
            (self.root / name).mkdir(exist_ok=True)
        r = self.root
        if args.agents_extension:
            (r/'empty/python3').symlink_to('/usr/bin/python3')  # host-resolved, private allowlist only
        self.env = {'HOME': str(r/'home'), 'PATH': str(r/'empty'), 'LANG': 'C.UTF-8',
                    'XDG_CONFIG_HOME': str(r/'config'), 'XDG_STATE_HOME': str(r/'state'),
                    'XDG_RUNTIME_DIR': str(r/'run'), 'SHELL': '/bin/sh', 'TERM': 'xterm-256color',
                    'HC_SESSION': 'pi-e2e', 'HC_SOCKET': str(r/'h.sock'),
                    'PRIM_STATE_DIR': str(r/'prim'), 'PRIM_SOCKET': str(r/'p.sock'),
                    'PRIM_EXTENSIONS_DIR': str(r/'ext')}
        assert max(len(self.env[k].encode()) for k in ('HC_SOCKET', 'PRIM_SOCKET')) < 103

    def command(self, argv, allow_error=False):
        p = subprocess.run(argv, env=self.env, cwd=self.root/'work', capture_output=True, text=True, timeout=20)
        try:
            result = json.loads(p.stdout)
        except json.JSONDecodeError:
            result = None
        self.log.append({'argv': argv, 'code': p.returncode, 'result': result, 'stderr': p.stderr})
        if not allow_error and p.returncode:
            raise RuntimeError(self.log[-1])
        return result if not allow_error else p

    def hc(self, *args):
        return self.command([str(self.args.hc), *args])

    def call(self, method, params=None, allow_error=False):
        if self.args.agents_extension and method == 'resource.invoke' and (params or {}).get('op', '').startswith('agent.'):
            args = dict(params.get('args') or {}, resource=params['resource'])
            if 'message' in args:
                args['text'] = args.pop('message')
            if 'id' in args:
                args['request'] = args.pop('id')
            if 'confirmed' in args:
                args['value'] = args.pop('confirmed')
            method, params = 'command.invoke', {'name': 'agents.'+params['op'][6:], 'args': args}
        return self.command([str(self.args.prim), 'call', method, json.dumps(params or {})], allow_error)

    def wait(self, predicate, label, timeout=15):
        end, last = time.monotonic() + timeout, None
        while time.monotonic() < end:
            try:
                last = predicate()
                if last:
                    return last
            except (RuntimeError, OSError, KeyError) as error:
                last = repr(error)
            time.sleep(.1)
        if self.ui_running:
            (self.root/'ui-timeout.txt').write_text(self.ui('capture-pane', '-p', '-t', 'ui:0.0'))
        raise AssertionError(f'{label}: timeout; last={last!r}; see prim.log and receipt.json')

    def snapshot(self):
        return self.call('workspace.snapshot')

    def resource(self, rid):
        return next((r for r in self.snapshot()['resources'] if r['id'] == rid), None)

    def start(self):
        # Backends run as provider extensions: herdr-core is the `herdr` default extension (prim-herdr next to
        # the prim under test), reached through PRIM_HERDR. These private defaults hold only that provider, so
        # the core runs without --safe (safe mode starts nothing, providers included).
        defaults = self.root/'ext'/'defaults.json'
        if not defaults.exists():
            defaults.write_text(json.dumps({'version': 2, 'extensions': [{'name': 'herdr', 'since': 2,
                'argv': [str(Path(self.args.prim).parent/'prim-herdr'), '--name', 'herdr'], 'kind': 'provider', 'default': True}]}))
        env = dict(self.env, PRIM_HERDR=json.dumps([str(self.args.hc), 'connect']))
        with (self.root/'prim.log').open('ab') as output:
            # Safe mode deliberately starts no providers and is not an isolation shortcut.
            self.core = subprocess.Popen([str(self.args.prim), 'serve'],
                                         cwd=self.root/'work', env=env, stdout=output, stderr=subprocess.STDOUT)
        self.tracked.append(identity(self.core.pid))
        self.log.append({'started_prim': self.tracked[-1], 'image_sha256': hashlib.sha256(
            Path(f'/proc/{self.core.pid}/exe').read_bytes()).hexdigest()})
        self.wait(lambda: self.snapshot().get('providers', {}).get('herdr', {}).get('connected'), 'prim herdr registration')
        children = Path(f'/proc/{self.core.pid}/task/{self.core.pid}/children').read_text().split()
        self.tracked.extend(identity(int(pid)) for pid in children)
        if self.args.agents_extension and self.snapshot()['entries'].get('extension/agents'):
            self.wait(self.extension_ready, 'agent commands restored after core restart')

    def extension_ready(self):
        names = {c['name'] for c in self.call('command.list')['commands'] if c.get('available')}
        return {'pi.new', 'agents.answer', 'tabs.show', 'panes.close', 'sidebar.show'} <= names

    def stop(self, crash=False):
        if self.core and self.core.poll() is None:
            (self.core.kill if crash else self.core.terminate)()
            try:
                self.core.wait(timeout=4)
            except subprocess.TimeoutExpired:
                self.core.kill()
                self.core.wait(timeout=3)

    def audit_error(self, cursor, outcome, event_method='resource.invoke'):
        # Observe exact public audits, including CLI errors and physical UI
        # effects. Never repeat an effect merely to discover its outcome.
        with socket.socket(socket.AF_UNIX) as sock:
            sock.settimeout(4)
            sock.connect(self.env['PRIM_SOCKET'])
            with sock.makefile('rwb', buffering=0) as wire:
                for ident, method, params in [(1, 'hello', {'client': 'e2e-audit'}),
                        (2, 'events.subscribe', {'from': cursor['seq'], 'epoch': cursor['epoch'], 'types': ['effect']})]:
                    wire.write((json.dumps({'jsonrpc': '2.0', 'id': ident, 'method': method, 'params': params})+'\n').encode())
                    while True:
                        row = json.loads(wire.readline())
                        if row.get('method') == 'event' and row['params']['type'] == 'effect':
                            event = row['params']['data']
                            if event['method'] == event_method:
                                assert event['outcome'] == outcome, event
                                self.log.append({'audited_effect': event})
                                return event
                        if row.get('id') == ident:
                            assert 'error' not in row, row
                            if ident == 1:
                                break

    def invoke(self, rid, op, args=None, outcome='accepted'):
        params = {'resource': rid, 'op': 'agent.'+op, 'args': args or {}}
        if outcome in ('rejected', 'indeterminate'):
            snapshot = self.snapshot()
            cursor = {key: snapshot[key] for key in ('epoch', 'seq')}
            result = self.call('resource.invoke', params, True)
            assert result.returncode, result.stdout
            self.audit_error(cursor, outcome)
            return result
        result = self.call('resource.invoke', params)
        assert result['outcome'] == outcome, result
        return result

    def agent(self, rid, predicate=lambda r: True):
        def found():
            r = self.resource(rid)
            return r if r and predicate(r) else None
        return self.wait(found, 'agent facts '+rid)

    def probe_terminal_input(self, host):
        resource = self.resource(host)
        caps = self.snapshot()['providers'][resource['provider']]['capabilities']['terminal.input']
        assert caps.get('stream') is True and caps.get('effect') is False, caps
        with socket.socket(socket.AF_UNIX) as sock:
            sock.settimeout(5)
            sock.connect(self.env['PRIM_SOCKET'])
            with sock.makefile('rwb', buffering=0) as wire:
                for ident, method, params in [(1, 'hello', {'client': 'input-transport-probe'}),
                        (2, 'resource.invoke', {'resource': host, 'op': 'terminal.input', 'args': {'text': ''}})]:
                    wire.write((json.dumps({'jsonrpc': '2.0', 'id': ident, 'method': method, 'params': params})+'\n').encode())
                    while True:
                        row = json.loads(wire.readline())
                        if row.get('id') == ident:
                            break
                    if ident == 1:
                        assert 'error' not in row, row
                assert row['error']['data']['kind'] == 'unsupported', row
                assert row['error']['data']['outcome'] == 'rejected', row
                self.log.append({'declared_stream_only_input_rejected': row})

    def terminal_eof(self, host):
        # Stream handles are connection-scoped: attach and input share one public
        # protocol connection. Agent effects still all use the public prim CLI.
        with socket.socket(socket.AF_UNIX) as sock:
            sock.settimeout(5)
            sock.connect(self.env['PRIM_SOCKET'])
            with sock.makefile('rwb', buffering=0) as wire:
                def request(ident, method, params):
                    wire.write((json.dumps({'jsonrpc': '2.0', 'id': ident, 'method': method, 'params': params})+'\n').encode())
                    while True:
                        row = json.loads(wire.readline())
                        if row.get('id') == ident:
                            assert 'error' not in row, row
                            return row['result']
                request(1, 'hello', {'client': 'e2e-terminal-eof'})
                attached = request(2, 'resource.attach', {'resource': host, 'cols': 80, 'rows': 24, 'history': 0})
                result = request(3, 'stream.input', {'stream': attached['stream'], 'text': '\u0004'})
                assert result['outcome'] == 'dispatched', result
                self.log.append({'public_terminal_eof': result})

    def ui(self, *args):
        p = subprocess.run(['/usr/bin/tmux', '-S', str(self.root/'t.sock'), '-f', '/dev/null', *args],
            cwd=self.root/'work', env={'HOME': self.env['HOME'], 'PATH': self.env['PATH'], 'LANG': 'C.UTF-8', 'SHELL': '/bin/sh'},
            capture_output=True, text=True, timeout=5)
        assert not p.returncode, (args, p.stderr)
        return p.stdout

    def ui_frame(self, name, *texts):
        def rendered():
            screen = self.ui('capture-pane', '-p', '-t', 'ui:0.0')
            return screen if all(text in screen for text in texts) else None
        screen = self.wait(rendered, 'UI frame '+name, timeout=8)
        (self.root/('ui-'+name+'.txt')).write_text(screen)
        self.log.append({'ui_frame': name, 'contains': texts})
        return screen

    def ui_menu(self, rid, title):
        self.ui('send-keys', '-t', 'ui:0.0', 'C-g')
        self.ui_frame('prefix-menu', 'pane menu')
        self.ui('send-keys', '-t', 'ui:0.0', 'm')
        self.ui_frame('pane-menu', title)
        context = {'view': 'agents-'+rid, 'kind': 'panel', 'resource': None, 'live': False}
        rows = [(v['value'].get('order', 0), k, v['value']) for k, v in self.snapshot()['entries'].items()
                if k.startswith('menu/') and v['value'].get('where') == 'pane'
                and all(context.get(field) == value for field, value in v['value'].get('when', {}).items())]
        rows.sort(key=lambda r: (r[0], r[1]))
        index = next(i for i, (_, _, item) in enumerate(rows) if item.get('label') == title)
        self.ui('send-keys', '-t', 'ui:0.0', *(['Down']*index), 'Enter')

    def ui_launch(self):
        before = self.snapshot()
        cursor = {key: before[key] for key in ('epoch', 'seq')}
        self.ui('new-session', '-d', '-s', 'ui', '-x', '150', '-y', '36', '/usr/bin/env', '-i',
                *[key+'='+value for key, value in self.env.items()], str(self.args.prim), 'tui')
        self.ui_running = True
        for format_ in ('#{pid}', '#{pane_pid}'):
            self.tracked.append(identity(int(self.ui('display-message', '-p', '-t', 'ui:0.0', format_).strip())))
        self.ui_frame('initial', 'prim')
        self.ui('send-keys', '-t', 'ui:0.0', 'C-g')
        self.ui_frame('new-binding', 'new Pi agent')
        self.ui('send-keys', '-t', 'ui:0.0', 'a')
        agent = self.wait(lambda: next((r for r in self.snapshot()['resources'] if r.get('kind') == 'agent' and r.get('available')), None), 'UI new agent')
        self.ui_frame('agent-ready', 'Ready for a prompt', 'created (observed)')
        event = self.audit_error(cursor, 'observed', 'resource.spawn')
        rid = agent['id']
        self.ui_navigation(rid, agent['host'])
        self.ui_menu(rid, 'Prompt')
        self.ui('send-keys', '-t', 'ui:0.0', '-l', 'ui-typed-prompt')
        self.ui_frame('prompt-input', 'ui-typed-prompt')
        self.ui('send-keys', '-t', 'ui:0.0', 'Enter')
        self.agent(rid, lambda r: r['data']['run']['active'])
        self.ui_frame('working', 'working', 'Running')
        self.ui_menu(rid, 'Abort')
        self.agent(rid, lambda r: r['data']['run']['settled'] and r['data']['run']['aborted'])
        return {'outcome': event['outcome'], 'host': agent['host'], 'agent': rid}

    def ui_navigation(self, rid, host):
        def leaves(node):
            return ([node['view']] if 'view' in node else []) + [view for child in node.get('children', []) for view in leaves(child)] + [view for tab in node.get('tabs', []) for view in leaves(tab['node'])]
        def layout_views():
            return [view for key, entry in self.snapshot()['entries'].items() if key.startswith('layout/') for view in leaves(entry['value'])]
        views = layout_views()
        assert views.count('agents-'+rid) == 1, views
        assert 'term-'+rid not in views and 'term-'+host not in views, views
        # The first actionable row is the generic bridge-terminal escape hatch.
        self.ui('send-keys', '-t', 'ui:0.0', 'Home', 'Enter')
        self.wait(lambda: 'term-'+host in layout_views(), 'explicit bridge terminal tab')
        self.ui_frame('bridge-terminal', 'Pi '+rid+' bridge', 'core connected')
        self.call('command.invoke', {'name': 'sidebar.toggle'})
        screen = self.ui_frame('sidebar-agent', 'Terminals', 'Pi '+rid+' bridge')
        # The agent is a top-level row; its bridge is a collapsed child, not another terminal.
        rows = self.wait(lambda: self.snapshot().get('live', {}).get('sidebar', {}).get('items'), 'sidebar rows')
        agent_row = next(row for row in rows if row.get('args', {}).get('resource') == rid)
        assert agent_row['children'][0]['args']['resource'] == host, rows
        assert not any(row.get('args', {}).get('resource') == host for row in rows), rows
        line = next(i for i, line in enumerate(screen.splitlines()) if i > 2 and 'Pi' in line[:35] and 'bridge' not in line[:35])
        click = f'\x1b[<0;6;{line+1}M\x1b[<0;6;{line+1}m'.encode()
        self.ui('send-keys', '-t', 'ui:0.0', '-H', *[f'{byte:02x}' for byte in click])
        self.ui_frame('sidebar-focused-agent', 'Ready for a prompt', 'Bridge terminal')
        self.ui('send-keys', '-t', 'ui:0.0', 'C-g', 'm')
        self.ui_frame('sidebar-click-focus-menu', 'Prompt', 'Abort')
        self.ui('send-keys', '-t', 'ui:0.0', 'Escape')
        for args in ({'resource': rid}, {'picked': rid}):
            self.call('command.invoke', {'name': 'tabs.show', 'args': args})
        assert layout_views().count('agents-'+rid) == 1
        assert not any(entry['value'].get('kind') == 'terminal' and entry['value'].get('resource') == rid
                       for key, entry in self.snapshot()['entries'].items() if key.startswith('view/'))
        self.call('command.invoke', {'name': 'sidebar.toggle'})
        self.log.append({'agent_navigation': 'PASS', 'one_agent_tab': True, 'bridge_opened_explicitly': host, 'sidebar_mouse_focus': rid})

    def ui_ended(self, rid, host):
        self.call('command.invoke', {'name': 'tabs.show', 'args': {'view': 'agents-'+rid, 'title': 'Pi '+rid}})
        self.ui('new-session', '-d', '-s', 'ui', '-x', '150', '-y', '36', '/usr/bin/env', '-i',
                *[key+'='+value for key, value in self.env.items()], str(self.args.prim), 'tui')
        self.ui_running = True
        self.ui_frame('gone-agent', 'Agent gone', '[gone]')
        self.call('command.invoke', {'name': 'sidebar.toggle'})
        self.ui_frame('ended-sidebar', 'ended', '[gone]')
        self.call('command.invoke', {'name': 'sidebar.close_ended'})
        def cleaned():
            snap = self.snapshot()
            rows = snap.get('live', {}).get('sidebar', {}).get('items', [])
            return not any(row.get('args', {}).get('resource') == host for row in rows)
        self.wait(cleaned, 'ended host hidden after explicit cleanup')
        self.ui_frame('cleaned-ended', 'Terminals')
        self.log.append({'ended_ui': 'PASS', 'gone_agent': rid, 'ended_host': host, 'explicit_cleanup': True})
        self.stop_ui()

    def split_independence(self, rid, native_process):
        self.call('command.invoke', {'name': 'prim.extension.disable', 'args': {'name': 'pi'}})
        assert live(native_process) and self.resource(rid)['available']
        self.call('workspace.apply', {'ops': [{'op': 'set', 'key': 'extension/other-agent', 'value': {'enabled': True,
            'argv': ['/usr/bin/python3', str(REPO/'tests/fixtures/other_agent.py')]}}], 'note': 'Other native runtime fixture'})
        other = self.wait(lambda: next((r for r in self.snapshot()['resources'] if r.get('provider') == 'other-agent' and r.get('available')), None), 'non-Pi agent')
        self.wait(lambda: self.snapshot()['entries'].get('menu/agents.'+other['id']+'.abort'), 'declared other abort menu')
        assert 'menu/agents.'+other['id']+'.prompt' not in self.snapshot()['entries']
        self.invoke(other['id'], 'abort', outcome='observed')
        self.call('command.invoke', {'name': 'agents.show', 'args': {'resource': other['id']}})
        if self.args.agents_ui:
            self.ui_frame('other-runtime', 'Other agent', 'abort_count', 'other native runtime')
        children = Path(f'/proc/{self.core.pid}/task/{self.core.pid}/children').read_text().split()
        self.tracked.extend(identity(int(pid)) for pid in children)
        self.call('command.invoke', {'name': 'prim.extension.disable', 'args': {'name': 'other-agent'}})
        if self.args.agents_ui:
            self.ui_frame('unavailable-agent', 'Agent unavailable')
        self.call('command.invoke', {'name': 'prim.extension.enable', 'args': {'name': 'pi'}})
        self.wait(self.extension_ready, 'Pi launcher re-enabled')
        self.call('command.invoke', {'name': 'prim.extension.disable', 'args': {'name': 'agents'}})
        assert live(native_process) and self.resource(rid)['available']
        auxiliary = self.call('command.invoke', {'name': 'pi.new', 'args': {'runtime': 'fake'}})
        host = self.resource(auxiliary['host'])
        terminal = self.hc('get', host['backend_ref'])
        child_ids = Path(f'/proc/{terminal["pid"]}/task/{terminal["pid"]}/children').read_text().split()
        self.tracked.append(identity(terminal['pid']))
        self.tracked.extend(identity(int(pid)) for pid in child_ids)
        assert host['kind'] == 'terminal' and 'term-'+host['id'] in json.dumps(self.snapshot()['entries'])
        assert self.resource(auxiliary['agent'])['available'], 'agents disable killed the runtime'
        self.call('resource.kill', {'resource': host['id']})  # explicit fixture cleanup only
        self.call('command.invoke', {'name': 'prim.extension.enable', 'args': {'name': 'agents'}})
        self.wait(self.extension_ready, 'neutral agents re-enabled')
        self.log.append({'independent_extensions': 'PASS', 'other_runtime': other['id'], 'terminal_fallback': host['id']})

    def stop_ui(self):
        if self.ui_running:
            try:
                self.ui('kill-server')
            except (AssertionError, subprocess.SubprocessError) as error:
                self.log.append({'ui_stop_error': str(error)})
            self.ui_running = False

    def prompt_latency(self, rid):
        """Same public command path as the prefix menu, excluding human typing/model time."""
        samples = []
        with socket.socket(socket.AF_UNIX) as sock:
            sock.settimeout(5)
            sock.connect(self.env['PRIM_SOCKET'])
            with sock.makefile('rwb', buffering=0) as wire:
                ident = 0
                def send(method, params):
                    nonlocal ident
                    ident += 1
                    wire.write((json.dumps({'jsonrpc': '2.0', 'id': ident, 'method': method, 'params': params})+'\n').encode())
                    return ident
                for method, params in [('hello', {'client': 'prompt-latency'}), ('events.subscribe', {'types': ['resource', 'view.content']})]:
                    request = send(method, params)
                    while json.loads(wire.readline()).get('id') != request:
                        pass
                for index in range(7):
                    start = time.monotonic_ns()
                    params = {'resource': rid, 'op': 'agent.prompt', 'args': {'message': 'latency-'+str(index)}}
                    if self.args.agents_extension:
                        request = send('command.invoke', {'name': 'agents.prompt', 'args': {'resource': rid, 'text': 'latency-'+str(index)}})
                    else:
                        request = send('resource.invoke', params)
                    reply = facts = panel = None
                    while reply is None or facts is None or panel is None:
                        row = json.loads(wire.readline()); now = time.monotonic_ns()
                        if row.get('id') == request:
                            assert row.get('result', {}).get('outcome') == 'accepted', row
                            reply = now
                        elif row.get('method') == 'event':
                            event = row['params']; value = event['data']
                            if event['type'] == 'resource' and value.get('id') == rid and value.get('data', {}).get('run', {}).get('active'):
                                facts = now
                            target = ('agents-' if self.args.agents_extension else 'agent-')+rid
                            if event['type'] == 'view.content' and value.get('view') == target and 'Running' in json.dumps(value.get('content')):
                                panel = now
                    native = [json.loads(line) for line in (self.root/'timing.jsonl').read_text().splitlines()]
                    receive = next(v for v in reversed(native) if v['event'] == 'receive' and v['command'] == 'prompt')
                    emitted = next(v for v in native if v['event'] == 'emit' and v['id'] == receive['id'] and v['command'] == 'prompt')
                    runtime = emitted['ns']-receive['ns']
                    samples.append({'accept_ms': (reply-start)/1e6, 'facts_ms': (facts-start)/1e6,
                                    'panel_ms': (panel-start)/1e6, 'fake_pi_ms': runtime/1e6,
                                    'prim_accept_ms': (reply-start-runtime)/1e6})
                    self.invoke(rid, 'abort')
                    self.agent(rid, lambda r: r['data']['run']['settled'])
        self.log.append({'prompt_latency': samples, 'note': 'persistent public RPC; fake Pi only; no model or CLI startup; seven prompt/abort cycles'})

    def trace(self):
        return [json.loads(line) for line in (self.root/'native.jsonl').read_text().splitlines()]

    def run(self):
        self.command([str(self.args.hc), '--version'])
        started = self.hc('server', 'start')
        self.hc_process = identity(started['pid'])
        assert self.hc('server', 'status')['pid'] == started['pid']
        self.log.append({'started_hc': self.hc_process, 'image_sha256': hashlib.sha256(
            Path(f'/proc/{started["pid"]}/exe').read_bytes()).hexdigest()})
        assert self.hc_process and not self.hc('ls', '--all')['terminals']
        if self.args.agents_extension:
            # Stage the installed-directory layout, not just source imports.
            for helper in (REPO/'extensions').glob('*.py'):
                shutil.copy2(helper, self.root/'ext'/helper.name)
            shutil.copytree(REPO/'extensions/pi_bridge', self.root/'ext/pi_bridge', ignore=shutil.ignore_patterns('__pycache__'))
        self.start()
        if self.args.real_pi_metadata:
            assert json.loads((PACKAGE/'package.json').read_text())['version'] == '1.5.0'
            native = ['/usr/bin/node', str(PACKAGE/'dist/bundle/cli.js'), '--mode', 'rpc', '--no-session',
                      '--session-dir', str(self.root/'sessions'), '--no-extensions', '--no-skills',
                      '--no-prompt-templates', '--no-themes', '--no-tools']
        else:
            native = ['/usr/bin/python3', str(REPO/'tests/fixtures/fake_pi.py'), '--bridge-e2e',
                      '--trace-file', str(self.root/'native.jsonl'), '--timing-file', str(self.root/'timing.jsonl')]
        if self.args.agents_extension:
            assert not self.args.real_pi_metadata, 'extension E2E uses fake Pi only'
            ops = [{'op': 'set', 'key': 'doc/settings/agents', 'value': {'runtimes': [{'name': 'fake', 'argv': native}]}}]
            ops += [{'op': 'set', 'key': 'extension/'+name, 'value': {'enabled': True,
                    'argv': ['/usr/bin/python3', str(self.root/'ext'/(name+'.py'))]}}
                    for name in ('tabs', 'panes', 'agents', 'pi', 'sidebar')]
            self.call('workspace.apply', {'ops': ops, 'note': 'Private agents extension conformance'})
            self.wait(self.extension_ready, 'agents/tabs/panes command registration')
            created = self.ui_launch() if self.args.agents_ui else self.call('command.invoke', {'name': 'pi.new', 'args': {'runtime': 'fake', 'cwd': str(self.root/'work')}})
            assert created.get('agent'), created
            spawned = {'outcome': created['outcome'], 'resource': self.resource(created['host'])}
            layout = [entry['value'] for key, entry in self.snapshot()['entries'].items() if key.startswith('layout/')]
            assert 'agents-'+created['agent'] in json.dumps(layout), 'pi.new did not open the neutral agent tab'
            assert self.snapshot()['entries']['doc/settings/pi']['value']['runtimes'][0]['argv'] == native
            children = Path(f'/proc/{self.core.pid}/task/{self.core.pid}/children').read_text().split()
            self.tracked.extend(identity(int(pid)) for pid in children)
        else:
            spawned = self.call('resource.spawn', {'bridge': True, 'name': 'isolated-pi-provider',
                    'argv': ['/usr/bin/python3', '-m', 'pi_bridge', '--', *native], 'cwd': str(self.root/'work'),
                    'env': {'PYTHONPATH': str(REPO/'extensions'), 'PI_CODING_AGENT_DIR': str(self.root/'agent'),
                            'PI_SKIP_VERSION_CHECK': '1'}})
        assert spawned['outcome'] == 'observed', spawned
        host = spawned['resource']['id']
        def find_agent():
            agents = [r for r in self.snapshot()['resources'] if r['kind'] == 'agent' and r.get('available')]
            assert len(agents) <= 1, agents
            return agents[0] if agents else None
        agent = self.wait(find_agent, 'runtime registration + one agent')
        rid, provider = agent['id'], agent['provider']
        view_key = 'view/agent-'+rid
        view = self.wait(lambda: self.snapshot()['entries'].get(view_key), 'agent view declaration')
        assert view['value']['kind'] == 'panel'
        live_view = self.wait(lambda: self.snapshot().get('live', {}).get('agent-'+rid), 'live agent panel')
        assert len(json.dumps(live_view).encode()) < 32768
        self.log.append({'host': host, 'agent': rid, 'provider': provider, 'view': view_key})
        terminal = self.hc('get', spawned['resource']['backend_ref'])
        bridge_process = identity(terminal['pid'])
        children = Path(f'/proc/{terminal["pid"]}/task/{terminal["pid"]}/children').read_text().split()
        assert len(children) == 1, children
        native_process = identity(int(children[0]))
        self.tracked.extend([bridge_process, native_process])
        if self.args.probe_terminal_invoke:
            self.probe_terminal_input(host)
        if not self.args.real_pi_metadata:
            self.prompt_latency(rid)
            first = self.trace()[0]
            assert not first['bootstrap_in_child'] and not first['herdr_in_child'], first
            assert first['pid'] == native_process['pid']
            assert self.invoke(rid, 'prompt', {'message': 'start'})['data']['disposition'] == 'started'
            self.agent(rid, lambda r: r['data']['run']['active'])
            self.invoke(rid, 'prompt', {'message': 'would-overlap'}, outcome='rejected')
            self.invoke(rid, 'steer', {'message': 'explicit-steering'})
            self.invoke(rid, 'follow_up', {'message': 'explicit-follow-up'})
            queued = self.agent(rid, lambda r: r['data']['queues']['steering'] == ['explicit-steering']
                                and r['data']['queues']['follow_up'] == ['explicit-follow-up'])
            assert not queued['data']['run']['settled']
            if self.args.agents_ui:
                self.ui_frame('queues', 'Queued steering: 1', 'follow-up: 1', 'explicit-steering')
            cleared = self.invoke(rid, 'clear_queue')
            assert cleared['data']['steering'] == ['explicit-steering'], cleared
            self.invoke(rid, 'abort')
            self.agent(rid, lambda r: r['data']['run']['settled'] and r['data']['run']['aborted'])
            self.invoke(rid, 'prompt', {'message': 'fixture-dialog'})
            dialog = self.agent(rid, lambda r: bool(r['data']['requests']))['data']['requests'][0]
            assert dialog['id'] == 'fixture-confirm', dialog
            if self.args.agents_ui:
                before_answer = self.snapshot()
                cursor = {key: before_answer[key] for key in ('epoch', 'seq')}
                self.ui_menu(rid, 'Answer pending request')
                self.ui_frame('requests', 'Requests: '+rid, 'Fixture confirmation')
                self.ui('send-keys', '-t', 'ui:0.0', 'Enter')
                self.ui_frame('confirm-options', 'Yes', 'No')
                self.ui('send-keys', '-t', 'ui:0.0', 'Enter')
                self.audit_error(cursor, 'dispatched')
                self.ui_frame('transcript', 'FIXTURE FINAL transcript tail')
            else:
                self.invoke(rid, 'answer', {'id': dialog['id'], 'confirmed': True}, outcome='dispatched')
            self.agent(rid, lambda r: r['data']['run']['settled'] and not r['data']['requests'])
            if self.args.agents_extension:
                self.split_independence(rid, native_process)
            self.invoke(rid, 'prompt', {'message': 'resume-active-run'})
            before = self.agent(rid, lambda r: r['data']['run']['active'])
            trace = self.trace()
        else:
            before = self.agent(rid, lambda r: not r['data']['run']['active'])
        self.stop_ui()  # owned lifecycle tests; do not race a TUI auto-start
        safe_trace = self.trace() if not self.args.real_pi_metadata else None
        self.call('command.invoke', {'name':'prim.safe_mode','args':{'on':True}})
        self.wait(lambda: not self.resource(rid).get('available'), 'agent unavailable in safe mode')
        assert live(native_process) and live(bridge_process), 'safe mode killed backend-owned execution'
        self.call('command.invoke', {'name':'prim.safe_mode','args':{'on':False}})
        self.agent(rid, lambda r: r.get('available') and r['instance']==before['instance'])
        assert live(native_process) and live(bridge_process)
        if safe_trace is not None: assert self.trace()==safe_trace, 'safe-mode resume replayed effects'
        if self.args.agents_extension: self.wait(self.extension_ready, 'agent UI after safe mode')
        self.log.append({'safe_mode_resume':'PASS','same_agent':rid,'no_effect_replay':True})
        old_host = self.resource(host)
        self.stop(crash=True)
        time.sleep(.3)
        assert live(native_process) and live(bridge_process), 'Pi/bridge died with core'
        self.start()
        after = self.agent(rid, lambda r: r.get('available') and r['provider'] == provider)
        assert after['data']['run'] == before['data']['run'], (before, after)
        assert self.snapshot()['entries'][view_key] == view, 'owned view rewritten on resume'
        resumed_host = self.resource(host)
        assert resumed_host['backend_ref'] == old_host['backend_ref']
        assert resumed_host['incarnation'] == old_host['incarnation']
        assert after['instance'] == before['instance']
        assert self.hc('get', resumed_host['backend_ref'])['pid'] == bridge_process['pid']
        assert live(native_process), 'Pi replaced on reconnect'
        self.wait(lambda: self.snapshot().get('live', {}).get('agent-'+rid), 'resumed live agent panel')
        if not self.args.real_pi_metadata:
            assert self.trace() == trace, 'native commands replayed or child replaced during core restart'
            self.invoke(rid, 'abort')
            self.invoke(rid, 'prompt', {'message': 'crash'}, outcome='indeterminate')
            self.wait(lambda: self.resource(rid) is None, 'Pi exit -> resource.gone')
            assert sum(c.get('message') == 'start' for c in self.trace()) == 1
            assert sum(c.get('message') == 'resume-active-run' for c in self.trace()) == 1
            assert sum(c.get('type') == 'clear_queue' for c in self.trace()) == 1, 'abort silently cleared queues'
            assert sum(c.get('type') == 'extension_ui_response' for c in self.trace()) == 1
        else:
            # Public terminal.input sends canonical EOT to the bridge, never a Pi prompt.
            self.terminal_eof(host)
            self.wait(lambda: self.resource(rid) is None, 'real Pi metadata-only EOF -> gone')
        def finished():
            t = self.hc('get', resumed_host['backend_ref'])
            return t if t['state'] == 'finished' else None
        exited = self.wait(finished, 'bridge terminal exits without forced cleanup', timeout=6)
        assert exited['exit_code'] == (0 if self.args.real_pi_metadata else 23), exited
        assert exited['signal'] is None, exited
        self.wait(lambda: not live(native_process) and not live(bridge_process), 'native runtime exits')
        if self.args.agents_ui:
            self.ui_ended(rid, host)
        self.log.append({'native_exit': exited, 'result': 'PASS', 'stable_agent_id': rid, 'host': host, 'native_effect_replay': False,
                         'real_pi_metadata_only': self.args.real_pi_metadata,
                         'agents_extension': self.args.agents_extension})

    def cleanup(self):
        self.stop_ui()
        self.stop()
        if live(self.hc_process):
            for terminal in self.hc('ls', '--all')['terminals']:
                if terminal['state'] == 'live':
                    self.hc('kill', terminal['address'])
                self.hc('rm', terminal['address'])
            assert not self.hc('ls', '--all')['terminals']
            self.hc('server', 'stop')
            self.wait(lambda: not live(self.hc_process), 'private hc exit', timeout=4)
        self.wait(lambda: all(not live(p) for p in self.tracked if p), 'owned children exit', timeout=5)
        assert not Path(self.env['HC_SOCKET']).exists(), 'private HC socket still exists after stop'
        self.log.append({'cleanup': 'PASS', 'tracked': self.tracked, 'hc': self.hc_process})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--run', action='store_true')
    parser.add_argument('--prim', type=Path, required=True)
    parser.add_argument('--hc', type=Path, required=True)
    parser.add_argument('--real-pi-metadata', action='store_true')
    parser.add_argument('--agents-extension', action='store_true', help='drive agents extension commands with fake Pi')
    parser.add_argument('--agents-ui', action='store_true', help='also drive a real TUI in private tmux with fake Pi')
    parser.add_argument('--probe-terminal-invoke', action='store_true', help='assert declared stream-only input rejects an unsupported effect')
    parser.add_argument('--scratch-parent', type=Path, default=Path.home()/'.local/share/herdr/scratch')
    parser.add_argument('--isolated-root', type=Path, help=argparse.SUPPRESS)
    parser.add_argument('--parent-net', help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.agents_ui:
        args.agents_extension = True
    if not args.isolated_root:
        if not args.run:
            parser.error('requires --run; never part of ordinary unit discovery')
        assert args.prim.is_absolute() and args.hc.is_absolute()
        args.scratch_parent.mkdir(parents=True, exist_ok=True)
        root = Path(tempfile.mkdtemp(prefix='ep-', dir=args.scratch_parent))
        (root/'.private-e2e').touch()
        (root/'home').mkdir()
        command = ['/usr/bin/unshare', '-Urn', '/usr/bin/python3', str(Path(__file__).resolve()), '--run',
                   '--prim', str(args.prim), '--hc', str(args.hc), '--isolated-root', str(root),
                   '--parent-net', os.readlink('/proc/self/ns/net')]
        if args.real_pi_metadata:
            command.append('--real-pi-metadata')
        if args.probe_terminal_invoke:
            command.append('--probe-terminal-invoke')
        if args.agents_extension:
            command.append('--agents-extension')
        if args.agents_ui:
            command.append('--agents-ui')
        result = subprocess.run(command, cwd=root, env={'HOME': str(root/'home'), 'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8'})
        print(json.dumps({'code': result.returncode, 'receipt': str(root/'receipt.json')}), flush=True)
        raise SystemExit(result.returncode)
    assert (args.isolated_root/'.private-e2e').is_file()
    assert args.parent_net != os.readlink('/proc/self/ns/net')
    assert not any(k.startswith('HERDR_') for k in os.environ)
    test, failed = E2E(args), False
    try:
        test.run()
    except BaseException:
        failed = True
        test.log.append({'failure': traceback.format_exc()})
        traceback.print_exc()
    finally:
        try:
            test.cleanup()
        except BaseException:
            failed = True
            test.log.append({'cleanup_failure': traceback.format_exc()})
            traceback.print_exc()
        (test.root/'receipt.json').write_text(json.dumps({'environment': test.env,
            'network_namespace': os.readlink('/proc/self/ns/net'), 'log': test.log}, indent=2)+'\n')
    raise SystemExit(1 if failed else 0)


if __name__ == '__main__':
    main()
