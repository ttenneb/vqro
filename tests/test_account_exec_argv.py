"""Actual wrapper exec seams with independent CRT decoding; no native execution."""
import json
import unittest
from types import SimpleNamespace
from unittest.mock import Mock, patch
from test_pi_rpc_testing import crt_split
from accounts_keychain import Secret
import accounts_keychain_helper as helper
import tier1_run


class AccountExecArgv(unittest.TestCase):
    def argv(self):
        return [r'C:\Program Files\Pi\pi.exe','-e',r'C:\Native Extensions\index.mjs',
                '--vqro-account',json.dumps({'id':'b','label':'B spaced','native_provider':'alias-b'}),
                'trailing space \\']

    def test_tier1_exec_preserves_explicit_program_and_crt_roundtrip_or_posix_argv(self):
        for platform in ('nt','posix'):
            with self.subTest(platform=platform):
                argv=self.argv();execute=Mock()
                fake=SimpleNamespace(name=platform,environ={},execvpe=execute)
                with patch.object(tier1_run,'os',fake),patch('sys.stderr'):
                    tier1_run.main(['--',*argv])
                execute.assert_called_once();program,encoded,_=execute.call_args.args
                self.assertEqual(program,argv[0])
                self.assertEqual(crt_split(' '.join(encoded)) if platform=='nt' else encoded,argv)

    def test_subscription_helper_exec_preserves_program_and_crt_roundtrip_without_retry(self):
        for platform in ('nt','posix'):
            with self.subTest(platform=platform):
                argv=self.argv();execute=Mock();store=Mock()
                store.retrieve.return_value=Secret('synthetic-only-never-a-real-token')
                with patch.object(helper,'os',SimpleNamespace(name=platform)):
                    helper.launch('b'*32,'anthropic',argv,credential_kind='claude_subscription',
                                  store=store,environment={},execute=execute)
                execute.assert_called_once();store.retrieve.assert_called_once_with('b'*32)
                program,encoded,_=execute.call_args.args
                self.assertEqual(program,argv[0])
                self.assertEqual(crt_split(' '.join(encoded)) if platform=='nt' else encoded,argv)


if __name__=='__main__':unittest.main()
