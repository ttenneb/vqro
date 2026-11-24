"""Installer file-selection emulation only; no PowerShell/install/native execution."""
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT=Path(__file__).resolve().parents[1]


class AccountInstallAssets(unittest.TestCase):
    def test_windows_preflight_and_copy_share_complete_native_account_inventory(self):
        script=(ROOT/'scripts/dev-install.ps1').read_text(encoding='utf-8')
        helper=script.split('function Get-PythonPackageFiles($Package) {',1)[1].split('function Find-Python',1)[0]
        names=re.search(r"foreach \(\$name in @\(([^)]*)\)\)",helper).group(1)
        assets=re.findall(r"'([^']+)'",names)
        self.assertEqual(set(assets),{'index.mjs','provider.mjs','limit_messages.json'})
        self.assertIn("-Filter '*.py' -File",helper)
        self.assertEqual(script.count('Get-PythonPackageFiles $package'),2)
        self.assertIn("foreach ($file in @(Get-PythonPackageFiles $package)) {\n            Assert-PlainPath (Join-Path $destination $file.Name) 'File'",script)
        self.assertIn('Get-PythonPackageFiles $package |\n            ForEach-Object { Copy-Item -LiteralPath $_.FullName -Destination $destination -Force }',script)
        with tempfile.TemporaryDirectory(prefix='vq-account-install-') as temporary:
            layout=Path(temporary)/'extensions';layout.mkdir()
            home=Path(temporary)/'home';home.mkdir()
            for source in (ROOT/'extensions').glob('*.py'):shutil.copyfile(source,layout/source.name)
            for package in (ROOT/'extensions').iterdir():
                if not package.is_dir() or not (package/'__init__.py').is_file():continue
                destination=layout/package.name;destination.mkdir()
                selected=list(package.glob('*.py'))
                if package.name=='pi_accounts':selected.extend(package/name for name in assets)
                for source in selected:shutil.copyfile(source,destination/source.name)
            for name in assets:self.assertTrue((layout/'pi_accounts'/name).is_file())
            result=subprocess.run([sys.executable,'-c','import accounts, pi'],cwd=layout,
                env={'HOME':str(home),'PATH':os.environ.get('PATH',''),'PYTHONPATH':str(layout),'PYTHONDONTWRITEBYTECODE':'1'},
                capture_output=True,text=True,timeout=10)
            self.assertEqual(result.returncode,0,result.stderr)

    def test_unix_package_marker_and_nonpython_assets_are_installed(self):
        self.assertTrue((ROOT/'extensions/pi_accounts/__init__.py').is_file())
        script=(ROOT/'scripts/dev-install.sh').read_text(encoding='utf-8')
        self.assertIn('cp extensions/pi_accounts/*.mjs extensions/pi_accounts/*.json',script)


if __name__=='__main__':unittest.main()
