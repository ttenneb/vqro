"""Secondary regressions: metadata/argv/mock-exec only. No native/profile/auth calls."""
import io
import json
from pathlib import Path
import sys
import unittest
from unittest.mock import patch
sys.path[:0]=[str(Path(__file__).resolve().parents[1]/'extensions'),str(Path(__file__).resolve().parent)]
from agent_fixture import FakeCore
from accounts import Accounts
import account_profiles as profiles
import tier1_run
from vqro_ext import VqroError

ROW={'id':'fake','label':'Fake','runtime':'codex','kind':'subscription','profile':'/fake/profile'}

class AccountsSecondary(unittest.TestCase):
    def test_explicit_null_provider_is_rejected_before_registry_ui_or_bridge_fact(self):
        core=FakeCore('accounts');accounts=Accounts(core)
        with self.assertRaises(VqroError):accounts.add(dict(ROW,provider=None))
        self.assertIsNone(core.value(profiles.SETTINGS))
        accounts.add(ROW)
        accounts.show({})
        account,popup=profiles.choose(core,{},'codex','codex.new')
        self.assertIsNone(account);self.assertIn('popup',popup['client'])
        selected,_=profiles.choose(core,{'account':'fake'},'codex','codex.new')
        metadata=profiles.fact(selected)
        self.assertNotIn('provider',metadata)
        fixture=Path(__file__).parent/'fixtures'/'configured_account_omitted_provider.json'
        self.assertEqual(metadata,json.loads(fixture.read_text(encoding="utf-8")))  # same golden fixture parsed by Rust; no profile I/O
        self.assertFalse(any(method=='resource.spawn' for method,params in core.calls))

    def test_codex_sign_in_validates_full_suffix_before_any_spawn(self):
        bad=[['--unknown'],['--profile'],['--profile','fake'],['--'],['--','fake prompt'],
             ['app-server'],['fake prompt'],['--stdio','--unknown'],['--listen'],
             ['--listen','http://fake.invalid'],['--listen','stdio://','--profile','fake']]
        for tail in bad:
            with self.subTest(tail=tail),self.assertRaises(VqroError):
                profiles.native_login_argv('codex',['codex','app-server',*tail])
        core=FakeCore('accounts');accounts=Accounts(core)
        core.apply([{'op':'set','key':'doc/settings/codex','value':{'runtimes':[
            {'name':'fake','argv':['codex','app-server','--unknown']} ]}}],'fake config')
        with self.assertRaises(VqroError):accounts.sign_in({'account':'normal','runtime':'codex'})
        self.assertFalse(any(method=='resource.spawn' for method,params in core.calls))
        # Only these explicitly supported transport-only suffixes are removed. Profile selectors stay BEFORE subcommand.
        for tail in ([],['--stdio'],['--listen','stdio://']):
            argv=['codex','--profile','fake','-c','model="fake"','app-server',*tail]
            self.assertEqual(profiles.native_login_argv('codex',argv),['codex','--profile','fake','-c','model="fake"','login'])
        self.assertEqual(profiles.native_login_argv('codex',['node','/fake/codex.js','--profile','fake','app-server','--stdio']),
                         ['node','/fake/codex.js','--profile','fake','login'])

    def test_tier1_native_exec_strips_internal_fact_but_keeps_selected_profile(self):
        marker=json.dumps(profiles.fact(profiles.validate(ROW)))
        for runtime,selector in profiles.SELECTORS.items():
            with self.subTest(runtime=runtime),patch.dict(tier1_run.os.environ,
                    {'VQRO_ACCOUNT':marker,selector:'/fake/selected-profile'},clear=True), \
                    patch.object(tier1_run.os,'execvpe') as execute,patch('sys.stderr',new_callable=io.StringIO):
                tier1_run.main(['--',runtime])
                program,argv,env=execute.call_args[0]
                self.assertEqual(program,runtime);self.assertEqual(argv,[runtime])
                self.assertNotIn('VQRO_ACCOUNT',env)
                self.assertEqual(env[selector],'/fake/selected-profile')

if __name__=='__main__':unittest.main()
