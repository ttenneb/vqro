#!/usr/bin/env python3
"""Real private hc: coalesced natural exits, automatic pruning and explicit forget converge.
Usage: python3 tests/e2e_herdr_drift.py TARGET [--receipt FILE]
No model calls. Every child has a clean environment and private HOME/XDG/HC/PRIM endpoints.
"""
import argparse
import concurrent.futures
import json
import socket
import subprocess
import tempfile
import time
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('target', type=Path)
    parser.add_argument('--receipt', type=Path)
    args = parser.parse_args()
    binary = args.target.resolve() / 'debug'
    hc_binary = Path.home() / '.local/bin/herdr-core'
    receipt = {'result': 'fail', 'waves': []}
    core = None
    with tempfile.TemporaryDirectory(prefix='phd', dir='/tmp') as temporary:
        root = Path(temporary)  # short opaque socket endpoints; this tree is always removed
        for name in ('home', 'state', 'run', 'config', 'ext'):
            (root / name).mkdir(mode=0o700)
        env = {'HOME': str(root / 'home'), 'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8', 'SHELL': '/bin/sh',
               'XDG_RUNTIME_DIR': str(root / 'run'), 'XDG_STATE_HOME': str(root / 'state'),
               'XDG_CONFIG_HOME': str(root / 'config'), 'HC_SOCKET': str(root / 'h'), 'HC_SESSION': 'drift-private',
               'PRIM_SOCKET': str(root / 'p'), 'PRIM_STATE_DIR': str(root / 'prim'),
               'PRIM_EXTENSIONS_DIR': str(root / 'ext'), 'PRIM_HERDR': json.dumps([str(hc_binary), 'connect'])}
        (root / 'ext/defaults.json').write_text(json.dumps({'version': 1, 'extensions': [
            {'name': 'herdr', 'since': 1, 'argv': [str(binary / 'prim-herdr'), '--name', 'herdr'],
             'kind': 'provider', 'default': True}]}))

        def hc(*argv):
            result = subprocess.run([str(hc_binary), *argv], env=env, capture_output=True, text=True, timeout=30)
            assert result.returncode == 0, result.stderr
            return json.loads(result.stdout)

        def call(method, params=None):
            with socket.socket(socket.AF_UNIX) as connection:
                connection.settimeout(30)
                connection.connect(str(root / 'p'))
                with connection.makefile('rb') as reader:
                    for identifier, (name, values) in enumerate([
                            ('hello', {'client': 'private-drift-test'}), (method, params or {})], 1):
                        connection.sendall((json.dumps({'jsonrpc': '2.0', 'id': identifier, 'method': name,
                                                       'params': values}) + '\n').encode())
                        while True:
                            line = reader.readline()
                            assert line, 'private core disconnected'
                            message = json.loads(line)
                            if message.get('id') == identifier:
                                break
                        assert 'error' not in message, message
                    return message['result']

        def converged(generation):
            deadline = time.monotonic() + 15
            while True:
                native = {row['address']: row for row in hc('ls', '--all', '--json')['terminals']}
                snapshot = call('workspace.snapshot')
                provider = snapshot['providers']['herdr']
                assert provider['generation'] == generation and provider['connected'], 'provider reconnected'
                observed = {row['backend_ref']: row for row in snapshot['resources']
                            if row['provider'] == 'herdr' and row['kind'] == 'terminal'}
                if (native.keys() == observed.keys() and native
                        and all(row['state'] == 'finished' and observed[ref]['state'] == 'finished'
                                and observed[ref]['exit']['exit_code'] == row['exit_code']
                                for ref, row in native.items())):
                    return native, observed
                assert time.monotonic() < deadline, {
                    'native': len(native), 'prim': len(observed),
                    'phantom_live': sum(row['state'] == 'live' for row in observed.values()),
                    'pruned_not_retired': len(observed.keys() - native.keys())}
                time.sleep(.05)

        try:
            receipt['hc'] = hc('server', 'start')['version']
            with (root / 'core.log').open('w') as log:
                core = subprocess.Popen([str(binary / 'prim'), 'serve'], env=env, stdout=log, stderr=subprocess.STDOUT)
            deadline = time.monotonic() + 10
            while True:
                try:
                    provider = call('workspace.snapshot')['providers'].get('herdr', {})
                    if provider.get('synced') and provider.get('connected'):
                        break
                except (OSError, AssertionError):
                    pass
                assert time.monotonic() < deadline, 'private provider startup timeout'
                time.sleep(.05)
            generation = provider['generation']
            receipt['generation'] = generation
            ids = set()
            for wave, command in enumerate(('sleep 1', 'sleep 0.01', 'exit 0')):
                started = time.monotonic()
                def spawn(index):
                    return call('resource.spawn', {'argv': ['/bin/sh', '-c', command],
                                                   'name': f'private-{wave}-{index}'})['resource']['id']
                with concurrent.futures.ThreadPoolExecutor(max_workers=16) as pool:
                    created = list(pool.map(spawn, range(600)))
                assert not ids.intersection(created) and len(set(created)) == 600, 'reused resource id'
                ids.update(created)
                native, observed = converged(generation)
                # hc0.1.21 retains500 finished rows: every wave must exercise actual automatic pruning.
                assert len(native) < 600, 'native finished pruning was not exercised'
                receipt['waves'].append({'command': command, 'spawned': 600, 'native': len(native),
                                         'prim': len(observed), 'seconds': round(time.monotonic() - started, 3)})
            for row in list(observed.values())[:3]:
                call('resource.forget', {'resource': row['id']})
            native, observed = converged(generation)
            receipt.update(result='pass', spawned=len(ids), after_forget=len(native), reconnects=0)
        finally:
            if core is not None:
                try:
                    call('core.shutdown')
                    core.wait(timeout=10)
                except Exception:
                    core.kill()
                    core.wait()
            hc('server', 'stop')
            if args.receipt:
                args.receipt.parent.mkdir(parents=True, exist_ok=True)
                args.receipt.write_text(json.dumps(receipt, indent=2) + '\n')
    print(json.dumps(receipt))


if __name__ == '__main__':
    main()
