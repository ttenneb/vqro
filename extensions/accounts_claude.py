#!/usr/bin/env python3
"""Claude-only cutover callback. Policy/claims/continuation belong to accounts.

The callback never resolves a keychain handle, kills a terminal, starts another
supervisor or sends input. The route-1 adapter owns one guarded child restart.
"""
import copy
import re

import accounts_policy as policy
from account_credentials import snapshot as account_snapshot
from vqro_display import label
from vqro_ext import VqroError

OP = 'claude.restart'


def text(value, maximum=256):
    return isinstance(value, str) and 0 < len(value.encode('utf-8', errors='replace')) <= maximum and '\0' not in value


class ClaudeCutover:
    def __init__(self, ext):
        self.ext = ext
        ext.command('accounts.cutover.claude', 'Switch this Claude agent to another account')(self.restart)

    def source(self, resource, episode):
        reply = self.ext.vqro.call('resource.list', {'id': resource})
        rows = reply.get('resources') if isinstance(reply, dict) else None
        if not isinstance(rows, list) or len(rows) != 1:
            raise VqroError('Claude source is unavailable; no switch', 'stale')
        row = rows[0]
        if row.get('id') != resource or row.get('kind') != 'agent' or not policy.blocked(row, limit_only=True):
            raise VqroError('Claude source is no longer quota/rate limited; no switch', 'stale')
        facts = policy.facts(row)
        session = facts.get('session')
        account = policy.configured_id(row)
        caps = facts.get('caps')
        if (policy.episode(row) != episode or not isinstance(session, dict) or not text(session.get('id'))
                or not text(account, 64) or not isinstance(caps, dict) or not isinstance(caps.get(OP), dict)):
            raise VqroError('Claude session/source/restart declaration changed; no switch', 'stale')
        availability = facts.get('availability') or {}
        native = availability.get('native')
        if (not isinstance(native, dict) or native.get('subscription_limit') is not True
                or native.get('session_id') != session['id'] or not text(native.get('episode'))
                or native.get('terminal_reason') != 'blocking_limit'):
            raise VqroError('native ended subscription-limit evidence is missing; no switch', 'unsupported')
        provider = self.ext.providers.get(row.get('provider')) or {}
        declarations = provider.get('capabilities')
        host = provider.get('host')
        if (provider.get('role') != 'runtime' or not text(host, 128) or row.get('host') != host
                or not isinstance(declarations, dict) or not isinstance(declarations.get(OP), dict)):
            raise VqroError('Claude execution host is not authenticated for restart; no switch', 'unsupported')
        return copy.deepcopy(row), host

    def target(self, args, source_account, host):
        rows, revision = account_snapshot(self.ext)
        if type(args.get('_account_revision')) is not int or revision != args['_account_revision']:
            raise VqroError('account registry changed; no switch', 'stale')
        selected = [row for row in rows if row['id'] == args.get('account')]
        if len(selected) != 1:
            raise VqroError('Claude target account is missing or ambiguous; no switch', 'invalid')
        row = selected[0]
        if (not text(row.get('id'), 64) or row['id'] == source_account or row.get('runtime') != 'claude'
                or row.get('provider', 'anthropic') != 'anthropic' or row.get('kind') != 'subscription'
                or row.get('host') != host or not isinstance(row.get('secret_ref'), str)
                or re.fullmatch('[0-9a-f]{32}', row['secret_ref']) is None or not text(row.get('label'), 160)):
            raise VqroError('Claude target needs a same-host subscription keychain reference; no switch', 'unsupported')
        # Only the nonsecret fields understood by the native operation cross this boundary.
        return {key: row[key] for key in ('id', 'label', 'kind', 'secret_ref', 'host')} | {'provider': 'anthropic'}

    def claimed(self, args, host):
        # Common owns the journal format; this callback only reads its bound claim.
        claim = self.ext.invoke('accounts.claim.read', {'claim': args['_claim']})
        expected = {key: args[key] for key in ('resource', 'episode', 'account', '_account_revision', 'automatic')}
        expected.update(host=host, state='claimed')
        if not isinstance(claim, dict) or any(claim.get(key) != value for key, value in expected.items()):
            raise VqroError('matching protected account cutover claim is required; no switch', 'conflict')

    def restart(self, args):
        if (not isinstance(args, dict) or not text(args.get('resource'), 128)
                or not text(args.get('account'), 64) or not isinstance(args.get('episode'), str)
                or re.fullmatch('[0-9a-f]{64}', args['episode']) is None
                or type(args.get('automatic')) is not bool or not text(args.get('_claim'), 128)):
            raise VqroError('invalid Claude cutover request', 'invalid')
        self.ext.fresh()
        source, host = self.source(args['resource'], args['episode'])
        facts = policy.facts(source)
        expected = {'incarnation': source['incarnation'], 'epoch': facts['epoch'],
                    'session': facts['session']['id'], 'account': policy.configured_id(source),
                    'episode': facts['availability']['native']['episode']}
        target = self.target(args, expected['account'], host)
        self.claimed(args, host)
        current, current_host = self.source(args['resource'], args['episode'])
        current_facts = policy.facts(current)
        if (current_host != host or current['incarnation'] != expected['incarnation']
                or current_facts.get('session') != facts['session']
                or policy.configured_id(current) != expected['account']
                or current_facts['availability']['native']['episode'] != expected['episode']):
            raise VqroError('Claude source/session changed after claim; no switch', 'stale')
        if self.target(args, expected['account'], host) != target:
            raise VqroError('Claude target changed after claim; no switch', 'stale')
        self.claimed(args, host)
        # One attempt. Native admission owns the atomic epoch/session/account/child fence.
        # Keychain access occurs only in the execution-host helper, not this extension.
        native_args = {key: expected[key] for key in ('epoch', 'session', 'account', 'episode')}
        native_args.update(v=0, target={'account': target['id'], 'ref': target['secret_ref'],
                                      'host': host, 'revision': args['_account_revision']})
        result = self.ext.vqro.call('resource.invoke', {'resource': args['resource'], 'op': OP, 'args': native_args})
        data = result.get('data') if isinstance(result, dict) else None
        if (not isinstance(data, dict) or result.get('outcome') not in ('accepted', 'observed')
                or data.get('started') is not True or data.get('requested_session') != expected['session']
                or ('session' in data and data['session'] != expected['session'])
                or data.get('account') != target['id'] or not text(data.get('epoch'))
                or data['epoch'] == expected['epoch'] or type(data.get('resume_confirmed')) is not bool
                or (data['resume_confirmed'] and data.get('session') != expected['session'])):
            raise VqroError('Claude switch not confirmed; inspect native status; do not retry',
                            'indeterminate', data=result)
        confirmed = data['resume_confirmed']
        notice = 'resumed session ' + label(expected['session']) + ' on ' + label(target['label'])
        if not confirmed:
            notice += ' (not confirmed)'
        # Account is configured selection, not independent authenticated-account readback.
        # Common policy owns the only continuation send and visible continuation notice.
        member = {'resource': args['resource'], 'host': host, 'requested_session': expected['session'],
                  'account': target['id'], 'epoch': data['epoch'], 'process_started': True,
                  'session_confirmed': confirmed, 'resumed': confirmed}
        if 'session' in data:
            member['session'] = data['session']  # native observation only, never requested-session substitution
        return {**result, **member, 'affected': [member], 'notice': notice}

