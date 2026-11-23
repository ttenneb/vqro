#!/usr/bin/env python3
"""Private real-core/backend + SYNTHETIC Claude peer. No native CLI/model/auth/profile access."""
import sys
if __name__ == '__main__' and sys.platform != 'linux':
    print('SKIP claude_launcher_e2e: requires Linux /proc and namespace/process probes')
    raise SystemExit(77)

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import traceback
from pi_provider_e2e import E2E, REPO, identity


class ClaudeLaunch(E2E):
    def run(self):
        self.env['VQRO_PRINCIPAL'] = 'owner'
        (self.root / 'empty/vqro-bridge-claude').symlink_to(self.args.bridge)
        for source in (REPO / 'extensions').glob('*.py'):
            shutil.copy2(source, self.root / 'ext' / source.name)
        shutil.copytree(REPO / 'extensions/manifests', self.root / 'ext/manifests')
        peer = self.root / 'native-peer'
        shutil.copy2(REPO / 'tests/fixtures/fake_claude_stream.py', peer)
        peer.chmod(0o755)
        ledger = self.root / 'synthetic-native.jsonl'
        started = self.hc('server', 'start')
        self.hc_process = identity(started['pid'])
        assert not self.hc('ls', '--all')['terminals']
        self.start()
        self.call('workspace.apply', {'ops': [{'op': 'set', 'key': 'extension/' + name,
            'value': {'enabled': True, 'kind': kind, 'argv': ['/usr/bin/python3', str(self.root / 'ext' / file)]}}
            for name, kind, file in [('tabs', 'interface', 'tabs.py'), ('agents', 'management', 'agents.py'),
                                    ('coord', 'management', 'coordination.py'), ('claude-code', 'runtime', 'claude_code.py')]],
            'note': 'Private synthetic Claude launcher fixture'})
        required = {'claude-code.new', 'coord.request_route', 'agents.show', 'tabs.show'}
        self.wait(lambda: required <= {row['name'] for row in self.call('command.list')['commands'] if row.get('available')}, 'public Claude launcher')
        runtime = {'name': 'synthetic', 'argv': [str(peer)], 'bridge_argv': [str(self.args.bridge)],
                   'bridge_env': {'FAKE_CLAUDE_LOG': str(ledger), 'CLAUDE_CODE_RESUME_INTERRUPTED_TURN': '1'}}
        self.call('workspace.apply', {'ops': [{'op': 'set', 'key': 'doc/settings/claude-code', 'value': {'runtimes': [runtime]}}],
                                     'note': 'Synthetic native executable only'})
        launched = self.call('command.invoke', {'name': 'claude-code.new', 'args': {'runtime': 'synthetic', 'cwd': str(self.root / 'work')}})
        assert launched.get('agent') and launched['outcome'] == 'observed', launched
        assert launched['request_route'] == {'configured': 'human', 'route': 'human', 'confirmed': True}, launched
        rid, host = launched['agent'], launched['host']
        self.wait(lambda: 'agent.send' in self.resource(rid)['data']['agent']['caps']
                  and self.resource(rid)['data']['agent']['background'] == [], 'native metadata initialization')
        resource = self.resource(rid)
        native = resource['data']['agent']
        assert resource['host'] == host and resource['available'], resource
        assert native['principal'] == launched['principal'] and native['principal'].startswith('p:'), resource
        assert 'account' not in native, 'normal login must not invent configured account/kind'
        assert native['session'] is None and native['queue'] is None, native
        lines = [json.loads(line) for line in ledger.read_text().splitlines()]
        self.tracked.append(identity(next(row['pid'] for row in lines if 'pid' in row)))
        terminal = self.hc('get', self.resource(host)['backend_ref'])
        self.tracked.append(identity(terminal['pid']))
        inputs = [row['in'] for row in lines if 'in' in row]
        assert len(inputs) == 2 and all(row['type'] == 'control_request' and row['request']['subtype'] == 'initialize' for row in inputs), inputs
        assert next(row['forbidden_env_keys'] for row in lines if 'forbidden_env_keys' in row) == []
        assert native['_claude']['input_ledger']['pending'] == 0 and native['_claude']['input_ledger']['complete'] is True
        provider = self.snapshot()['providers'][resource['provider']]
        self.log.append({'launched': launched, 'resource': resource, 'authenticated_provider_metadata': provider})
        args = {'v': 0, 'epoch': native['epoch'], 'mode': 'admit', 'delivery': 'literal', 'origin': {'kind': 'human'},
                'content': [{'type': 'text', 'text': '/clear @private'}, {'type': 'text', 'text': 'second exact block'}]}
        sent = self.call('resource.invoke', {'resource': rid, 'op': 'agent.send', 'args': args})
        assert sent['outcome'] in ('dispatched', 'accepted'), sent
        self.wait(lambda: self.resource(rid)['data']['agent']['state'] == 'idle'
                  and self.resource(rid)['data']['agent'].get('completion'), 'native correlated completion')
        receipt = self.call('resource.invoke', {'resource': rid, 'op': 'agent.receipt', 'args': {'v': 0, 'delivery': 'literal'}})
        assert receipt['data']['stage'] == 'completed' and receipt['data']['may_transform'] == 'unknown', receipt
        lines = [json.loads(line) for line in ledger.read_text().splitlines()]
        user = next(row['in'] for row in lines if row.get('in', {}).get('type') == 'user')
        assert user['message']['content'] == args['content'] and user['client_composed'] is True, user
        assert user['message']['role'] == 'user'
        args = dict(args, delivery='permission', content=[{'type': 'text', 'text': '[[permission]]'}])
        self.call('resource.invoke', {'resource': rid, 'op': 'agent.send', 'args': args})
        self.wait(lambda: self.resource(rid)['data']['agent']['requests'], 'native permission request')
        question = self.resource(rid)['data']['agent']['requests'][0]
        self.call('resource.invoke', {'resource': rid, 'op': 'agent.respond', 'args': {'v': 0, 'epoch': native['epoch'],
            'request': question['request'], 'revision': question['revision'], 'response': {'action': 'accept', 'content': {'behavior': 'allow'}}}})
        self.wait(lambda: self.resource(rid)['data']['agent']['state'] == 'idle' and not self.resource(rid)['data']['agent']['requests'], 'permission response correlation')
        native = self.resource(rid)['data']['agent']
        assert native['queue'] is None and native['_claude']['input_ledger']['pending'] == 0 and native['background'] == [], native
        self.log.append({'native_completion': native, 'literal_receipt': receipt, 'native_user_frame': user, 'model_calls': 0})


def main():
    if sys.platform != 'linux':
        print('SKIP claude_launcher_e2e: requires Linux /proc and namespace/process probes')
        return 77
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--run', action='store_true', required=True)
    target = Path(os.environ.get('CARGO_TARGET_DIR') or REPO / 'target') / 'debug'
    for key, default in (('vqro', target / 'vqro'), ('bridge', target / 'vqro-bridge-claude'),
                         ('hc', Path(shutil.which('herdr-core') or 'herdr-core').resolve())):
        parser.add_argument('--' + key, type=Path, default=default)
    parser.add_argument('--scratch-parent', type=Path, default=REPO / '.run')
    parser.add_argument('--isolated-root', type=Path, help=argparse.SUPPRESS)
    parser.add_argument('--parent-net', help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.isolated_root is None:
        assert all(getattr(args, key).is_absolute() for key in ('vqro', 'bridge', 'hc'))
        args.scratch_parent.mkdir(parents=True, exist_ok=True)
        root = Path(tempfile.mkdtemp(prefix='cc-', dir=args.scratch_parent))
        (root / '.private-e2e').touch()
        command = ['/usr/bin/unshare', '-Urnpmf', '--kill-child=KILL', '--mount-proc', '/usr/bin/python3', str(Path(__file__).resolve()),
                   '--run', '--vqro', str(args.vqro), '--bridge', str(args.bridge), '--hc', str(args.hc),
                   '--isolated-root', str(root), '--parent-net', os.readlink('/proc/self/ns/net')]
        code = subprocess.run(command, cwd=root, env={'HOME': str(root), 'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8',
                              'PYTHONDONTWRITEBYTECODE': '1'}, timeout=180).returncode
        print(json.dumps({'exit': code, 'receipt': str(root / 'receipt.json')}))
        return code
    assert (args.isolated_root / '.private-e2e').is_file() and args.parent_net != os.readlink('/proc/self/ns/net')
    assert not any(key.startswith('HERDR_') for key in os.environ)
    args.agents_extension = args.rust_bridge = True
    test, failed = ClaudeLaunch(args), False
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
        (test.root / 'receipt.json').write_text(json.dumps({'fixture_native': True, 'model_calls': 0, 'failed': failed,
            'environment': test.env, 'log': test.log}, indent=2) + '\n')
    return int(failed)


if __name__ == '__main__':
    raise SystemExit(main())
