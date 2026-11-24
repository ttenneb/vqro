#!/usr/bin/env python3
"""Claude headless route-1 launch policy; native TUI remains the separate claude extension.

No native profile/credential reads, login, inference prompt or input replay here. Execution
and native user settings remain in the backend-owned, explicit Claude executable.
"""
import json
from pathlib import Path
import sys
import uuid
sys.path.insert(0, str(Path(__file__).resolve().parent))
from vqro_ext import Extension, VqroError
from vqro_display import label
from account_credentials import choose as choose_account, environment as account_environment, runtime_environment, require_fresh
from agent_launch import initialize_request_route, launch, launch_cwd, runtimes as validate_runtimes
SETTINGS = 'doc/settings/claude-code'
DEFAULTS = {'runtimes': [{'name': 'claude', 'argv': ['claude']}]}


def runtimes(value):
    return validate_runtimes(value, brand='Claude Code')


def bridge_argv(runtime, principal):
    # Resolve execution-host paths, never helper argv supplied by a command caller.
    # Do not preflight at launch: unavailable helpers must refuse only restart.
    helper = [str(Path(sys.executable).resolve()),
              str(Path(__file__).resolve().with_name('accounts_keychain_helper.py'))]
    return [*runtime.get('bridge_argv', ['vqro-bridge-claude']), '--endpoint-principal', principal,
            '--restart-helper', json.dumps(helper, ensure_ascii=False, separators=(',', ':')),
            '--', *runtime['argv']]


class ClaudeCode:
    def __init__(self, ext):
        self.ext = ext
        for name, fn, title in [('new', self.new, 'New Claude Code agent (headless)'),
                                ('settings', self.settings, 'Claude Code headless runtime settings'),
                                ('configure', self.configure, 'Configure Claude Code executable/options')]:
            ext.command('claude-code.' + name, title)(self.guarded(fn))
        def initialize():
            if not ext.version(SETTINGS):
                ext.apply([{'op': 'set', 'key': SETTINGS, 'expect': 0, 'value': DEFAULTS}], 'claude-code: default runtime')
        ext.retrying(initialize)
        ext.contribute({
            'view/accounts-choice-claude-code-new': {'kind': 'panel', 'title': 'Claude account', 'items': []},
            'view/claude-code-runtimes': {'kind': 'panel', 'title': 'Claude Code headless runtimes', 'items': []},
            'view/claude-code-settings': {'kind': 'panel', 'title': 'Claude Code headless runtime settings', 'items': []},
        })

    def guarded(self, fn):
        def command(args):
            try:
                return fn(args)
            except VqroError as error:
                self.ext.notify('Claude Code operation unconfirmed; no automatic retry.',
                                'warn' if error.kind in ('timeout', 'indeterminate') else 'error')
                raise
        return command

    def new(self, args):
        entry = self.ext.vqro.call('workspace.get', {'key': SETTINGS})
        options = runtimes(entry['value'] if entry else DEFAULTS)
        selected = args.get('runtime') or args.get('value')
        cwd = launch_cwd(self.ext, args)
        if selected is None and len(options) > 1:
            self.ext.publish('claude-code-runtimes', {'title': 'New Claude Code agent: runtime', 'requires': ['menu'], 'items': [
                {'text': label(row['name']), 'command': 'claude-code.new',
                 'args': {'runtime': row['name'], **({'cwd': cwd} if cwd else {}),
                          **{k: args[k] for k in ('account', '_account_revision', 'fresh') if k in args}}} for row in options]})
            return {'client': {'popup': {'view': 'claude-code-runtimes', 'width': 72}}}
        runtime = next((row for row in options if row['name'] == selected), None) if selected is not None else options[0]
        if runtime is None:
            raise VqroError('configured Claude Code runtime not found', 'invalid')
        if args.get('fresh') is True:
            require_fresh('claude', runtime['argv'])
        account, popup = choose_account(self.ext, dict(args, runtime=runtime['name'], **({'cwd': cwd} if cwd else {})), 'claude', 'claude-code.new')
        if popup:
            return popup
        if account is not None:
            raise VqroError('API-key accounts: launching is not available yet; the keychain entry is kept'
                            if account['kind']=='api_key' else 'account choice is not available for this launcher yet','unsupported')
        principal = 'p:' + uuid.uuid4().hex
        env = account_environment(account, runtime_environment('claude', runtime))
        env['VQRO_PRINCIPAL'] = principal
        params = {'bridge': True, 'name': 'claude-code-bridge-' + uuid.uuid4().hex[:12],
                  'argv': bridge_argv(runtime, principal),
                  'env': env}
        if cwd:
            params['cwd'] = cwd
        result = launch(self.ext, params, brand='Claude',
                        initialize_route=lambda agent: initialize_request_route(self.ext, agent, brand='Claude'), expected_agents=1)
        result['principal'] = principal
        return result

    def settings(self, args):
        self.ext.fresh()
        value = self.ext.value(SETTINGS, DEFAULTS)
        text = json.dumps(value, ensure_ascii=False)
        detail = text if len(text) <= 8192 else 'Runtime details omitted: configuration exceeds panel bound.'
        self.ext.publish('claude-code-settings', {'title': 'Claude Code headless runtime', 'requires': ['input'], 'items': [
            {'type': 'text', 'text': 'Route 1 uses the vqro agent UI, not native-TUI parity. Native Claude owns login/settings. No prompt on launch.'},
            {'type': 'text', 'text': 'Native user settings remain enabled; no bare mode. Do not put credentials in workspace settings. Existing agents are unchanged.'},
            {'type': 'text', 'text': detail},
            {'text': 'Edit runtime executable/options JSON', 'input': 'JSON list of {name, argv, bridge_argv?, bridge_env?}', 'command': 'claude-code.configure'}]})
        return {'client': {'popup': {'view': 'claude-code-settings', 'width': 90}}}

    def configure(self, args):
        try:
            value = self.ext.value(SETTINGS, {})
            value = dict(value) if isinstance(value, dict) else {}
            value['runtimes'] = json.loads(args.get('text', ''))
        except (ValueError, TypeError):
            raise VqroError('enter a JSON runtime list', 'invalid') from None
        runtimes(value)
        def save():
            self.ext.apply([{'op': 'set', 'key': SETTINGS, 'value': value, 'expect': self.ext.version(SETTINGS)}], 'claude-code: runtimes')
        self.ext.retrying(save)
        self.ext.notify('Claude Code runtimes saved; existing agents unchanged.', 'ok')
        return {'client': {'close': True}}


def main():
    ext = Extension('claude-code', event_types=['entries', 'resource'])
    ClaudeCode(ext)
    ext.run()


if __name__ == '__main__':
    main()
