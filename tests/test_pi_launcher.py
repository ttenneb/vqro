"""Trusted default Rust exec shim and explicit legacy choice; no runtime/core/model is started."""
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

LAUNCH = Path(__file__).resolve().parents[1]/'extensions/pi_bridge/launch.py'
LEGACY = LAUNCH.with_name('python_launch.py')


class PiLauncherTests(unittest.TestCase):
    @unittest.skipIf(sys.platform == "win32", "Rust-bridge stand-in is an executable POSIX shell script, not a native exe")
    def test_exec_preserves_literal_argv_and_never_imports_project_code(self):
        with tempfile.TemporaryDirectory(prefix='pi-launch-') as name:
            root=Path(name);binary=root/'bin/vqro-bridge-pi';binary.parent.mkdir();capture=root/'argv'
            binary.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "$CAPTURE"\n');binary.chmod(0o755)
            (root/'pi_bridge.py').write_text('raise RuntimeError("project code imported")\n')
            env={'HOME':name,'PATH':str(binary.parent),'PYTHONPATH':name,'CAPTURE':str(capture)}
            result=subprocess.run([sys.executable,'-I',str(LAUNCH),'--','/native path/pi','$(not-shell)','a\nb'],cwd=root,env=env,capture_output=True,text=True,timeout=5)
            self.assertEqual(result.returncode,0,result.stderr)
            self.assertEqual(capture.read_text(),'--\n/native path/pi\n$(not-shell)\na\nb\n')

    def test_missing_rust_binary_fails_explicitly_without_legacy_fallback(self):
        with tempfile.TemporaryDirectory(prefix='pi-no-rust-') as name:
            result=subprocess.run([sys.executable,'-I',str(LAUNCH),'--help'],cwd=name,env={'HOME':name,'PATH':name},capture_output=True,text=True,timeout=5)
            self.assertNotEqual(result.returncode,0);self.assertIn('Cannot start vqro-bridge-pi',result.stderr);self.assertIn('python_launch.py',result.stderr)

    def test_explicit_legacy_entry_remains_isolated_and_available(self):
        with tempfile.TemporaryDirectory(prefix='pi-legacy-') as name:
            result=subprocess.run([sys.executable,'-I',str(LEGACY),'--help'],cwd=name,env={'HOME':name,'PATH':'/usr/bin:/bin','PYTHONPATH':name},capture_output=True,text=True,timeout=5)
            self.assertEqual(result.returncode,0,result.stderr)


if __name__=='__main__':
    unittest.main()
