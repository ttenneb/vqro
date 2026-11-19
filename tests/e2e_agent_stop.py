"""Opt-in private E2E: Stop agent, prompt removal of dead agents, closing the last tab or only pane.

python3 tests/e2e_agent_stop.py --run --vqro /absolute/vqro --hc /absolute/herdr-core
Reuses tests/pi_provider_e2e.py isolation: new user/network namespace, empty PATH, private HOME, state,
sockets and herdr-core. Fake Pi and a fake non-Pi provider only; no model calls. Retention runs with a
short grace (GRACE seconds) so "dead now" and "still waiting" are told apart by time.
"""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import time
import traceback

sys.path.insert(0, str(Path(__file__).resolve().parent))
import pi_provider_e2e as base  # noqa: E402

REPO = base.REPO
GRACE = 6.0


def tabs_path(node, path=()):
    """Path to the first tab group (the layout's main tabs), as clients send it in tab context."""
    if 'tabs' in node:
        return list(path)
    for i, child in enumerate(node.get('children', [])):
        found = tabs_path(child, path+(i,))
        if found is not None:
            return found
    return None


def node_at(node, path):
    for i in path:
        node = node['children'][i] if 'children' in node else node['tabs'][i]['node']
    return node


class StopE2E(base.E2E):
    def layout(self):
        snap = self.snapshot()
        name = snap['entries']['workspace/active']['value']['layout']
        entry = snap['entries']['layout/'+name]
        return name, entry['version'], entry['value']

    def run(self):
        started = self.hc('server', 'start')
        self.hc_process = base.identity(started['pid'])
        for helper in (REPO/'extensions').glob('*.py'):
            shutil.copy2(helper, self.root/'ext'/helper.name)
        shutil.copytree(REPO/'extensions/pi_bridge', self.root/'ext/pi_bridge', ignore=shutil.ignore_patterns('__pycache__'))
        self.start()
        native = ['/usr/bin/python3', str(REPO/'tests/fixtures/fake_pi.py'), '--bridge-e2e',
                  '--trace-file', str(self.root/'native.jsonl'), '--timing-file', str(self.root/'timing.jsonl')]
        ops = [{'op': 'set', 'key': 'doc/settings/pi', 'value': {'runtimes': [{'name': 'fake', 'argv': native}]}}]
        argv = {name: ['/usr/bin/python3', str(self.root/'ext'/(name+'.py'))] for name in ('tabs', 'panes', 'agents', 'pi', 'sidebar', 'retention')}
        argv['retention'] += ['--grace', str(GRACE)]
        ops += [{'op': 'set', 'key': 'extension/'+name, 'value': {'enabled': True, 'argv': value}} for name, value in argv.items()]
        self.call('workspace.apply', {'ops': ops, 'note': 'Private stop/last-tab E2E'})
        names = {'pi.new', 'agents.stop', 'tabs.close', 'panes.close', 'sidebar.show'}
        self.wait(lambda: names <= {c['name'] for c in self.call('command.list')['commands'] if c.get('available')}, 'commands')
        self.stop_agent()
        self.unexplained_disconnect()
        self.close_last()

    def stop_agent(self):
        created = self.call('command.invoke', {'name': 'pi.new', 'args': {'runtime': 'fake', 'cwd': str(self.root/'work')}})
        rid, host = created['agent'], created['host']
        backend_ref = self.resource(host)['backend_ref']
        terminal = self.hc('get', backend_ref)
        bridge = base.identity(terminal['pid'])
        self.tracked.append(bridge)
        asked = self.call('command.invoke', {'name': 'agents.stop', 'args': {'context': {'view': 'agents-'+rid}}})
        assert asked['client']['popup']['view'] == 'agents-stop-'+rid, asked
        assert self.resource(host)['state'] == 'live' and base.live(bridge), 'asking must not kill'
        popup = self.snapshot()['live']['agents-stop-'+rid]
        assert 'terminal '+host in json.dumps(popup), popup
        confirm = next(row for row in popup['items'] if row.get('command') == 'agents.stop')
        start = time.monotonic()
        result = self.call('command.invoke', {'name': 'agents.stop', 'args': confirm['args']})
        assert result['outcome'] in ('accepted', 'observed'), result
        self.wait(lambda: self.resource(rid) is None, 'stopped agent removed', timeout=GRACE-1)
        removed = time.monotonic()-start
        self.wait(lambda: not base.live(bridge), 'bridge process ended')
        assert self.wait(lambda: self.hc('get', backend_ref)['state'] == 'finished', 'host terminal finished')
        self.log.append({'stop_agent': 'PASS', 'agent': rid, 'host': host, 'kill_outcome': result['outcome'],
                         'removed_after_s': round(removed, 2), 'grace_s': GRACE})

    def unexplained_disconnect(self):
        self.call('workspace.apply', {'ops': [{'op': 'set', 'key': 'extension/other-agent', 'value': {'enabled': True,
            'argv': ['/usr/bin/python3', str(REPO/'tests/fixtures/other_agent.py')]}}], 'note': 'Hostless provider fixture'})
        other = self.wait(lambda: next((r for r in self.snapshot()['resources'] if r.get('provider') == 'other-agent' and r.get('available')), None), 'other agent')
        rid = other['id']
        refused = self.call('command.invoke', {'name': 'agents.stop', 'args': {'resource': rid}}, allow_error=True)
        assert refused.returncode and "can't be stopped from vqro" in refused.stdout+refused.stderr, refused
        self.call('command.invoke', {'name': 'vqro.extension.disable', 'args': {'name': 'other-agent'}})
        start = time.monotonic()
        self.wait(lambda: self.resource(rid) and not self.resource(rid).get('available'), 'other agent disconnected')
        pending = self.wait(lambda: self.snapshot()['live'].get('retention.agents', {}).get('data', {}).get(rid), 'retention deadline published')
        panel = self.wait(lambda: 'forgets it at' in json.dumps(self.snapshot()['live'].get('agents-'+rid, {})) or None, 'panel names the deadline')
        time.sleep(max(0, GRACE/2-(time.monotonic()-start)))
        assert self.resource(rid) is not None, 'unexplained disconnect must keep its grace'
        self.wait(lambda: self.resource(rid) is None, 'removed after the grace', timeout=GRACE+5)
        waited = time.monotonic()-start
        assert waited >= GRACE-1, waited
        self.log.append({'unexplained_disconnect': 'PASS', 'agent': rid, 'forget_at': pending['forget_at'],
                         'removed_after_s': round(waited, 2), 'grace_s': GRACE, 'panel_deadline': bool(panel)})

    def close_last(self):
        name, _, root = self.layout()
        path = tabs_path(root)
        assert path is not None, root
        before = {r['id'] for r in self.snapshot()['resources'] if r['kind'] == 'terminal' and r['state'] == 'live'}
        closed = 0
        while True:
            name, version, root = self.layout()
            tabs = node_at(root, path)['tabs']
            last = len(tabs) == 1
            self.call('command.invoke', {'name': 'tabs.close', 'args': {'context': {
                'layout': name, 'layout_version': version, 'tab_path': path, 'tab_index': 0}}})
            closed += 1
            if last:
                break
        name, version, root = self.layout()
        tabs = node_at(root, path)['tabs']
        assert len(tabs) == 1, tabs
        view = tabs[0]['node'].get('view', '')
        value = self.snapshot()['entries']['view/'+view]['value']
        shell = self.resource(value['resource'])
        assert value['kind'] == 'terminal' and shell['state'] == 'live' and shell['id'] not in before, (value, shell)
        still = {r['id'] for r in self.snapshot()['resources'] if r['kind'] == 'terminal' and r['state'] == 'live'}
        assert before <= still, 'closing tabs killed a program'
        # The only pane of the last tab: same replacement through panes.close.
        self.call('command.invoke', {'name': 'panes.close', 'args': {'context': {
            'layout': name, 'layout_version': version, 'path': path+[0], 'view': view}}})
        name, version, root = self.layout()
        tabs = node_at(root, path)['tabs']
        assert len(tabs) == 1 and tabs[0]['node'].get('view') not in (None, view), tabs
        self.log.append({'close_last': 'PASS', 'tabs_closed': closed, 'replacement': shell['id'],
                         'pane_replacement': tabs[0]['node']['view']})


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--run', action='store_true')
    parser.add_argument('--vqro', type=Path, required=True)
    parser.add_argument('--hc', type=Path, required=True)
    parser.add_argument('--scratch-parent', type=Path, default=Path.home()/'.local/share/herdr/scratch')
    parser.add_argument('--isolated-root', type=Path, help=argparse.SUPPRESS)
    parser.add_argument('--parent-net', help=argparse.SUPPRESS)
    args = parser.parse_args()
    args.agents_extension, args.agents_ui, args.real_pi_metadata = True, False, False
    args.probe_terminal_invoke, args.large_workspace, args.bridge_program = False, False, None
    if not args.isolated_root:
        if not args.run:
            parser.error('requires --run; never part of ordinary unit discovery')
        assert args.vqro.is_absolute() and args.hc.is_absolute()
        args.scratch_parent.mkdir(parents=True, exist_ok=True)
        root = Path(tempfile.mkdtemp(prefix='es-', dir=args.scratch_parent))
        (root/'.private-e2e').touch()
        (root/'home').mkdir()
        command = ['/usr/bin/unshare', '-Urn', '/usr/bin/python3', str(Path(__file__).resolve()), '--run',
                   '--vqro', str(args.vqro), '--hc', str(args.hc), '--isolated-root', str(root),
                   '--parent-net', os.readlink('/proc/self/ns/net')]
        result = subprocess.run(command, cwd=root, env={'HOME': str(root/'home'), 'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8'})
        print(json.dumps({'code': result.returncode, 'receipt': str(root/'receipt.json')}), flush=True)
        raise SystemExit(result.returncode)
    assert (args.isolated_root/'.private-e2e').is_file()
    assert args.parent_net != os.readlink('/proc/self/ns/net')
    assert not any(k.startswith('HERDR_') for k in os.environ)
    test, failed = StopE2E(args), False
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
