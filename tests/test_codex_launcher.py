"""Codex extension policy doubles; actual bridge/backend evidence is a separate suite."""
import copy
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'extensions'))
from codex import Codex, DEFAULTS, SETTINGS, runtimes
from vqro_ext import VqroError
from agent_fixture import FakeCore


class Core(FakeCore):
    def __init__(self): super().__init__('codex')
    def add_agent(self):
        super().add_agent()
        self.resources['r2']['data']['agent'] = {'v': 0, 'epoch': 'fixture-e1', 'session': None,
            'caps': {'rev': 1, 'agent.session': {'actions': ['new']}}}
    def call(self, method, args):
        if method == 'resource.invoke' and args.get('op') == 'agent.session':
            self.calls.append((method, copy.deepcopy(args)))
            self.resources[args['resource']]['data']['agent']['session'] = {'id': 'fixture-session'}
            return {'outcome': 'observed'}
        return super().call(method, args)
    def invoke(self, name, args=None):
        if name == 'coord.request_route':
            self.calls.append((name, copy.deepcopy(args)))
            key = 'doc/agents/request-routes/' + args['agent']
            if args.get('action') == 'initialize':
                caller = self.caller_record.get('principal')
                matched = [row for row in self.resources.values() if row.get('kind') == 'agent'
                           and (row.get('data') or {}).get('agent', {}).get('principal') == caller]
                route = 'coordinator' if len(matched) == 1 else 'human'
                self.apply([{'op': 'set', 'key': key, 'expect': args['expect'], 'value': {'route': route}}], 'route mock')
                return {'state': 'accepted'}
            return {'configured': self.value(key, {}).get('route'), 'route': self.value(key, {}).get('route')}
        return super().invoke(name, args)


class CodexLauncherTests(unittest.TestCase):
    def setUp(self): self.core = Core(); self.launcher = Codex(self.core)
    def new(self, **args): return self.core.handlers['codex.new'](args)
    def spawns(self): return [args for method, args in self.core.calls if method == 'resource.spawn']
    def configure(self, row): self.core.apply([{'op': 'set', 'key': SETTINGS, 'value': {'runtimes': [row]}}], 'settings mock')

    def test_default_native_owns_auth_and_no_prompt_is_sent(self):
        self.assertEqual(self.core.value(SETTINGS), DEFAULTS)
        self.assertEqual(self.spawns(), [])
        with patch('pathlib.Path.open', side_effect=AssertionError('no native profile IO')):
            result = self.new(cwd='/project')
        spawn, = self.spawns()
        self.assertEqual(spawn['argv'], ['vqro-bridge-codex', '--slots', '1', '--endpoint-principal', result['principal'],
                                         '--', 'codex', 'app-server', '--stdio'])
        self.assertEqual(set(spawn['env']), {'VQRO_PRINCIPAL'})
        self.assertRegex(result['principal'], r'^p:[0-9a-f]{32}$')
        self.assertEqual(spawn['env']['VQRO_PRINCIPAL'], result['principal'])
        self.assertTrue(spawn['bridge']); self.assertEqual(spawn['cwd'], '/project')
        self.assertEqual(result['request_route'], {'configured': 'human', 'route': 'human', 'confirmed': True})
        effects = [args for method, args in self.core.calls if method == 'resource.invoke']
        self.assertEqual(len(effects), 1)
        self.assertEqual((effects[0]['op'], effects[0]['args']['action']), ('agent.session', 'new'))
        self.assertEqual(result['session'], {'confirmed': True, 'created': True})
        self.assertEqual(self.core.calls[-1], ('agents.show', {'resource': 'r2'}))

    def test_fresh_principal_cannot_be_overridden_by_configured_owner(self):
        self.configure({'name': 'configured', 'argv': ['/literal codex', 'a;$(no-shell)'],
                        'bridge_env': {'VQRO_PRINCIPAL': 'owner'}})
        first = self.new(); second = self.new()
        self.assertNotEqual(first['principal'], second['principal'])
        self.assertTrue(all(spawn['env']['VQRO_PRINCIPAL'].startswith('p:') for spawn in self.spawns()))
        self.assertNotEqual(self.spawns()[0]['name'], self.spawns()[1]['name'])
        self.assertEqual(self.spawns()[0]['argv'][-2:], ['/literal codex', 'a;$(no-shell)'])

    def test_explicit_dedicated_profile_is_only_literal_environment_not_file_io(self):
        self.configure({'name': 'dedicated', 'argv': ['codex', 'app-server'], 'home': '/test/profile/home',
                        'codex_home': '/test/profile/config', 'bridge_argv': ['/host/bridge']})
        with patch('pathlib.Path.open', side_effect=AssertionError('no credential/config IO')):
            self.new()
        spawn, = self.spawns()
        self.assertEqual(spawn['env']['HOME'], '/test/profile/home')
        self.assertEqual(spawn['env']['CODEX_HOME'], '/test/profile/config')
        self.assertEqual(spawn['argv'][0], '/host/bridge')
        self.assertEqual(self.core.value(SETTINGS)['runtimes'][0]['codex_home'], '/test/profile/config')

    def test_route_initialize_uses_sdk_caller_not_a_claimed_role(self):
        self.core.caller_record = {'principal': 'coordinator:one', 'actor': 'coordinator:one'}
        self.core.resources['r77'] = {'id': 'r77', 'kind': 'agent', 'host': 'other',
            'data': {'agent': {'principal': 'coordinator:one'}}}
        result = self.new(role='human')
        writes = [args for name, args in self.core.calls if name == 'coord.request_route' and args.get('action') == 'initialize']
        self.assertEqual(len(writes), 1); self.assertNotIn('route', writes[0])
        self.assertEqual(writes[0]['expect'], 0)
        self.assertEqual(result['request_route']['configured'], 'coordinator')
        self.assertFalse(any(name == 'coord.request_route' and args.get('action') == 'set' for name, args in self.core.calls))

    def test_profile_and_runtime_validation_precedes_launch(self):
        for row in ({'name': 'x', 'argv': []}, {'name': 'x', 'argv': ['codex'], 'home': ''},
                    {'name': 'x', 'argv': ['codex'], 'codex_home': None},
                    {'name': 'x', 'argv': ['codex'], 'bridge_env': {'HOME': None}}):
            with self.subTest(row=row), self.assertRaises(VqroError): runtimes({'runtimes': [row]})
        self.assertEqual(self.spawns(), [])

    def test_picker_waits_for_selection_without_any_native_effect(self):
        self.core.apply([{'op': 'set', 'key': SETTINGS, 'value': {'runtimes': [
            {'name': 'one', 'argv': ['one']}, {'name': 'two', 'argv': ['two']}]}}], 'settings')
        self.assertEqual(self.new()['client']['popup']['view'], 'codex-runtimes')
        self.assertEqual(self.spawns(), [])
        self.new(runtime='two'); self.assertEqual(self.spawns()[0]['argv'][-1], 'two')

    def test_scoped_source_cwd_lookup(self):
        self.core.resources['r88'] = {'id': 'r88', 'cwd': '/source'}
        with patch.object(self.core, 'fresh', side_effect=AssertionError('no global launch refresh')):
            self.new(context={'resource': 'r88'})
        self.assertEqual(self.spawns()[0]['cwd'], '/source')
        self.assertIn(('resource.list', {'id': 'r88'}), self.core.calls)

    def test_observed_not_ready_does_not_repeat_spawn(self):
        self.core.make_agent = False
        with patch('agent_launch.time.monotonic', side_effect=[0, 13]): result = self.new()
        self.assertEqual((result['outcome'], result['host'], result['ready']), ('observed', 'r1', False))
        self.assertEqual(len(self.spawns()), 1)
        self.assertTrue(any('Do not blindly retry' in text for text, _ in self.core.notices))

    def test_unrelated_or_ambiguous_agent_is_not_chosen(self):
        self.core.make_agent = False
        self.core.resources['r9'] = {'id': 'r9', 'kind': 'agent', 'available': True, 'host': 'unrelated'}
        with patch('agent_launch.time.monotonic', side_effect=[0, 13]): self.assertFalse(self.new()['ready'])
        self.core.make_agent = True; self.core.resources['r9']['host'] = 'r1'
        self.assertFalse(self.new()['ready']); self.assertEqual(len(self.spawns()), 2)

    def test_read_retry_does_not_repeat_native_spawn(self):
        original = self.core.call; count = []
        def call(method, args):
            if method == 'resource.list' and 'host' in args:
                count.append(1)
                if len(count) == 1: raise VqroError('busy', 'overloaded')
            return original(method, args)
        with patch.object(self.core, 'call', side_effect=call), patch('agent_launch.time.sleep'): self.new()
        self.assertEqual(len(count), 2); self.assertEqual(len(self.spawns()), 1)

    def test_unknown_spawn_is_attempted_only_once(self):
        original = self.core.call
        def call(method, args):
            if method == 'resource.spawn':
                self.core.calls.append((method, copy.deepcopy(args)))
                raise VqroError('native outcome unknown', 'indeterminate')
            return original(method, args)
        with patch.object(self.core, 'call', side_effect=call), self.assertRaises(VqroError): self.new()
        self.assertEqual(len(self.spawns()), 1)
        self.assertTrue(any('no automatic retry' in text for text, _ in self.core.notices))

    def test_optional_ui_failures_preserve_observed_host_and_agent(self):
        self.core.fail_agents = self.core.fail_tabs = True
        result = self.new()
        self.assertEqual((result['host'], result['agent']), ('r1', 'r2'))
        self.assertEqual(len(self.spawns()), 1)
        self.assertTrue(any('resource picker' in text for text, _ in self.core.notices))

    def test_route_failure_never_replays_or_claims_confirmation(self):
        original = self.core.invoke
        def invoke(name, args=None):
            if name == 'coord.request_route': raise VqroError('denied', 'forbidden')
            return original(name, args)
        with patch.object(self.core, 'invoke', side_effect=invoke): result = self.new()
        self.assertEqual(result['request_route'], {'confirmed': False})
        self.assertEqual((result['outcome'], result['agent']), ('observed', 'r2'))
        self.assertEqual(len(self.spawns()), 1)

    def test_malformed_route_readback_is_not_confirmed(self):
        original = self.core.invoke
        def invoke(name, args=None):
            if name == 'coord.request_route' and args.get('action') == 'inspect':
                return {'route': 'not-a-route', 'configured': None}
            return original(name, args)
        with patch.object(self.core, 'invoke', side_effect=invoke): self.assertFalse(self.new()['request_route']['confirmed'])

    def test_unknown_session_outcome_is_never_replayed(self):
        original = self.core.call
        def call(method, args):
            if method == 'resource.invoke':
                self.core.calls.append((method, copy.deepcopy(args)))
                raise VqroError('native session outcome unknown', 'indeterminate')
            return original(method, args)
        with patch.object(self.core, 'call', side_effect=call): result = self.new()
        self.assertEqual(result['session'], {'confirmed': False})
        self.assertEqual(len(self.spawns()), 1)
        self.assertEqual(len([args for method, args in self.core.calls if method == 'resource.invoke']), 1)
        self.assertEqual(result['agent'], 'r2')

    def test_configuration_updates_only_own_settings(self):
        self.core.apply([{'op': 'set', 'key': 'doc/settings/pi', 'value': {'preserve': True}}], 'other')
        before = self.core.value(SETTINGS)
        with self.assertRaises(VqroError): self.core.handlers['codex.configure']({'text': 'not JSON'})
        self.assertEqual(self.core.value(SETTINGS), before)
        self.core.handlers['codex.configure']({'text': '[{"name":"dedicated","argv":["codex"],"codex_home":"/profile"}]'})
        self.assertEqual(self.core.value('doc/settings/pi'), {'preserve': True})
        self.assertEqual(self.spawns(), [])


if __name__ == '__main__': unittest.main()
