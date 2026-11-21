# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""G11 shared-host branch: public two-endpoint SDK/CLI evidence, no endpoint inference."""
import asyncio
import hashlib
import json
import re
from pathlib import Path

from pi_provider_e2e import identity
from ..results import FixtureFailed, HarnessGap


def pin(value):
    if not isinstance(value, dict) or not isinstance(value.get('path'), str) or not Path(value['path']).is_absolute():
        raise FixtureFailed('G11 requires an explicit absolute immutable artifact')
    path = Path(value['path'])
    if not path.is_file() or hashlib.sha256(path.read_bytes()).hexdigest() != value.get('sha256'):
        raise FixtureFailed('G11 artifact hash mismatch')
    return path


def grade_host_route(ctx, value):
    """Only observed core-assigned host/actual CLI authors satisfy the shared-host row."""
    ctx.check(isinstance(value, dict), 'G11 actual public observation is an object')
    host = value.get('host')
    ctx.check(isinstance(host, str) and bool(re.fullmatch(r'r[1-9][0-9]*', host)), 'host is a canonical core resource, not a caller assertion')
    actor = 'host:' + host
    registration = value.get('registration', {})
    ctx.check(registration.get('host') == host and registration.get('actor') == actor and
              type(registration.get('generation')) is int and registration['generation'] > 0,
              'actual runtime registration derives the hosted actor')
    ctx.check(value.get('private_token_present') is True and value.get('token_distinct') is True,
              'opt-in private reply credential exists and is not bootstrap/resume')
    hello = value.get('hello', {})
    ctx.check(hello.get('actor') == actor, 'actual verified host_context hello binds that host')
    env = value.get('sdk_env', {})
    ctx.check(env.get('VQRO_SOCKET') == ctx.backend.env['VQRO_SOCKET'] and env.get('VQRO_SOCKET_SOURCE') == '' and
              env.get('VQRO_PRINCIPAL') == 'undeclared', 'actual SDK tool map selects private socket and discards inherited owner')
    rows = value.get('resources', [])
    ctx.check(isinstance(rows, list) and len(rows) == 2 and {row.get('backend_ref') for row in rows} == {'endpoint-a', 'endpoint-b'} and
              len({row.get('id') for row in rows}) == 2, 'two distinct actual runtime endpoints are observed')
    for row in rows:
        ctx.check(row.get('host') == host and row.get('kind') == 'agent' and row.get('available') is True and
                  row.get('stale') is False and row.get('state') == 'live' and row.get('principal') is None,
                  'both live available endpoints share actual host without an endpoint principal')
        caps = (row.get('data') or {}).get('agent', {}).get('caps', {}).get('agent', {})
        ctx.check(caps.get('tool_principal') != 'env', 'shared host never declares endpoint tool_principal=env')
        ctx.evidence.append({'subcase': 'endpoint-env-tool', 'status': 'not-applicable',
            'declaration': {'path': 'resources.' + row['id'] + '.data.agent.caps.agent.tool_principal', 'actual': caps.get('tool_principal')}})
    writes = value.get('cli', [])
    ctx.check(isinstance(writes, list) and len(writes) == 2 and {item.get('key') for item in writes} ==
              {'doc/g11/endpoint-a', 'doc/g11/endpoint-b'}, 'each endpoint launches a distinct real CLI tool write')
    history = value.get('history', {}).get('txns', [])
    authors = []
    for item in writes:
        ctx.check(type(item.get('exit')) is int and item['exit'] == 0 and isinstance(item.get('reply'), dict) and
                  type(item['reply'].get('txn')) is int and item['reply']['txn'] > 0, 'real tool CLI completed with an actual transaction receipt')
        matches = [row for row in history if type(row.get('id')) is int and row['id'] == item['reply']['txn']]
        ctx.check(len(matches) == 1, 'CLI receipt identifies exactly one actual history transaction')
        txn = matches[0]
        ctx.check(txn.get('actor') == actor and txn.get('principal') == actor,
                  'CLI transaction actor and effective principal are exactly shared host, never an endpoint/owner')
        ctx.check([change.get('key') for change in txn.get('changes', [])] == [item['key']],
                  'actual attributed history entry belongs to this exact CLI tool write')
        authors.append(txn.get('author'))
    ctx.check(all(isinstance(author, str) and author for author in authors) and len(set(authors)) == 2,
              'two actual CLI connection authors stay distinct under their common host principal')
    ctx.check(value.get('ordinary', {}).get('actor') == 'undeclared', 'ordinary SDK hello never implicitly inherits tool context')
    for field in ('wrong_token', 'raw_host', 'no_grant'):
        ctx.check(isinstance(value.get(field), str) and value[field] in {'invalid', 'forbidden', 'no_grant', 'stale_generation'},
                  'wrong token/raw assertion/delegation cannot acquire host or endpoint authority', refused=field)
    denied_entry = value.get('no_grant_entry')
    ctx.check('no_grant_entry' in value and denied_entry is None,
              'denied endpoint delegation created no workspace entry')
    ctx.check(value.get('human_spawn') == {'context_present': False, 'principal': 'undeclared'},
              'ordinary native shell spawn does not receive host credential or inherited owner')
    ctx.check(value.get('public_secrets_absent') is True, 'private registration credentials are absent from public snapshot/history')
    stale = value.get('stale', {})
    ctx.check(stale.get('bound') == 'stale_generation' and stale.get('hello') in {'invalid', 'forbidden', 'stale_generation'} and
              type(stale.get('cli_exit')) is int and stale['cli_exit'] != 0,
              'retired provider fences bound tool, fresh hello and actual stale CLI without downgrade/replay')
    entries = stale.get('entries', [])
    ctx.check(isinstance(entries, list) and len(entries) == 2 and all(row is None for row in entries),
              'retired tool calls created no workspace entry')


async def shared_host_route(ctx):
    config = ctx.manifest.get('host_route')
    if not isinstance(config, dict): raise HarnessGap('G11 shared-host public SDK/CLI route requires its pinned native kit')
    revision = config.get('revision')
    if not isinstance(revision, str) or not re.fullmatch(r'[0-9a-f]{40}', revision) or revision != ctx.manifest.get('coord_revision'):
        raise FixtureFailed('G11 exact full target/binding revision mismatch')
    script, test, cli = (pin(config.get(key)) for key in ('archive_script', 'archive_test', 'cli'))
    if test != script.parent / 'g11-host-tool-cli-test' or cli != script.parent / 'vqro':
        raise FixtureFailed('G11 archive descriptor must name the exact script-used test and CLI')
    if hashlib.sha256(ctx.vqro.read_bytes()).hexdigest() != config['cli']['sha256']:
        raise FixtureFailed('G11 running core/CLI differs from accepted kit')
    probe = Path(__file__).with_name('host_tool_probe.py')
    ctx.metadata['g11'] = {'profile': 'shared-host-native-fixture', 'endpoint_env_conformance': False,
        'native_adapter_conformance': False, 'revision': revision,
        'probe_sha256': hashlib.sha256(probe.read_bytes()).hexdigest(),
        'archive_script_sha256': config['archive_script']['sha256'], 'archive_test_sha256': config['archive_test']['sha256']}
    async def execute(argv, name, env):
        process = await asyncio.create_subprocess_exec(*argv, env=env, cwd=ctx.root / 'work',
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
        ctx.backend.tracked.append(identity(process.pid))
        try: out, err = await asyncio.wait_for(process.communicate(), 30)
        finally:
            if process.returncode is None: process.kill(); await process.wait()
        (ctx.root / (name + '.stdout')).write_bytes(out)
        (ctx.root / (name + '.stderr')).write_bytes(err)
        ctx.check(process.returncode == 0, 'owned native G11 witness completed', witness=name, exit=process.returncode)
        return out
    # Positive K1 evidence is structured public RPC/SDK/CLI output, not test-exit inference.
    output = await execute(['/usr/bin/python3', str(probe), '--sdk', str(ctx.root / 'ext'), '--cli', str(cli)],
                           'g11-public', dict(ctx.backend.env))
    observed = json.loads(output)
    grade_host_route(ctx, observed)
    ctx.evidence.append({'kind': 'actual_public_host_tool_route', 'observed': observed})
    # Accepted Rust native fixture additionally covers generation rotation, queued-before-
    # writer rejection and truthful already-admitted writer adoption. Never edit its seams.
    archive_env = {**ctx.backend.env, 'PATH': '/usr/bin:/bin'}
    native = await execute(['/bin/bash', str(script)], 'g11-archived-native', archive_env)
    expected = b'two_endpoint_tools_use_public_sdk_environment_and_real_cli_and_retire_truthfully ... ok'
    ctx.check(expected in native and b'1 passed; 0 failed' in native,
              'exact archived native test executed, not zero filtered tests or another test')
