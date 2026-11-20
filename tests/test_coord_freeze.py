# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Target metadata needed by a native producer stays in the immutable closure."""
import hashlib
import tempfile
import unittest
from pathlib import Path
from agent_conformance.part_b.runner import freeze


class TargetClosure(unittest.TestCase):
    def test_sibling_manifests_and_defaults_are_captured_not_read_live(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / 'extensions'
            (source / 'manifests').mkdir(parents=True)
            for name, content in [('coordination.py', '# fixture'), ('manifests/coord.json', '{}'),
                                  ('manifests/agents.json', '{"name":"agents"}'),
                                  ('defaults.json', '{"extensions":[]}')]:
                (source / name).write_text(content)
            core = root / 'core'
            core.write_bytes(b'nonexecuted fixture')
            pinned, _ = freeze(root / 'capture', {'extensions': str(source)}, core)
            for name in ('manifests/agents.json', 'defaults.json'):
                captured = Path(pinned['extensions']) / name
                before = captured.read_bytes()
                self.assertEqual(hashlib.sha256(before).hexdigest(), pinned['target_sha256'][name])
                (source / name).write_text('changed after capture')
                self.assertEqual(captured.read_bytes(), before)


if __name__ == '__main__':
    unittest.main()
