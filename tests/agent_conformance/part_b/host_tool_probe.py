#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Owned two-endpoint native process using ONLY public provider/SDK/CLI routes.

No registration credential or exported tool environment is written to evidence.
This is a shared-host fixture, never an endpoint/native-adapter qualification.
"""
import argparse
import asyncio
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[2]))
from agent_conformance.wire import Peer, RpcError


async def run(args):
    sys.path.insert(0, str(args.sdk))
    from vqro_provider import Provider
    socket = os.environ['VQRO_SOCKET']
    control = await Peer.connect(socket)
    await control.call('hello', {'client': 'g11-control', 'identity': {'kind': 'owner'}})
    backend = Provider('g11-native-backend', 'one', {'terminal.spawn': {'env': True}}, principal='undeclared')
    providers, peers, spawns = [backend], [control], []
    children, bootstrap_env = [], {}

    @backend.effect('terminal.spawn')
    async def spawn(effect, value):
        env = value.get('env', {})
        if 'VQRO_BRIDGE_TOKEN' in env: bootstrap_env.update(env)
        spawns.append({'context_present': 'VQRO_HOST_CONTEXT' in env, 'principal': env.get('VQRO_PRINCIPAL')})
        return {'outcome': 'observed', 'data': {'facts': {'ref': 'native-host-' + str(len(spawns)),
            'incarnation': 'one', 'kind': 'terminal', 'state': 'live', 'argv': [], 'data': {}}}}

    async def hello(token=None, **extra):
        peer = await Peer.connect(socket); peers.append(peer)
        return peer, await peer.call('hello', {'client': 'g11-tool', **({'host_context': token} if token else {}), **extra})

    async def refused(peer, method, value):
        try: await peer.call(method, value)
        except RpcError as error: return error.kind
        return None

    async def cli(env, key):
        # Actual SDK map replaces an inherited owner; tool children ONLY.
        child_env = {**os.environ, 'VQRO_PRINCIPAL': 'owner', **env}
        child = await asyncio.create_subprocess_exec(str(args.cli), 'call', 'workspace.apply',
            json.dumps({'ops': [{'op': 'set', 'key': key, 'value': 1}]}), env=child_env,
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
        children.append(child)
        stdout, stderr = await asyncio.wait_for(child.communicate(), 8)
        return {'key': key, 'exit': child.returncode,
            'reply': json.loads(stdout) if child.returncode == 0 else None,
            'stderr': stderr.decode(errors='replace')[-1024:]}

    try:
        await backend.connect(socket); await backend.full_sync([])
        hosted = await control.call('resource.spawn', {'provider': backend.name, 'bridge': True})
        # Bootstrap comes only from core's actual backend callback, not an asserted host.
        # The effect envelope is captured privately below, never printed.
        bootstrap = bootstrap_env['VQRO_BRIDGE_TOKEN']
        runtime = Provider('g11-two-endpoint-runtime', 'one', {}, role='runtime', token=bootstrap,
                           principal='undeclared', host_context=True)
        providers.append(runtime); await runtime.connect(socket)
        facts = [{'ref': ref, 'incarnation': 'one', 'kind': 'agent', 'state': 'live', 'argv': [], 'data': {}}
                 for ref in ('endpoint-a', 'endpoint-b')]
        await runtime.full_sync(facts)
        registration = runtime.registration
        env = runtime.tool_context_env(socket)
        if not env: raise RuntimeError('public SDK exported no verified tool environment')
        token = env['VQRO_HOST_CONTEXT']
        bound, bound_hello = await hello(token)
        ordinary, ordinary_hello = await hello()
        wrong = await refused(ordinary, 'hello', {'host_context': 'not-a-valid-host-context'})
        raw = await refused(ordinary, 'hello', {'identity': {'kind': 'principal', 'principal': registration['actor']}})
        inventory = await control.call('resource.list', {'provider': runtime.name})
        writes = [await cli(env, 'doc/g11/' + fact['ref']) for fact in facts]
        history = await control.call('workspace.history', {'limit': 20})
        denied = await refused(bound, 'workspace.apply', {'on_behalf_of': 'endpoint-a',
            'ops': [{'op': 'set', 'key': 'doc/g11/no-grant', 'value': 1}]})
        await bound.call('resource.spawn', {'provider': backend.name})
        snapshot = await control.call('workspace.snapshot', {})
        # No token/bootstrap/resume bytes are returned, even on a failed check.
        absent = all(secret not in json.dumps(snapshot) + json.dumps(history)
                     for secret in (token, bootstrap, registration['resume_token']))
        await runtime.close()
        async def disconnected():
            while (await control.call('workspace.snapshot', {}))['providers'][runtime.name].get('connected'):
                await asyncio.sleep(.01)
        await asyncio.wait_for(disconnected(), 8)
        stale_bound = await refused(bound, 'workspace.apply', {'ops': [{'op': 'set', 'key': 'doc/g11/stale-bound', 'value': 1}]})
        stale_hello = await refused(ordinary, 'hello', {'host_context': token})
        stale_cli = await cli(env, 'doc/g11/stale-cli')
        missing = [await control.call('workspace.get', {'key': key}) for key in ('doc/g11/stale-bound', 'doc/g11/stale-cli')]
        result = {'host': hosted['resource']['id'], 'registration': {key: registration.get(key) for key in ('host', 'actor', 'generation')},
            'private_token_present': isinstance(registration.get('host_context'), str) and bool(token),
            'token_distinct': token not in (bootstrap, registration['resume_token']), 'hello': bound_hello,
            'sdk_env': {key: env.get(key) for key in ('VQRO_SOCKET', 'VQRO_SOCKET_SOURCE', 'VQRO_PRINCIPAL')},
            'resources': inventory['resources'], 'cli': writes, 'history': history,
            'ordinary': ordinary_hello, 'wrong_token': wrong, 'raw_host': raw, 'no_grant': denied,
            'human_spawn': spawns[-1], 'public_secrets_absent': absent,
            'stale': {'bound': stale_bound, 'hello': stale_hello, 'cli_exit': stale_cli['exit'], 'entries': missing}}
    finally:
        for child in children:
            if child.returncode is None: child.kill(); await child.wait()
        for provider in reversed(providers): await provider.close()
        for peer in reversed(peers): await peer.close()
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--sdk', type=Path, required=True)
    parser.add_argument('--cli', type=Path, required=True)
    args = parser.parse_args()
    print(json.dumps(asyncio.run(run(args))))


if __name__ == '__main__': main()
