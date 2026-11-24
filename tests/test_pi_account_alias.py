"""Run the shipped native-extension API fakes, not a Pi/model process."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest


class NativeAccountAlias(unittest.TestCase):
    def test_public_extension_api_fakes(self):
        node = shutil.which('node')
        self.assertIsNotNone(node, 'Node is required to check the shipped Pi native extension')
        with tempfile.TemporaryDirectory(prefix='vqro-account-api-fakes-') as home:
            env = {'HOME': home, 'PATH': os.environ.get('PATH', '')}
            if os.name == 'nt' and 'SystemRoot' in os.environ:
                env['SystemRoot'] = os.environ['SystemRoot']
            result = subprocess.run([node, str(Path(__file__).with_name('pi_account_alias.mjs'))],
                                    env=env, capture_output=True, text=True, timeout=15)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('PASS native account extension API fakes', result.stdout)


if __name__ == '__main__':
    unittest.main()
