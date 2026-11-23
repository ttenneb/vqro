#!/usr/bin/env python3
"""Claude-only cutover callback. Policy/claims/continuation belong to accounts.

The callback never resolves a keychain handle, kills a terminal, starts another
supervisor or sends input. The route-1 adapter owns one guarded child restart.
"""
import copy
import re

import accounts_policy as policy
from account_credentials import credential_identity, selection_problem, snapshot as account_snapshot
from vqro_display import label
from vqro_ext import VqroError

OP = 'claude.restart'


def text(value, maximum=256):
    return isinstance(value, str) and 0 < len(value.encode('utf-8', errors='replace')) <= maximum and '\0' not in value


def evidence_complete(facts, required):
    markers = facts.get('incomplete', [])
    if not isinstance(markers, list) or any(not isinstance(mark, str) or not mark or not mark.isprintable() for mark in markers):
        return False
    return not any(mark == path or mark.startswith((path + '.', path + '['))
                   or path.startswith((mark + '.', mark + '[')) for mark in markers for path in required)


def eligible_episode(row, automatic=True):
    """Pure scheduler predicate: return the current native limit UUID or None.

    Producer contract: the retained witness is invalidated on ANY newer native
    turn start/terminal result, allowed-rate observation, epoch or incarnation.
    Presence is not reconstructed from cached timers, blocked state or old input.
    Native queue remains unknown: use the producer's own epoch-bound input ledger;
    AUTO also requires its complete native empty background level. OFFER background
    loss is separately authorized by the protected, disclosed human claim.
    Native restart admission independently fences latest work and quiescence.
    """
    if type(automatic) is not bool or not isinstance(row, dict) or row.get('kind') != 'agent' or not policy.live(row):
        return None
    facts = policy.facts(row)
    required = ('state', '_claude.input_ledger', 'session.id', 'epoch', 'account.id', 'caps.' + OP,
                'availability.native.subscription_limit', 'availability.native.episode',
                'availability.native.session_id', 'availability.native.terminal_reason')
    if automatic:
        required += ('background',)
    if not evidence_complete(facts, required) or facts.get('state') != 'idle':
        return None
    private = facts.get('_claude')
    ledger = private.get('input_ledger') if isinstance(private, dict) else None
    if (not isinstance(ledger, dict) or type(ledger.get('v')) is not int or ledger['v'] != 0
            or ledger.get('source') != 'vqro-ledger' or ledger.get('epoch') != facts.get('epoch')
            or ledger.get('complete') is not True or type(ledger.get('pending')) is not int or ledger['pending'] != 0):
        return None
    if automatic and (not isinstance(facts.get('background'), list) or facts['background']):
        return None
    session = facts.get('session')
    caps = facts.get('caps')
    if (not isinstance(session, dict) or not text(session.get('id')) or not text(row.get('incarnation'))
            or not text(facts.get('epoch')) or not text(policy.configured_id(row), 64)
            or not isinstance(caps, dict) or not isinstance(caps.get(OP), dict)):
        return None
    availability = facts.get('availability')
    native = availability.get('native') if isinstance(availability, dict) else None
    if (not isinstance(native, dict) or native.get('subscription_limit') is not True
            or native.get('session_id') != session['id'] or not text(native.get('episode'))
            or native.get('terminal_reason') != 'blocking_limit'):
        return None
    return native['episode']


class ClaudeCutover:
    def __init__(self, ext):
        self.ext = ext
        ext.command('accounts.cutover.claude', 'Switch this Claude agent to another account')(self.restart)

    def source(self, resource, episode, automatic):
        reply = self.ext.vqro.call('resource.list', {'id': resource})
        rows = reply.get('resources') if isinstance(reply, dict) else None
        if not isinstance(rows, list) or len(rows) != 1:
            raise VqroError('Claude source is unavailable; no switch', 'stale')
        row = rows[0]
        if (not isinstance(row, dict) or row.get('id') != resource or eligible_episode(row, automatic=automatic) is None
                or policy.episode(row) != episode):
            raise VqroError('Claude needs a current native episode, idle and complete empty work witnesses; no switch', 'stale')
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
        if selection_problem(row):
            raise VqroError('Claude target credential route is incomplete or conflicting; no switch', 'unsupported')
        source_row = next((entry for entry in rows if entry['id'] == source_account), None)
        if source_row is not None and credential_identity(row) == credential_identity(source_row):
            raise VqroError('Claude target is a known alias of the source credential; no switch', 'invalid')
        if (not text(row.get('id'), 64) or row['id'] == source_account or row.get('runtime') != 'claude'
                or row.get('provider', 'anthropic') != 'anthropic' or row.get('kind') != 'subscription'
                or row.get('host') != host or not isinstance(row.get('secret_ref'), str)
                or re.fullmatch('[0-9a-f]{32}', row['secret_ref']) is None or not text(row.get('label'), 160)):
            raise VqroError('Claude target needs a same-host subscription keychain reference; no switch', 'unsupported')
        # Only the nonsecret fields understood by the native operation cross this boundary.
        return {key: row[key] for key in ('id', 'label', 'kind', 'secret_ref', 'host')} | {'provider': 'anthropic'}

    def claimed(self, args, host, source):
        # Common owns the journal/authority; never accept authority from callback args.
        claim = self.ext.invoke('accounts.claim.read', {'claim': args['_claim']})
        expected = {key: args[key] for key in ('resource', 'episode', 'account', '_account_revision', 'automatic')}
        expected.update(host=host, state='claimed', session=source['session'],
                        source_incarnation=source['incarnation'], source_epoch=source['epoch'],
                        source_account=source['account'], native_episode=source['episode'])
        if (not isinstance(claim, dict) or type(claim.get('automatic')) is not bool
                or any(claim.get(key) != value for key, value in expected.items())):
            raise VqroError('matching protected account cutover claim is required; no switch', 'conflict')
        authority = claim.get('authorization')
        if args['automatic']:
            if authority != {'mode': 'auto', 'principal': None, 'owner_revision': None}:
                raise VqroError('protected automatic cutover authority is missing; no switch', 'forbidden')
        else:
            if (not isinstance(authority, dict) or set(authority) != {'mode', 'principal', 'owner_revision', 'background_loss_disclosed'}
                    or authority.get('mode') != 'offer' or authority.get('background_loss_disclosed') is not True
                    or not text(authority.get('principal'))
                    or type(authority.get('owner_revision')) is not int or authority['owner_revision'] <= 0):
                raise VqroError('protected human offer authority is missing; no switch', 'forbidden')
            owner = self.ext.vqro.call('workspace.get', {'key': 'doc/principals/owner'})
            if (not isinstance(owner, dict) or owner.get('value') != authority['principal']
                    or owner.get('version') != authority['owner_revision']):
                raise VqroError('workspace owner changed after the human choice; no switch', 'stale')

    def restart(self, args):
        if (not isinstance(args, dict) or not text(args.get('resource'), 128)
                or not text(args.get('account'), 64) or not isinstance(args.get('episode'), str)
                or re.fullmatch('[0-9a-f]{64}', args['episode']) is None
                or type(args.get('automatic')) is not bool or not text(args.get('_claim'), 128)):
            raise VqroError('invalid Claude cutover request', 'invalid')
        self.ext.fresh()
        source, host = self.source(args['resource'], args['episode'], args['automatic'])
        facts = policy.facts(source)
        expected = {'incarnation': source['incarnation'], 'epoch': facts['epoch'],
                    'session': facts['session']['id'], 'account': policy.configured_id(source),
                    'episode': facts['availability']['native']['episode']}
        target = self.target(args, expected['account'], host)
        self.claimed(args, host, expected)
        current, current_host = self.source(args['resource'], args['episode'], args['automatic'])
        current_facts = policy.facts(current)
        if (current_host != host or current['incarnation'] != expected['incarnation']
                or current_facts.get('session') != facts['session']
                or policy.configured_id(current) != expected['account']
                or current_facts['availability']['native']['episode'] != expected['episode']):
            raise VqroError('Claude source/session changed after claim; no switch', 'stale')
        if self.target(args, expected['account'], host) != target:
            raise VqroError('Claude target changed after claim; no switch', 'stale')
        self.claimed(args, host, expected)
        # One attempt. Native admission owns the atomic epoch/session/account/child fence.
        # Keychain access occurs only in the execution-host helper, not this extension.
        native_args = {key: expected[key] for key in ('epoch', 'session', 'account', 'episode')}
        native_args.update(v=0, allow_background_loss=not args['automatic'],
                           target={'account': target['id'], 'ref': target['secret_ref'],
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

