#!/usr/bin/env python3
"""Private New Codex agent public-command E2E; fake native only, never auth/model APIs."""

import sys
if __name__ == "__main__" and sys.platform != "linux":
    print("SKIP codex_launcher_e2e: requires Linux /proc and namespace/process probes")
    raise SystemExit(77)

from python_interpreter import python
from extension_layout import copy_packages
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import traceback
import uuid

from pi_provider_e2e import E2E, REPO, identity, live


class CodexLaunch(E2E):
    def run(self):
        self.env['VQRO_PRINCIPAL'] = 'owner'  # configured human caller; launcher must not inherit it
        (self.root / 'empty/vqro-bridge-codex').symlink_to(self.args.bridge)
        for source in (REPO / 'extensions').glob('*.py'):
            shutil.copy2(source, self.root / 'ext' / source.name)
        shutil.copytree(REPO / 'extensions/manifests', self.root / 'ext/manifests')
        copy_packages(REPO/'extensions',self.root/'ext')
        started = self.hc('server', 'start'); self.hc_process = identity(started['pid'])
        assert not self.hc('ls', '--all')['terminals']
        self.start()
        self.call('workspace.apply', {'ops': [{'op': 'set', 'key': 'extension/' + name,
            'value': {'enabled': True, 'kind': kind, 'argv': [python(), str(self.root / 'ext' / file)]}}
            for name, kind, file in [('tabs', 'interface', 'tabs.py'), ('agents', 'management', 'agents.py'),
                ('coord', 'management', 'coordination.py'), ('codex', 'runtime', 'codex.py')]], 'note': 'Private Codex launch fixture'})
        required = {'codex.new', 'coord.request_route', 'agents.show', 'agents.prompt', 'tabs.show'}
        self.wait(lambda: required <= {row['name'] for row in self.call('command.list')['commands'] if row.get('available')}, 'launcher/public UI commands')
        profile = self.root / 'profile'; (profile / 'home').mkdir(parents=True); (profile / 'config').mkdir()
        sentinel = self.root / 'home/.codex/untouched'
        sentinel.parent.mkdir(); sentinel.write_bytes(b'no launcher or adapter profile writes\n')
        before = {str(path): path.read_bytes() for path in sentinel.parent.iterdir()}
        created = []
        for mode in ('dedicated', 'default'):
            observation, ledger = self.root / (mode + '-environment.json'), self.root / (mode + '-native.jsonl')
            native = [python(), '-I', str(REPO / 'tests/fixtures/codex_launcher_native.py'),
                      '--observe', str(observation), '--ledger', str(ledger)]
            runtime = {'name': mode, 'argv': native}
            if mode == 'dedicated': runtime.update(home=str(profile / 'home'), codex_home=str(profile / 'config'))
            self.call('workspace.apply', {'ops': [{'op': 'set', 'key': 'doc/settings/codex', 'value': {'runtimes': [runtime]}}],
                                         'note': 'Private fake native profile selection'})
            launched = self.call('command.invoke', {'name': 'codex.new', 'args': {'runtime': mode, 'cwd': str(self.root / 'work')}})
            assert launched.get('agent') and launched['outcome'] == 'observed', launched
            assert launched['request_route'] == {'configured': 'human', 'route': 'human', 'confirmed': True}, launched
            assert launched['session'] == {'confirmed': True, 'created': True}, launched
            rid, host = launched['agent'], launched['host']
            resource = self.resource(rid)
            assert resource['host'] == host and resource['available'], resource
            # Generic resource.principal is the core's terminal-launch marker;
            # native endpoint identity is its public agent fact (coordination_requests.principal_for).
            assert resource['data']['agent'].get('principal') == launched['principal'] and launched['principal'].startswith('p:'), resource
            assert resource.get('principal') in (None, launched['principal']), resource
            route = self.call('command.invoke', {'name': 'coord.request_route', 'args': {'action': 'inspect', 'agent': rid}})
            assert route['configured'] == route['route'] == 'human', route
            snapshot = self.snapshot()
            assert snapshot['entries']['doc/agents/request-routes/' + rid]['value'] == {'route': 'human'}
            layouts = [row['value'] for key, row in snapshot['entries'].items() if key.startswith('layout/')]
            assert ('agents-' + rid) in json.dumps(layouts), layouts
            observed = json.loads(observation.read_text())
            assert not observed['bridge_token_present'] and not observed['principal_present'] and not observed['herdr_present'], observed
            assert observed['home'] == str(profile / 'home' if mode == 'dedicated' else self.root / 'home'), observed
            assert observed['codex_home'] == (str(profile / 'config') if mode == 'dedicated' else None), observed
            messages = [json.loads(line) for line in ledger.read_text().splitlines()]
            assert sum(row.get('method') == 'thread/start' for row in messages) == 1, messages
            assert not any(row.get('method') in ('turn/start', 'turn/steer') for row in messages), messages
            terminal = self.hc('get', self.resource(host)['backend_ref'])
            self.tracked.append(identity(terminal['pid'])); self.tracked.append(identity(observed['pid']))
            self.log.append({'codex_launch': mode, 'result': launched, 'resource': resource, 'route': route,
                             'native_environment': observed, 'startup_native_methods': [row.get('method') for row in messages]})
            created.append(launched)
        # A real configured SDK caller mapped to the first native endpoint chooses
        # coordinator initialization; role/title/body assertions are never supplied.
        parent = created[0]
        def coord(name, args):
            window = self.call('command.invoke', {'name': 'coord.inbox.window', 'args': {}})
            return self.call('command.invoke', {'name': 'coord.' + name,
                'args': {'op': window['epoch'] + ':codex-test:' + uuid.uuid4().hex, **args}})
        coord('membership.set', {'agent': parent['agent'], 'project': None, 'reports_to': None, 'expect': 0})
        coord('grants', {'id': 'codex-test-launch-standing', 'expect': 0,
            'value': {'who': parent['principal'], 'can': ['configure', 'membership', 'answer'], 'on': '*'}})
        observation = self.root / 'coordinator-environment.json'; ledger = self.root / 'coordinator-native.jsonl'
        native = [python(), '-I', str(REPO / 'tests/fixtures/codex_launcher_native.py'),
                  '--observe', str(observation), '--ledger', str(ledger)]
        self.call('workspace.apply', {'ops': [{'op': 'set', 'key': 'doc/settings/codex',
            'value': {'runtimes': [{'name': 'child', 'argv': native}]}}], 'note': 'Private caller-driven child fixture'})
        self.env['VQRO_PRINCIPAL'] = parent['principal']
        try:
            child = self.call('command.invoke', {'name': 'codex.new', 'args': {'runtime': 'child', 'cwd': str(self.root / 'work')}})
        finally:
            self.env['VQRO_PRINCIPAL'] = 'owner'
        assert child.get('agent') and child['request_route'] == {'configured': 'coordinator', 'route': 'coordinator', 'confirmed': True}, child
        inspected = self.call('command.invoke', {'name': 'coord.request_route', 'args': {'action': 'inspect', 'agent': child['agent']}})
        assert inspected['coordinator'] == parent['agent'] and inspected['human'] is False, inspected
        membership = self.snapshot()['entries']['doc/agents/members/' + child['agent']]['value']
        assert membership['reports_to'] == parent['agent'], membership
        resource = self.resource(child['agent']); observed = json.loads(observation.read_text())
        assert resource['data']['agent']['principal'] == child['principal'] and resource['host'] == child['host'], resource
        assert child['session']['confirmed'] is True and not observed['bridge_token_present'] and not observed['herdr_present'], child
        messages = [json.loads(line) for line in ledger.read_text().splitlines()]
        assert sum(row.get('method') == 'thread/start' for row in messages) == 1 and not any(row.get('method') == 'turn/start' for row in messages)
        terminal = self.hc('get', self.resource(child['host'])['backend_ref'])
        self.tracked.extend([identity(terminal['pid']), identity(observed['pid'])])
        self.log.append({'codex_launch': 'coordinator-child', 'actual_caller': parent['principal'], 'result': child,
                         'route': inspected, 'membership': membership, 'resource': resource, 'native_environment': observed})
        created.append(child)
        assert len({row['principal'] for row in created}) == len({row['host'] for row in created}) == 3
        assert len({row['agent'] for row in created}) == 3
        layouts = [row['value'] for key, row in self.snapshot()['entries'].items() if key.startswith('layout/')]
        for row in created: assert json.dumps(layouts).count('agents-' + row['agent'] + '"') == 1, layouts
        assert {str(path): path.read_bytes() for path in sentinel.parent.iterdir()} == before
        # An actual user command can prompt the newly-created session; still ONLY fake native.
        first = created[0]
        choice = self.call('command.invoke', {'name': 'agents.prompt', 'args': {'resource': first['agent'], 'text': 'synthetic launcher hello'}})
        popup = choice['client']['popup']['view']
        content = self.wait(lambda: self.snapshot().get('live', {}).get(popup), 'actual native-mode chooser')
        send, = [row for row in content['items'] if row.get('command') == 'agents.send' and row.get('args', {}).get('mode') == 'admit']
        assert not any(json.loads(line).get('method') == 'turn/start' for line in (self.root / 'dedicated-native.jsonl').read_text().splitlines())
        self.call('command.invoke', {'name': send['command'], 'args': send['args']})
        def prompted():
            rows = [json.loads(line) for line in (self.root / 'dedicated-native.jsonl').read_text().splitlines()]
            return rows if any(row.get('method') == 'turn/start' for row in rows) else None
        rows = self.wait(prompted, 'fake user prompt native admission')
        assert sum(row.get('method') == 'turn/start' for row in rows) == 1, rows
        self.wait(lambda: 'synthetic launcher hello' in json.dumps(self.snapshot().get('live', {}).get('agents-' + first['agent'], {})),
                  'actual native assistant text in the agent pane')
        self.log.append({'fake_user_prompt': 'PASS', 'native_assistant_view': True, 'model_calls': 0, 'no_automatic_prompt': True,
                         'fresh_principals': True, 'new_tabs': True, 'human_routes': True,
                         'adapter_profile_writes': False, 'bridge_sha256': hashlib.sha256(self.args.bridge.read_bytes()).hexdigest()})


def main():
    if sys.platform != "linux":
        print("SKIP codex_launcher_e2e: requires Linux /proc and namespace/process probes")
        raise SystemExit(77)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--run', action='store_true', required=True)
    target = Path(os.environ.get('CARGO_TARGET_DIR') or REPO / 'target') / 'debug'
    for key, default in (('vqro', target / 'vqro'), ('bridge', target / 'vqro-bridge-codex'),
                         ('hc', Path(shutil.which('herdr-core') or 'herdr-core').resolve())):
        parser.add_argument('--' + key, type=Path, default=default)
    parser.add_argument('--scratch-parent', type=Path, default=REPO / '.run')
    parser.add_argument('--isolated-root', type=Path, help=argparse.SUPPRESS)
    parser.add_argument('--parent-net', help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.isolated_root is None:
        assert all(getattr(args, key).is_absolute() for key in ('vqro', 'bridge', 'hc'))
        args.scratch_parent.mkdir(parents=True, exist_ok=True)
        root = Path(tempfile.mkdtemp(prefix='cx-', dir=args.scratch_parent)); (root / '.private-e2e').touch()
        command = ['/usr/bin/unshare', '-Urnpmf', '--kill-child=KILL', '--mount-proc', python(), str(Path(__file__).resolve()),
                   '--run', '--vqro', str(args.vqro), '--bridge', str(args.bridge), '--hc', str(args.hc),
                   '--isolated-root', str(root), '--parent-net', os.readlink('/proc/self/ns/net')]
        code = subprocess.run(command, cwd=root, env={'HOME': str(root), 'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8',
                              'PYTHONDONTWRITEBYTECODE': '1'}, timeout=180).returncode
        print(json.dumps({'exit': code, 'receipt': str(root / 'receipt.json')}))
        return code
    assert (args.isolated_root / '.private-e2e').is_file() and args.parent_net != os.readlink('/proc/self/ns/net')
    assert not any(key.startswith('HERDR_') for key in os.environ)
    args.agents_extension = args.rust_bridge = True
    test, failed = CodexLaunch(args), False
    try: test.run()
    except BaseException:
        failed = True; test.log.append({'failure': traceback.format_exc()}); traceback.print_exc()
    finally:
        try: test.cleanup()
        except BaseException:
            failed = True; test.log.append({'cleanup_failure': traceback.format_exc()}); traceback.print_exc()
        (test.root / 'receipt.json').write_text(json.dumps({'fixture_native': True, 'model_calls': 0, 'failed': failed,
            'environment': test.env, 'log': test.log}, indent=2) + '\n')
    return int(failed)


if __name__ == '__main__': raise SystemExit(main())
