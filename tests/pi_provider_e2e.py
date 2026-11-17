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
        for name in ('home', 'config', 'state', 'run', 'work', 'ext', 'empty', 'prim', 'agent', 'sessions'):
            (self.root / name).mkdir(exist_ok=True)
        r = self.root
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
        raise AssertionError(f'{label}: timeout; last={last!r}; see prim.log and receipt.json')

    def snapshot(self):
        return self.call('workspace.snapshot')

    def resource(self, rid):
        return next((r for r in self.snapshot()['resources'] if r['id'] == rid), None)

    def start(self):
        with (self.root/'prim.log').open('ab') as output:
            self.core = subprocess.Popen([str(self.args.prim), 'serve', *([] if self.args.agents_extension else ['--safe']), '--herdr', str(self.args.hc)+' connect'],
                                         cwd=self.root/'work', env=self.env, stdout=output, stderr=subprocess.STDOUT)
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
        return {'agents.new', 'agents.answer', 'tabs.show', 'panes.close'} <= names

    def stop(self, crash=False):
        if self.core and self.core.poll() is None:
            (self.core.kill if crash else self.core.terminate)()
            try:
                self.core.wait(timeout=4)
            except subprocess.TimeoutExpired:
                self.core.kill()
                self.core.wait(timeout=3)

    def audit_error(self, cursor, outcome):
        # prim call deliberately prints only an error message; the public effect
        # audit retains its exact rejected/indeterminate outcome. Observe only,
        # never repeat an effect to learn its result.
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
                            if event['method'] == 'resource.invoke':
                                assert event['outcome'] == outcome, event
                                self.log.append({'audited_error': event})
                                return
                        if row.get('id') == ident:
                            assert 'error' not in row, row
                            break
                while True:
                    row = json.loads(wire.readline())
                    if row.get('method') == 'event' and row['params']['type'] == 'effect':
                        event = row['params']['data']
                        if event['method'] == 'resource.invoke':
                            assert event['outcome'] == outcome, event
                            self.log.append({'audited_error': event})
                            return

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
        self.start()
        if self.args.real_pi_metadata:
            assert json.loads((PACKAGE/'package.json').read_text())['version'] == '1.5.0'
            native = ['/usr/bin/node', str(PACKAGE/'dist/bundle/cli.js'), '--mode', 'rpc', '--no-session',
                      '--session-dir', str(self.root/'sessions'), '--no-extensions', '--no-skills',
                      '--no-prompt-templates', '--no-themes', '--no-tools']
        else:
            native = ['/usr/bin/python3', str(REPO/'tests/fixtures/fake_pi.py'), '--bridge-e2e',
                      '--trace-file', str(self.root/'native.jsonl')]
        if self.args.agents_extension:
            assert not self.args.real_pi_metadata, 'extension E2E uses fake Pi only'
            ops = [{'op': 'set', 'key': 'doc/settings/agents', 'value': {'runtimes': [{'name': 'fake', 'argv': native}]}}]
            ops += [{'op': 'set', 'key': 'extension/'+name, 'value': {'enabled': True,
                    'argv': ['/usr/bin/python3', str(REPO/'extensions'/(name+'.py'))]}}
                    for name in ('tabs', 'panes', 'agents')]
            self.call('workspace.apply', {'ops': ops, 'note': 'Private agents extension conformance'})
            self.wait(self.extension_ready, 'agents/tabs/panes command registration')
            created = self.call('command.invoke', {'name': 'agents.new', 'args': {'runtime': 'fake', 'cwd': str(self.root/'work')}})
            assert created.get('agent'), created
            spawned = {'outcome': created['outcome'], 'resource': self.resource(created['host'])}
            layout = [entry['value'] for key, entry in self.snapshot()['entries'].items() if key.startswith('layout/')]
            assert 'agent-'+created['agent'] in json.dumps(layout), 'agents.new did not open the agent tab'
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
            result = self.call('resource.invoke', {'resource': host, 'op': 'terminal.input', 'args': {'text': ''}})
            assert result['outcome'] in ('accepted', 'observed', 'dispatched'), result
        if not self.args.real_pi_metadata:
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
            cleared = self.invoke(rid, 'clear_queue')
            assert cleared['data']['steering'] == ['explicit-steering'], cleared
            self.invoke(rid, 'abort')
            self.agent(rid, lambda r: r['data']['run']['settled'] and r['data']['run']['aborted'])
            self.invoke(rid, 'prompt', {'message': 'fixture-dialog'})
            dialog = self.agent(rid, lambda r: bool(r['data']['requests']))['data']['requests'][0]
            assert dialog['id'] == 'fixture-confirm', dialog
            self.invoke(rid, 'answer', {'id': dialog['id'], 'confirmed': True}, outcome='dispatched')
            self.agent(rid, lambda r: r['data']['run']['settled'] and not r['data']['requests'])
            self.invoke(rid, 'prompt', {'message': 'resume-active-run'})
            before = self.agent(rid, lambda r: r['data']['run']['active'])
            trace = self.trace()
        else:
            before = self.agent(rid, lambda r: not r['data']['run']['active'])
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
        self.log.append({'native_exit': exited, 'result': 'PASS', 'stable_agent_id': rid, 'host': host, 'native_effect_replay': False,
                         'real_pi_metadata_only': self.args.real_pi_metadata,
                         'agents_extension': self.args.agents_extension})

    def cleanup(self):
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
    parser.add_argument('--probe-terminal-invoke', action='store_true', help='regression probe for advertised terminal.input dispatch')
    parser.add_argument('--scratch-parent', type=Path, default=Path.home()/'.local/share/herdr/scratch')
    parser.add_argument('--isolated-root', type=Path, help=argparse.SUPPRESS)
    parser.add_argument('--parent-net', help=argparse.SUPPRESS)
    args = parser.parse_args()
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
