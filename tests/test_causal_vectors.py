"""Same data vectors as Rust, including exact byte/depth and numeric edges."""
import json
from pathlib import Path
import sys
import unittest
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'extensions'))
from vqro_provider import causal_links, ProviderError


class CausalVectors(unittest.TestCase):
    def test_shared_causal_bounds_vectors(self):
        cases = json.loads((Path(__file__).parent / 'fixtures/causal_vectors.json').read_text())['cases']
        for case in cases:
            with self.subTest(case=case['name']):
                try:
                    causal_links(json.loads(case['wire']))
                    accepted = True
                except (json.JSONDecodeError, ProviderError):
                    accepted = False
                codec = case.get('codec_results', {}).get('python')
                self.assertEqual(accepted, codec['valid'] if codec else case['valid'])
                if codec:
                    encoded = json.dumps(json.loads(case['wire'])['cause'], ensure_ascii=False, separators=(',', ':'), allow_nan=False).encode()
                    self.assertEqual(len(encoded), codec['cause_bytes'])
