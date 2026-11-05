#!/usr/bin/env python3
"""Opt-in private Linux ownership lifecycle smoke, not a mouse-action demo.

A normal run writes/removes only its own human-approved fixed-namespace record.
--missing-runtime uses an isolated user namespace; it never chmods/renames the
real runtime directory. Candidate binary and all control sockets are explicit.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import struct
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--vqro', required=True, type=Path)
    parser.add_argument('--scratch-root', type=Path, default=Path(tempfile.gettempdir()))
    parser.add_argument('--app-dir', choices=('vqro-dev', 'vqro'), default='vqro-dev')
    parser.add_argument('--missing-runtime', action='store_true')
    args = parser.parse_args()
    binary = args.vqro.resolve(strict=True)
    root = Path(tempfile.mkdtemp(prefix='tva-', dir=args.scratch_root))
    config = root/'config'/args.app_dir
    for path in (root/'home', config, root/'state', root/'run', root/'work'):
        path.mkdir(parents=True, exist_ok=True, mode=0o700)
    (config/'config.toml').write_text('[update]\nversion_check = false\nmanifest_check = false\n[advanced]\nscrollback_limit_bytes = 10000321\n')
    env = {'HOME': str(root/'home'), 'PATH': os.environ['PATH'], 'XDG_CONFIG_HOME': str(root/'config'),
           'XDG_STATE_HOME': str(root/'state'), 'XDG_RUNTIME_DIR': str(root/'run'), 'TERM': 'xterm-256color',
           'SHELL': '/bin/bash', 'LANG': 'C.UTF-8', 'VQRO_NO_BUNDLED_DEFAULTS': '1'}
    api = config/'vqro.sock'
    if len(str(api).encode()) >= 108:
        raise ValueError('private socket path is too long; choose a shorter --scratch-root')
    command = [str(binary), 'server']
    if args.missing_runtime:
        command = ['unshare', '-Ur', '--', *command]
    with binary.open('rb') as source:
        digest = hashlib.file_digest(source, 'sha256').hexdigest()
    log = (root/'server-output.log').open('wb')
    server = subprocess.Popen(command, env=env, cwd=root/'work', stdin=subprocess.DEVNULL,
                              stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
    record = None
    proof = {'root': str(root), 'binary': str(binary), 'binary_sha256': digest,
             'server_pid': server.pid, 'missing_runtime_user_namespace': args.missing_runtime}

    def birth(pid):
        value = Path(f'/proc/{pid}/stat').read_text()
        return int(value[value.rfind(')')+2:].split()[19])

    original_birth = birth(server.pid)
    proof['server_birth'] = original_birth

    def request(method, params=None):
        with socket.socket(socket.AF_UNIX) as stream:
            stream.settimeout(3)
            stream.connect(str(api))
            peer_pid, uid, _ = struct.unpack('3i', stream.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
            assert peer_pid == server.pid and uid == os.geteuid() and birth(peer_pid) == original_birth
            stream.sendall((json.dumps({'id': 'private-ownership-smoke', 'method': method, 'params': params or {}})+'\n').encode())
            value = json.loads(stream.makefile('rb').readline(1024*1024))
            assert 'error' not in value, value
            return value['result']

    def observed_after(epoch):
        deadline = time.monotonic()+8
        last = None
        while time.monotonic() < deadline:
            if server.poll() is not None:
                raise RuntimeError(f'own server exited {server.returncode}')
            try:
                last = request('server.pty_ownership.status.v1')['status']
                if last and last['epoch'] > epoch and (not last['complete'] if args.missing_runtime else last['complete']):
                    return last
            except (FileNotFoundError, ConnectionRefusedError):
                pass
            time.sleep(.03)
        raise RuntimeError(f'no expected ownership observation: {last}')

    try:
        first = observed_after(0)
        key = first['server']
        runtime_uid = 0 if args.missing_runtime else os.geteuid()
        record = Path(f'/run/user/{runtime_uid}/vqro/pty-owners-v1') / f"{key['boot_id']}-{key['pid']}-{key['birth']}.json"
        if args.missing_runtime:
            assert not record.exists(), 'unusable namespace must not be fabricated'
            baseline_count = 0
        else:
            content = json.loads(record.read_text())
            assert content['ownership']['epoch'] == first['epoch'] and content['ownership']['complete']
            assert content['endpoint'] == str(api)
            baseline_count = len(content['ownership']['owned'])
        created = request('workspace.create', {'cwd': str(root/'work'), 'focus': False})
        process = request('pane.process_info', {'pane_id': created['root_pane']['pane_id']})
        shell_pid = process.get('process_info', {}).get('shell_pid')
        assert type(shell_pid) is int and shell_pid > 0, 'core did not create a live PTY child'
        after = observed_after(first['epoch'])
        if args.missing_runtime:
            assert not record.exists()
            assert not first['complete'] and not after['complete']
            content = {'ownership': {'owned': []}}
        else:
            content = json.loads(record.read_text())
            assert content['ownership']['epoch'] == after['epoch']
            assert len(content['ownership']['owned']) > baseline_count
        proof.update({'initial': first, 'after_create': after, 'owned_after_create': content['ownership']['owned'],
                      'workspace_create': created, 'process_info': process})
        request('server.stop')
        server.wait(timeout=8)
        assert server.returncode == 0, server.returncode
        assert not record.exists(), f'own fixed-namespace record leaked: {record}'
        server_log = config/'vqro-server.log'
        text = server_log.read_text() if server_log.exists() else (root/'server-output.log').read_text()
        assert '10000321' in text, 'private config canary was not observed'
        if args.missing_runtime:
            assert 'ownership publication unavailable' in text, 'publication failure was not logged'
        proof['own_record_absent_after_exit'] = True
        proof['passed'] = True
    finally:
        if server.poll() is None:
            try:
                request('server.stop')
                server.wait(timeout=5)
            except Exception:
                if server.poll() is None and birth(server.pid) == original_birth:
                    server.terminate()
                    server.wait(timeout=5)
        log.close()
        (root/'ownership-smoke.json').write_text(json.dumps(proof, indent=2)+'\n')
        print(json.dumps(proof, indent=2))


if __name__ == '__main__':
    main()
