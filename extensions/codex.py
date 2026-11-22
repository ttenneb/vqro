#!/usr/bin/env python3
"""Codex launch/settings policy; the backend-owned Rust A2 bridge owns execution.

Authentication belongs to Codex: this extension never opens a native home, config
or credential file. Optional profile paths are passed literally on the terminal host.
"""
import json
from pathlib import Path
import sys
import time
import uuid

sys.path.insert(0, str(Path(__file__).resolve().parent))
from vqro_ext import Extension, VqroError
from vqro_display import label
from account_profiles import choose as choose_account, environment as account_environment, runtime_environment
from agent_launch import initialize_request_route, launch, launch_cwd, runtimes as validate_runtimes

SETTINGS = 'doc/settings/codex'
DEFAULTS = {'runtimes': [{'name': 'codex', 'argv': ['codex', 'app-server', '--stdio']}]}


def runtimes(value):
    rows = validate_runtimes(value, brand='Codex')
    for row in rows:
        for name in ('codex_home', 'home'):
            if name in row and (not isinstance(row[name], str) or not row[name].strip() or '\0' in row[name]):
                raise VqroError(name + ' must be a nonempty literal path on the terminal host', 'invalid')
    return rows


class Codex:
    def __init__(self, ext):
        self.ext = ext
        for name, fn, title in [('new', self.new, 'New Codex agent'), ('settings', self.settings, 'Codex runtime settings'),
                                ('configure', self.configure, 'Configure Codex runtime argv/profile')]:
            ext.command('codex.' + name, title)(self.guarded(fn))
        def initialize():
            if not ext.version(SETTINGS):
                ext.apply([{'op': 'set', 'key': SETTINGS, 'expect': 0, 'value': DEFAULTS}], 'codex: default runtimes')
        ext.retrying(initialize)
        ext.contribute({
            'view/accounts-choice-codex-new': {'kind':'panel','title':'Codex account','items':[]},
            'view/codex-runtimes': {'kind': 'panel', 'title': 'Codex runtimes', 'items': []},
            'view/codex-settings': {'kind': 'panel', 'title': 'Codex runtime settings', 'items': []},
        })

    def guarded(self, fn):
        def command(args):
            try: return fn(args)
            except VqroError as error:
                self.ext.notify(label(f'Codex: {error} ({error.kind}); no automatic retry.'),
                                'warn' if error.kind in ('timeout', 'indeterminate') else 'error')
                raise
        return command

    def new(self, args):
        entry = self.ext.vqro.call('workspace.get', {'key': SETTINGS})
        options = runtimes(entry['value'] if entry else DEFAULTS)
        selected = args.get('runtime') or args.get('value')
        cwd = launch_cwd(self.ext, args)
        if selected is None and len(options) > 1:
            self.ext.publish('codex-runtimes', {'title': 'New Codex agent: runtime', 'requires': ['menu'], 'items': [
                {'text': label(row['name']), 'description': label(' '.join(row['argv'])), 'command': 'codex.new',
                 'args': {'runtime': row['name'], **({'cwd': cwd} if cwd else {}),
                          **{k:args[k] for k in ('account','_account_revision') if k in args}}} for row in options]})
            return {'client': {'popup': {'view': 'codex-runtimes', 'width': 72}}}
        runtime = next((row for row in options if row['name'] == selected), None) if selected is not None else options[0]
        if runtime is None: raise VqroError('configured Codex runtime not found', 'invalid')
        account, popup = choose_account(self.ext, dict(args, runtime=runtime['name'], **({'cwd':cwd} if cwd else {})), 'codex', 'codex.new')
        if popup: return popup
        principal = 'p:' + uuid.uuid4().hex
        # Shared with native sign-in: profile selectors match without reading a profile.
        env = account_environment(account, runtime_environment('codex', runtime))
        env['VQRO_PRINCIPAL'] = principal  # never inherit or accept a configured owner/other agent
        params = {'bridge': True, 'name': 'codex-bridge-' + uuid.uuid4().hex[:12],
                  'argv': [*runtime.get('bridge_argv', ['vqro-bridge-codex']), '--slots', '1', '--endpoint-principal', principal,
                           '--', *runtime['argv']], 'env': env}
        if cwd: params['cwd'] = cwd
        session = {}
        def prepare(agent):
            route = initialize_request_route(self.ext, agent, brand='Codex')
            session.update(self.initialize_session(agent, cwd))
            return route
        result = launch(self.ext, params, brand='Codex', initialize_route=prepare, expected_agents=1)
        result['principal'] = principal
        if session: result['session'] = session
        return result

    def initialize_session(self, agent, cwd):
        # A2 intentionally starts with an empty slot. One explicit empty session
        # makes this daily launch usable; thread/start is not a user prompt.
        try:
            def facts():
                rows = self.ext.vqro.call('resource.list', {'id': agent}).get('resources', [])
                row = next((row for row in rows if row.get('id') == agent), {})
                return (row.get('data') or {}).get('agent') or {}
            native = facts()
            if isinstance(native.get('session'), dict) and native['session'].get('id'):
                return {'confirmed': True, 'created': False}
            caps = native.get('caps') or {}
            if native.get('v') != 0 or not isinstance(native.get('epoch'), str) or type(caps.get('rev')) is not int \
                    or 'new' not in caps.get('agent.session', {}).get('actions', []):
                raise VqroError('native new-session declaration unavailable', 'unavailable')
            args = {'v': 0, 'epoch': native['epoch'], 'caps_rev': caps['rev'], 'action': 'new'}
            if cwd: args['cwd'] = cwd
            result = self.ext.vqro.call('resource.invoke', {'resource': agent, 'op': 'agent.session', 'args': args, 'deadline_ms': 8000})
            if not isinstance(result, dict) or result.get('outcome') not in ('observed', 'accepted', 'dispatched'):
                raise VqroError('native session outcome unconfirmed', 'indeterminate')
            end = time.monotonic() + 3
            while True:
                observed = facts().get('session')
                if isinstance(observed, dict) and isinstance(observed.get('id'), str) and observed['id']:
                    return {'confirmed': True, 'created': True}
                if time.monotonic() >= end:
                    raise VqroError('native session observation unavailable', 'unavailable')
                time.sleep(.05)  # read-only publication observation, never effect replay
        except VqroError as error:
            self.ext.notify('Codex agent ' + agent + ' exists; native session setup unconfirmed (' + error.kind
                            + '). Do not replay this launch or session creation.', 'warn')
            return {'confirmed': False}

    def settings(self, args):
        self.ext.fresh()
        value = self.ext.value(SETTINGS, DEFAULTS)
        self.ext.publish('codex-settings', {'title': 'Codex runtime argv/profile', 'requires': ['input'], 'items': [
            {'type': 'text', 'text': 'Codex owns login/config. No vqro credential reads/copies or profile writes. No prompt is sent on launch.'},
            {'type': 'text', 'text': 'Optional codex_home and home are literal terminal-host paths; absent means native defaults. Never put credentials in settings.'},
            {'type': 'text', 'text': label(json.dumps(value.get('runtimes', value) if isinstance(value, dict) else value, ensure_ascii=False), 8192)},
            {'text': 'Edit Codex runtimes JSON list', 'input': 'JSON list of {name, argv, codex_home?, home?}', 'command': 'codex.configure'}]})
        return {'client': {'popup': {'view': 'codex-settings', 'width': 90}}}

    def configure(self, args):
        try:
            value = self.ext.value(SETTINGS, {})
            value = dict(value) if isinstance(value, dict) else {}
            value['runtimes'] = json.loads(args.get('text', ''))
        except (ValueError, TypeError):
            raise VqroError('enter a JSON list of {name, argv, codex_home?, home?}', 'invalid') from None
        runtimes(value)
        def save():
            self.ext.apply([{'op': 'set', 'key': SETTINGS, 'value': value, 'expect': self.ext.version(SETTINGS)}], 'codex: runtimes')
        self.ext.retrying(save)
        self.ext.notify('Codex runtimes saved; existing agents unchanged.', 'ok')
        return {'client': {'close': True}}


def main():
    ext = Extension('codex', event_types=['entries', 'resource'])
    Codex(ext)
    ext.run()


if __name__ == '__main__': main()
