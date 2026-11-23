"""The same key display table is checked by Rust tui::keys and Python extensions."""
import json
from pathlib import Path
import runpy
import unittest

ROOT=Path(__file__).resolve().parents[1]
key_labels=runpy.run_path(str(ROOT/'extensions/vqro_display.py'))['key_labels']

class KeyLabelTests(unittest.TestCase):
    def test_shared_native_extension_table(self):
        for case in json.loads((ROOT/'tests/fixtures/key_labels.json').read_text(encoding="utf-8")):
            for mode in (None,'full','unknown','short'):
                with self.subTest(input=case['input'],mode=mode):
                    self.assertEqual(key_labels(case['input'],mode),case['short' if mode=='short' else 'full'])

if __name__=='__main__':
    unittest.main()
