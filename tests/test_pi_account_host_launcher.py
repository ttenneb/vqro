"""The execution-host launcher locates the shipped plugin; no native exec."""
from pathlib import Path
import runpy
import sys
import unittest
from unittest.mock import patch


class PiHostLauncher(unittest.TestCase):
    def test_plugin_path_is_resolved_by_the_host_launcher_not_the_core(self):
        script=Path(__file__).resolve().parents[1]/'extensions'/'pi_bridge'/'launch.py'
        module=runpy.run_path(str(script),run_name='launcher_test')
        with patch.dict(module['os'].environ,{'VQRO_PI_ACCOUNT':'synthetic-metadata'}),patch.object(sys,'argv',['launch.py','--','pi1','--mode','rpc']),patch.object(module['os'],'execvp') as execute:
            module['main']()
        argv=execute.call_args.args[1]
        self.assertEqual(argv[:2],['vqro-bridge-pi','--account-extension'])
        self.assertEqual(Path(argv[2]),script.parent.parent/'pi_accounts'/'index.mjs')
        self.assertEqual(argv[3:],['--','pi1','--mode','rpc'])

    def test_explicit_host_plugin_path_is_not_duplicated_or_replaced(self):
        script=Path(__file__).resolve().parents[1]/'extensions'/'pi_bridge'/'launch.py'
        module=runpy.run_path(str(script),run_name='launcher_test')
        args=['--account-extension','host-local-plugin.mjs','--','pi1']
        with patch.object(sys,'argv',['launch.py',*args]),patch.object(module['os'],'execvp') as execute:
            module['main']()
        self.assertEqual(execute.call_args.args[1],['vqro-bridge-pi',*args])


if __name__=='__main__':unittest.main()
