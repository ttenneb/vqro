"""Accounts select credentials over one shared setup; fake paths/core only."""
import json
from pathlib import Path
import sys
import unittest
from unittest.mock import patch
sys.path[:0] = [str(Path(__file__).resolve().parents[1] / 'extensions'), str(Path(__file__).resolve().parent)]
from agent_fixture import FakeCore
from vqro_ext import VqroError
import account_credentials as profiles
from accounts import Accounts
from pi import Pi
from codex import Codex

class AccountTests(unittest.TestCase):
    def setUp(self):
        self.core = FakeCore('accounts')
        self.accounts = Accounts(self.core)

    def add(self, runtime='pi', **extra):
        row = {'id': 'work', 'label': 'Work', 'runtime': runtime, 'provider': 'anthropic' if runtime == 'claude' else 'openai-codex',
               'kind': 'subscription', **({'native_provider': 'vqro-account-work'} if runtime == 'pi' else
               {'login_ref':{'sessionId':'saved-work','accountId':'native-work'}} if runtime=='codex' else
               {'host':'r1','secret_ref':'a'*32}), **extra}
        self.core.handlers['accounts.add'](row)
        return row

    def spawns(self):
        return [p for m,p in self.core.calls if m == 'resource.spawn']

    def test_registry_is_metadata_only_and_never_reads_profiles(self):
        with patch.object(Path, 'open', side_effect=AssertionError('profile content must never be opened')):
            self.add()
            rows = profiles.registry(self.core)
        self.assertEqual(rows[0]['native_provider'], 'vqro-account-work')
        self.assertEqual(self.spawns(), [])
        for field in ('token', 'api_key', 'auth', 'env'):
            with self.subTest(field=field), self.assertRaises(VqroError):
                self.add(id='bad', **{field: 'never-store-this'})
        self.assertEqual(len(profiles.registry(self.core)), 1)

    def test_each_program_has_normal_option_without_guessed_billing_kind(self):
        for runtime in ('pi','codex','claude'):
            account, popup = profiles.choose(self.core, {'account':'normal'}, runtime, runtime+'.new')
            self.assertIsNone(account)
            self.assertIsNone(popup)
        self.assertEqual(self.spawns(), [])

    def test_pi_choice_before_spawn_and_named_profile_mapping(self):
        self.add()
        pi = Pi(self.core)
        answer = pi.new({'cwd':'/fake project'})
        self.assertIn('popup', answer['client'])
        self.assertEqual(self.spawns(), [])
        answer = pi.new({'account':'work','cwd':'/fake project'})
        spawn = self.spawns()[0]
        self.assertNotIn('PI_CODING_AGENT_DIR', spawn['env'])
        self.assertEqual(json.loads(spawn['env']['VQRO_PI_ACCOUNT'])['native_provider'], 'vqro-account-work')
        fact = json.loads(spawn['env']['VQRO_ACCOUNT'])
        self.assertEqual(fact['id'], 'work')
        self.assertNotIn('profile', fact)
        self.assertEqual(fact['sources']['id'], 'vqro:launcher')
        self.assertIn('--provider', spawn['argv'])
        self.assertEqual(answer['agent'], 'r2')

    def test_codex_choice_maps_codex_home_no_credential_lookup(self):
        self.add('codex')
        codex = Codex(self.core)
        original=self.core.call
        def native_binding(method,params=None):
            if method=='resource.invoke' and params.get('op')=='agent.command':
                name=params['args']['name']
                if name=='_account.list':return {'outcome':'observed','data':{'logins':[{'sessionId':'saved-work','accountId':'native-work','active':True}]}}
                if name=='_account.bind':return {'outcome':'observed','data':{'account_confirmed':True}}
            return original(method,params)
        with patch.object(Path, 'open', side_effect=AssertionError('no profile reads')),patch.object(self.core,'call',side_effect=native_binding):
            codex.new({'account':'work'})
        self.assertNotIn('CODEX_HOME', self.spawns()[0]['env'])
        self.assertEqual(len(self.spawns()), 1)

    def test_bad_or_cross_program_account_never_spawns(self):
        self.add('claude')
        pi = Pi(self.core)
        for account in ('missing', 'work'):
            with self.subTest(account=account), self.assertRaises(VqroError):
                pi.new({'account':account})
        self.assertEqual(self.spawns(), [])

    def test_normal_login_keeps_existing_runtime_behavior(self):
        self.add()
        Pi(self.core).new({'account':'normal'})
        env = self.spawns()[0]['env']
        self.assertNotIn('PI_CODING_AGENT_DIR', env)
        self.assertNotIn('VQRO_ACCOUNT', env)

    def test_native_sign_in_literal_argv_one_terminal_no_typing(self):
        expected = {'pi': ('PI_CODING_AGENT_DIR', ['pi1']), 'codex': ('CODEX_HOME', ['codex','login']),
                    'claude': ('CLAUDE_CONFIG_DIR', ['claude','auth','login'])}
        for runtime,(selector,prefix) in expected.items():
            core = FakeCore('accounts'); Accounts(core)
            core.handlers['accounts.add']({'id':'work','label':'Work','runtime':runtime,'kind':'subscription',
                                           **({'provider':'openai-codex','native_provider':'vqro-account-work'} if runtime=='pi' else
                                              {'login_ref':{'sessionId':'saved-work','accountId':'native-work'}} if runtime=='codex' else
                                              {'host':'r1','secret_ref':'a'*32})})
            core.handlers['accounts.sign_in']({'account':'work'})
            spawns = [p for m,p in core.calls if m=='resource.spawn']
            self.assertEqual(len(spawns),1)
            self.assertEqual(spawns[0]['argv'][:len(prefix)], prefix)
            self.assertNotIn(selector, spawns[0]['env'])
            self.assertNotIn('VQRO_ACCOUNT', spawns[0]['env'])
            self.assertNotIn('VQRO_PI_ACCOUNT', spawns[0]['env'])
            if runtime=='pi':
                argv = spawns[0]['argv']
                self.assertIn('--vqro-account-login', argv)
                self.assertEqual(json.loads(argv[argv.index('--vqro-account') + 1])['native_provider'], 'vqro-account-work')
            self.assertFalse(spawns[0].get('bridge',False))
            self.assertFalse(any(m in ('resource.invoke','terminal.input') for m,p in core.calls))

    def test_tier1_uses_same_account_and_keeps_configured_fact_in_declaration(self):
        import test_tier1 as fixture
        f=fixture.Tier1Tests(); f.setUp()
        try:
            # The same fact invariant applies to the supported native-bound Pi
            # route; named Claude is separately tested as an explicit refusal.
            f.configure(pi=['pi1'])
            Accounts(f.core).add({'id':'work','label':'Work','runtime':'pi','kind':'subscription',
                                  'provider':'anthropic','native_provider':'vqro_work'})
            self.assertIn('popup', f.launch('pi')['client'])
            self.assertEqual(f.spawns(), [])
            f.launch('pi',account='work')
            self.assertNotIn('PI_CODING_AGENT_DIR', f.spawns()[0]['env'])
            self.assertEqual(f.core.value('doc/agents/terminals/r1')['account']['id'],'work')
        finally: f.tearDown()

    def test_stale_choice_and_conflicting_provider_never_spawn(self):
        self.add()
        pi=Pi(self.core)
        with self.assertRaises(VqroError): pi.new({'account':'work','_account_revision':0})
        self.core.apply([{'op':'set','key':'doc/settings/pi','value':{'runtimes':[{'name':'wrong','argv':['pi1','--provider','anthropic']}]}}], 'fake')
        with self.assertRaises(VqroError): pi.new({'account':'work'})
        self.assertEqual(self.spawns(), [])

    def test_native_normal_sign_in_matches_explicit_runtime_profile_settings(self):
        self.core.apply([{'op':'set','key':'doc/settings/codex','value':{'runtimes':[{'name':'custom','argv':['/fake/codex','app-server','--stdio'],
                         'codex_home':'/fake/normal-codex','home':'/fake/home'}]}}], 'fake')
        self.accounts.sign_in({'account':'normal','runtime':'codex'})
        spawn=self.spawns()[0]
        self.assertEqual(spawn['argv'],['/fake/codex','login'])
        self.assertEqual(spawn['env'],{'CODEX_HOME':'/fake/normal-codex','HOME':'/fake/home'})

    def test_sign_in_uncertainty_is_not_replayed(self):
        self.add('codex')
        original=self.core.call
        attempts=[]
        def call(method,params):
            if method=='resource.spawn':
                attempts.append(params)
                raise VqroError('unknown outcome','indeterminate')
            return original(method,params)
        with patch.object(self.core,'call',side_effect=call),self.assertRaises(VqroError):
            self.accounts.sign_in({'account':'work'})
        self.assertEqual(len(attempts),1)

    def test_removal_drops_metadata_only_not_native_profile_or_running_agents(self):
        self.add()
        revision=self.core.version(profiles.SETTINGS)
        with patch.object(Path,'unlink',side_effect=AssertionError('never delete a profile')):
            self.accounts.remove({'account':'work','expect':revision})
        self.assertEqual(profiles.registry(self.core),[])
        self.assertEqual(self.spawns(),[])
        self.assertFalse(any(m=='resource.invoke' for m,p in self.core.calls))

    def test_unknown_extra_fields_and_invalid_api_key_sign_in_fail_closed(self):
        self.add('codex',kind='api_key')
        with self.assertRaises(VqroError): self.accounts.sign_in({'account':'work'})
        self.assertEqual(self.spawns(),[])

    def test_normal_pi_sign_in_uses_same_home_as_launch(self):
        self.core.apply([{'op':'set','key':'doc/settings/pi','value':{'runtimes':[{'name':'pi','argv':['/fake/pi','--mode','rpc'],
                         'bridge_env':{'HOME':'/fake/pi-home'}}]}}], 'fake')
        Pi(self.core).new({'account':'normal'})
        self.accounts.sign_in({'account':'normal','runtime':'pi'})
        self.assertEqual(self.spawns()[1]['env']['HOME'],self.spawns()[0]['env']['HOME'])

    def test_sign_in_refuses_inference_flags_or_positional_prompts_before_spawn(self):
        cases=[('pi',['/fake/pi','--mode','rpc','-p','prompt']),
               ('pi',['/fake/pi','--mode','rpc','--print','prompt']),
               ('pi',['/fake/pi','prompt','--mode','rpc']),
               ('pi',['/fake/pi','--mode','json']),
               ('pi',['/fake/pi','--resume','old-session']),
               ('codex',['/fake/codex','exec','prompt','app-server']),
               ('claude',['/fake/claude','-p','prompt'])]
        for runtime,argv in cases:
            core=FakeCore('accounts'); accounts=Accounts(core)
            key='doc/settings/tier1' if runtime=='claude' else 'doc/settings/'+runtime
            settings={'claude':argv} if runtime=='claude' else {'runtimes':[{'name':runtime,'argv':argv}]}
            core.apply([{'op':'set','key':key,'value':settings}], 'fake')
            with self.subTest(runtime=runtime,argv=argv),self.assertRaises(VqroError):
                accounts.sign_in({'account':'normal','runtime':runtime})
            self.assertFalse(any(m=='resource.spawn' for m,p in core.calls))

    def test_supported_node_prefix_and_safe_pi_options_are_literal(self):
        self.core.apply([{'op':'set','key':'doc/settings/pi','value':{'runtimes':[{'name':'pi',
            'argv':['/fake/node','/fake path/pi.js','--mode','rpc','--provider','openai-codex','--no-extensions']}]}}], 'fake')
        self.accounts.sign_in({'account':'normal','runtime':'pi'})
        self.assertEqual(self.spawns()[0]['argv'],['/fake/node','/fake path/pi.js','--provider','openai-codex','--no-extensions'])

    def test_registry_bounds_and_literal_paths(self):
        for bad in ('', '~/profile', 'relative/profile', '/path\x00bad'):
            with self.subTest(native_provider=bad), self.assertRaises(VqroError): self.add(native_provider=bad)
        with self.assertRaises(VqroError): self.add(kind='oauth_token')
        self.assertEqual(self.spawns(), [])

if __name__ == '__main__': unittest.main()
