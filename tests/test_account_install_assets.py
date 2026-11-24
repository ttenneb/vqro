"""Installer file-selection emulation only; no PowerShell/install/native execution."""
import ast
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT=Path(__file__).resolve().parents[1]
sys.path.insert(0,str(ROOT/'extensions'))
from vqro_packages import package_files, copy_packages


class AccountInstallAssets(unittest.TestCase):
    def test_windows_preflight_and_copy_share_complete_native_account_inventory(self):
        script=(ROOT/'scripts/dev-install.ps1').read_text(encoding='utf-8')
        helper=script.split('function Get-PythonPackageFiles($Package) {',1)[1].split('function Find-Python',1)[0]
        self.assertIn("'vqro_packages.py'",helper)
        self.assertIn("'--package', $Package.Name",helper)
        assets=[p.name for p in package_files(ROOT/'extensions','pi_accounts') if p.suffix!='.py']
        self.assertEqual(set(assets),{'index.mjs','provider.mjs','limit_messages.json'})
        self.assertEqual(script.count('Get-PythonPackageFiles $package'),2)
        self.assertIn("foreach ($file in @(Get-PythonPackageFiles $package)) {\n            Assert-PlainPath (Join-Path $destination $file.Name) 'File'",script)
        self.assertIn('Get-PythonPackageFiles $package |\n            ForEach-Object { Copy-Item -LiteralPath $_.FullName -Destination $destination -Force }',script)
        with tempfile.TemporaryDirectory(prefix='vq-account-install-') as temporary:
            layout=Path(temporary)/'extensions';layout.mkdir()
            home=Path(temporary)/'home';home.mkdir()
            for source in (ROOT/'extensions').glob('*.py'):shutil.copyfile(source,layout/source.name)
            copy_packages(ROOT/'extensions',layout)
            for name in assets:self.assertTrue((layout/'pi_accounts'/name).is_file())
            result=subprocess.run([sys.executable,'-c','import accounts, pi'],cwd=layout,
                env={'HOME':str(home),'PATH':os.environ.get('PATH',''),'PYTHONPATH':str(layout),'PYTHONDONTWRITEBYTECODE':'1'},
                capture_output=True,text=True,timeout=10)
            self.assertEqual(result.returncode,0,result.stderr)

    def test_private_harness_copy_matches_installer_inventory_and_imports(self):
        from extension_layout import copy_packages as fixture_copy
        with tempfile.TemporaryDirectory(prefix='vq-package-layout-') as temporary:
            layout=Path(temporary)/'extensions';layout.mkdir()
            for source in (ROOT/'extensions').glob('*.py'):shutil.copy2(source,layout/source.name)
            copied=fixture_copy(ROOT/'extensions',layout)
            listed=subprocess.run([sys.executable,str(ROOT/'extensions/vqro_packages.py'),
                str(ROOT/'extensions')],capture_output=True,text=True,check=True,timeout=10)
            self.assertEqual(set(copied),{Path(p) for p in json.loads(listed.stdout)})
            for relative in copied:
                self.assertEqual((layout/relative).read_bytes(),(ROOT/'extensions'/relative).read_bytes())
            home=Path(temporary)/'home';home.mkdir()
            result=subprocess.run([sys.executable,'-I','-c',
                'import sys;sys.path.insert(0,sys.argv[1]);import accounts, pi;from pi_accounts.limits import match_limit',str(layout)],
                cwd=home,env={'HOME':str(home),'PATH':os.environ.get('PATH',''),'PYTHONDONTWRITEBYTECODE':'1'},
                capture_output=True,text=True,timeout=10)
            self.assertEqual(result.returncode,0,result.stderr)

    def test_incomplete_package_refuses_before_copying_any_file(self):
        with tempfile.TemporaryDirectory(prefix='vq-package-missing-') as temporary:
            root=Path(temporary)/'source';package=root/'pi_accounts';package.mkdir(parents=True)
            for name in ('__init__.py','limits.py','index.mjs','provider.mjs'):(package/name).write_text('')
            destination=Path(temporary)/'destination'
            with self.assertRaisesRegex(ValueError,'limit_messages.json'):copy_packages(root,destination)
            self.assertFalse(destination.exists())

    def test_private_code_copy_harnesses_use_shared_package_inventory(self):
        names=('pi_provider_e2e.py','e2e_agent_stop.py','e2e_tui_providers.py',
            'e2e_sidebar_scale.py','agent_popup_latency.py','agent_panel_bandwidth.py',
            'e2e_workspace_tabs.py','codex_launcher_e2e.py','claude_launcher_e2e.py',
            'e2e_agent_origin.py','e2e_tui_scale.py')
        for name in names:
            with self.subTest(harness=name):
                tree=ast.parse((ROOT/'tests'/name).read_text(encoding='utf-8'))
                self.assertTrue(any(isinstance(node,ast.ImportFrom) and node.module=='extension_layout'
                    and any(alias.name=='copy_packages' for alias in node.names) for node in ast.walk(tree)))
                self.assertTrue(any(isinstance(node,ast.Call) and isinstance(node.func,ast.Name)
                    and node.func.id=='copy_packages' for node in ast.walk(tree)))

    def test_unix_package_marker_and_nonpython_assets_are_installed(self):
        self.assertTrue((ROOT/'extensions/pi_accounts/__init__.py').is_file())
        script=(ROOT/'scripts/dev-install.sh').read_text(encoding='utf-8')
        self.assertIn('python3 extensions/vqro_packages.py extensions --check',script)
        self.assertIn('python3 extensions/vqro_packages.py extensions --copy-to',script)


if __name__=='__main__':unittest.main()
