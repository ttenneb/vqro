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
from pi_provider_e2e import E2E, REPO, identity, live
from extension_layout import copy_packages


class ClaudeLaunch(E2E):
    def run(self):
        self.env['VQRO_PRINCIPAL'] = 'owner'
        (self.root / 'empty/vqro-bridge-claude').symlink_to(self.args.bridge)
        for source in (REPO / 'extensions').glob('*.py'):
            shutil.copy2(source, self.root / 'ext' / source.name)
        shutil.copytree(REPO / 'extensions/manifests', self.root / 'ext/manifests')
        copy_packages(REPO/'extensions',self.root/'ext')
        if self.args.restart_fixture:
            shutil.copy2(REPO/'tests/fixtures/fake_claude_restart_helper.py',self.root/'ext/accounts_keychain_helper.py')
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
        helper_log=self.root/'synthetic-helper.jsonl'
        launch_args={'runtime':'synthetic','cwd':str(self.root/'work')}
        if self.args.restart_fixture:
            backends=[name for name,row in self.snapshot()['providers'].items()
                      if row.get('role')=='backend' and row.get('state')=='connected' and 'terminal.spawn' in row.get('capabilities',{})]
            assert len(backends)==1,backends
            backend=backends[0]
            account={'id':'synthetic-A','label':'Synthetic A','runtime':'claude','kind':'subscription','provider':'anthropic',
                     'secret_ref':'a'*32,'host':backend}
            self.call('workspace.apply',{'ops':[{'op':'set','key':'doc/settings/accounts','value':{'accounts':[account]}}],
                                         'note':'Synthetic named credential reference; no real keychain'})
            runtime['bridge_env'].update(FAKE_RESTART_LOG=str(helper_log),FAKE_RESTART_SESSION='same',FAKE_RESTART_ALLOW_INITIAL='1')
            launch_args['account']='synthetic-A'
        self.call('workspace.apply', {'ops': [{'op': 'set', 'key': 'doc/settings/claude-code', 'value': {'runtimes': [runtime]}}],
                                     'note': 'Synthetic native executable only'})
        if self.args.restart_fixture:
            # Raw private bootstrap with a deliberately wrong execution domain: no helper/native
            # process may run before the core-authenticated host mapping has been checked.
            wrong_helper=self.root/'wrong-helper.jsonl';wrong_native=self.root/'wrong-native.jsonl'
            wrong_metadata={key:account[key] for key in ('id','label','provider','kind')}
            wrong_metadata['sources']={key:'vqro:launcher' for key in wrong_metadata}
            principal='p:synthetic-wrong-host'
            wrong=self.call('resource.spawn',{'provider':backend,'bridge':True,'argv':[
                str(self.args.bridge),'--endpoint-principal',principal,'--restart-helper',json.dumps(['/usr/bin/python3',str(self.root/'ext/accounts_keychain_helper.py')]),
                '--launch-credential',json.dumps({'account':'synthetic-A','ref':'a'*32,'host':'different-execution-backend'}),'--',str(peer)],
                'env':{'VQRO_PRINCIPAL':principal,'VQRO_ACCOUNT':json.dumps(wrong_metadata),'FAKE_RESTART_LOG':str(wrong_helper),'FAKE_CLAUDE_LOG':str(wrong_native)}})
            wrong_host=wrong['resource']['id']
            self.wait(lambda:self.resource(wrong_host)['state']=='finished','wrong backend refused before helper')
            assert not wrong_helper.exists() and not wrong_native.exists(),'wrong backend must dispatch neither check nor launch'
            self.log.append({'wrong_backend_refused':wrong_host,'helper_calls':0,'native_calls':0})
        launched = self.call('command.invoke', {'name': 'claude-code.new', 'args': launch_args})
        assert launched.get('agent') and launched['outcome'] == 'observed', launched
        assert launched['request_route'] == {'configured': 'human', 'route': 'human', 'confirmed': True}, launched
        rid, host = launched['agent'], launched['host']
        self.wait(lambda: 'agent.send' in self.resource(rid)['data']['agent']['caps']
                  and self.resource(rid)['data']['agent']['background'] == [], 'native metadata initialization')
        resource = self.resource(rid)
        native = resource['data']['agent']
        assert resource['host'] == host and resource['available'], resource
        assert native['principal'] == launched['principal'] and native['principal'].startswith('p:'), resource
        if self.args.restart_fixture:
            assert native['account']['id']=='synthetic-A'
        else:
            assert 'account' not in native, 'normal login must not invent configured account/kind'
        assert native['session'] is None and native['queue'] is None, native
        lines = [json.loads(line) for line in ledger.read_text().splitlines()]
        initial_native=identity(next(row['pid'] for row in lines if 'pid' in row))
        self.tracked.append(initial_native)
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
        if self.args.restart_fixture:
            args=dict(args,delivery='restart-limit',content=[{'type':'text','text':'[[limit]]'}])
            self.call('resource.invoke',{'resource':rid,'op':'agent.send','args':args})
            self.wait(lambda:self.resource(rid)['data']['agent'].get('availability',{}).get('native',{}).get('subscription_limit') is True,'native terminal subscription witness')
            before=self.resource(rid)['data']['agent']
            restart_args={'v':0,'epoch':before['epoch'],'session':before['session']['id'],'account':'synthetic-A',
                'episode':before['availability']['native']['episode'],'target':{'account':'synthetic-B','ref':'0123456789abcdef0123456789abcdef','host':backend,'revision':3}}
            restarted=self.call('resource.invoke',{'resource':rid,'op':'claude.restart','args':restart_args})
            assert restarted['data']['started'] is True and restarted['data']['resume_confirmed'] is True,restarted
            assert restarted['data']['session']==restart_args['session'] and restarted['data']['epoch']!=restart_args['epoch'],restarted
            self.wait(lambda:self.resource(rid)['data']['agent']['epoch']==restarted['data']['epoch'],'replacement epoch published')
            after=self.resource(rid)['data']['agent'];assert after['account']['id']=='synthetic-B',after
            assert 'label' not in after['account'] and after['account']['sources']=={key:'vqro:restart' for key in ('id','provider','kind')},after
            assert after['host']['incarnation']!=before['host']['incarnation'] and not live(initial_native),after
            entries=[json.loads(line) for line in ledger.read_text().splitlines()]
            pids=[row['pid'] for row in entries if 'pid' in row];assert len(pids)==2,pids
            self.tracked.append(identity(pids[-1]))
            checks=[json.loads(line) for line in helper_log.read_text().splitlines()]
            assert [row['action'] for row in checks]==['check','launch','check','launch'],checks
            assert [row['handle'] for row in checks]==['a'*32,'a'*32,'0123456789abcdef0123456789abcdef','0123456789abcdef0123456789abcdef'],checks
            assert '--resume' not in checks[1]['runtime'],checks
            assert checks[1]['runtime'][0]==str(peer) and checks[-1]['runtime'][0]==str(peer),checks
            assert 'a'*32 not in str(checks[-1]['runtime']) and str(self.root/'ext/accounts_keychain_helper.py') not in checks[-1]['runtime'],checks
            assert checks[-1]['runtime'][-2:]==['--resume',restart_args['session']],checks
            assert len([row for row in entries if row.get('in',{}).get('type')=='user'])==3,'adapter must not replay/continue'
            # A NEW visible input is explicit test/caller work, not a restart side effect. It proves
            # the existing CLI supervisor still serves the replacement process rather than exiting.
            follow={'v':0,'epoch':after['epoch'],'mode':'admit','delivery':'after-restart','origin':{'kind':'human'},'content':[{'type':'text','text':'new visible test input'}]}
            self.call('resource.invoke',{'resource':rid,'op':'agent.send','args':follow})
            self.wait(lambda:self.call('resource.invoke',{'resource':rid,'op':'agent.receipt','args':{'v':0,'delivery':'after-restart'}})['data']['stage']=='completed','new epoch explicit input completion')
            self.log.append({'restart_before':before,'restart_result':restarted,'restart_after':after,'private_helper_trace':checks,'caller_new_delivery':'after-restart','model_calls':0})


def main():
    if sys.platform != 'linux':
        print('SKIP claude_launcher_e2e: requires Linux /proc and namespace/process probes')
        return 77
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--run', action='store_true', required=True)
    parser.add_argument('--restart-fixture', action='store_true', help='Also exercise synthetic owned-process restart; no real helper/native calls')
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
        if args.restart_fixture:
            command.append('--restart-fixture')
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
