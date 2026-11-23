"""Explicit prebuild artifact selection; private fake binaries, never native core/load."""
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from python_interpreter import shell_fixture

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location('scale_artifact', ROOT/'scripts/scale_artifact.py')
selector = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(selector)


@unittest.skipIf(sys.platform == "win32", "Linux timing-gate artifact controls require POSIX executable modes and shell fixtures")
class ScaleArtifactTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='scale-selection-')
        # Selectors return canonical paths (macOS /tmp is a symlink).
        self.root = Path(self.temp.name).resolve()
        self.checkout, self.target = self.root/'checkout', self.root/'target with spaces'
        (self.checkout/'tests').mkdir(parents=True)
        (self.checkout/'Cargo.toml').write_text('[package]\nname="vqro"\nversion="0.1.0"\n')
        (self.checkout/'tests/scale.rs').write_text('// fake target')
        self.manifest = self.checkout/'Cargo.toml'
        (self.target/'release/deps').mkdir(parents=True)
        self.current = self.executable(self.target/'release/deps/scale-current')
        self.finished = dict(reason='build-finished', success=True)

    def tearDown(self):
        self.temp.cleanup()

    def executable(self, path):
        path.write_text('#!/bin/sh\nexit 0\n')
        path.chmod(0o700)
        return path

    def message(self, **changes):
        message = dict(reason='compiler-artifact', package_id='path+file://checkout#vqro@0.1.0',
            manifest_path=str(self.manifest), target=dict(name='scale', kind=['test'], src_path=str(self.checkout/'tests/scale.rs')),
            profile=dict(test=True, debug_assertions=False), executable=str(self.current), fresh=False)
        message.update(changes)
        return message

    def test_cargo_json_selects_this_package_target_not_stale_extras_or_dependencies(self):
        for index in range(4): self.executable(self.target/f'release/deps/scale-stale-{index}')
        foreign = self.message(manifest_path=str(self.root/'other/Cargo.toml'))
        wrong_target = self.message(target=dict(name='other', kind=['test'], src_path=str(self.checkout/'tests/scale.rs')))
        path, row = selector.select_artifact([foreign, wrong_target, self.message(), self.finished], self.manifest, self.target)
        self.assertEqual(path, self.current)
        self.assertFalse(row['fresh'], 'explicit prebuild may compile; timing never queries Cargo')

    def test_failed_partial_ambiguous_or_wrong_target_streams_do_not_select(self):
        wrong = self.message(target=dict(name='scale', kind=['bin'], src_path=str(self.checkout/'tests/scale.rs')))
        for rows in ([self.message()], [self.message(), dict(reason='build-finished', success=False)],
                     [self.message(), self.message(), self.finished], [wrong, self.finished],
                     [self.message(profile=dict(test=False)), self.finished]):
            with self.subTest(rows=rows), self.assertRaises(ValueError):
                selector.select_artifact(rows, self.manifest, self.target)

    def test_requires_one_terminal_success_with_no_following_records(self):
        ordinary = dict(reason='unknown-ordinary-message')
        path, _ = selector.select_artifact([ordinary, self.message(), ordinary, self.finished], self.manifest, self.target)
        self.assertEqual(path, self.current)
        for rows in ([self.finished, self.message()], [self.message(), self.finished, self.finished],
                     [self.message(), self.finished, ordinary], [self.message(), self.finished, self.message()]):
            with self.subTest(rows=rows), self.assertRaises(ValueError):
                selector.select_artifact(rows, self.manifest, self.target)

    def test_missing_nonexec_outside_and_symlink_escape_paths_are_rejected(self):
        nonexec = self.target/'release/deps/no-exec'
        nonexec.write_text('not executable')
        outside = self.executable(self.root/'outside')
        escape = self.target/'release/deps/escape'
        escape.symlink_to(outside)
        for path in (nonexec, outside, escape, self.target/'release/deps/absent'):
            with self.subTest(path=path), self.assertRaises((ValueError, OSError)):
                selector.validate_path(path, self.target)

    def test_select_cli_prints_path_and_records_exact_caller_attested_hash_and_mtime(self):
        record = self.root/'record.json'
        result = subprocess.run([sys.executable, '-I', str(ROOT/'scripts/scale_artifact.py'), 'select',
            '--manifest', str(self.manifest), '--target-dir', str(self.target), '--record', str(record)],
            input='\n'.join(map(json.dumps, [self.message(), self.finished])), text=True, capture_output=True,
            env=dict(HOME=str(self.root), PATH='/usr/bin:/bin'))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.strip(), str(self.current))
        row = json.loads(record.read_text())
        self.assertEqual(row['assurance'], 'caller-attested, not revision-bound')
        self.assertEqual(row['sha256'], hashlib.sha256(self.current.read_bytes()).hexdigest())
        self.assertEqual(row['mtime_unix_ns'], self.current.stat().st_mtime_ns)
        self.assertEqual(row['selection']['method'], 'cargo compiler-artifact')

    def test_explicit_path_is_forwarded_by_gate_supervisor(self):
        spec = importlib.util.spec_from_file_location('check_lifetime', ROOT/'scripts/check_lifetime.py')
        lifetime = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(lifetime)
        self.assertIn('VQRO_SCALE_TEST_EXE', lifetime.FORWARDED)


@unittest.skipIf(sys.platform == "win32", "Linux timing-gate prebuild recipe executes /bin/bash and shebang helpers")
class PrebuildRecipeTests(unittest.TestCase):
    def test_failed_prebuild_never_reaches_exclusive_timing_even_with_old_files(self):
        doc = (ROOT/'docs/research/timing-gates.md').read_text()
        recipe = doc.split('## Strict proof before installation', 1)[1].split('```sh\n', 1)[1].split('\n```', 1)[0]
        with tempfile.TemporaryDirectory(prefix='prebuild-recipe-') as temp:
            root = Path(temp)
            target = root/'target'
            (target/'release/deps').mkdir(parents=True)
            for name in ('release/vqro', 'release/deps/scale-selected'):
                old = target/name
                old.write_text('#!/bin/sh\nexit 0\n')  # Old executable retained after a failed build.
                old.chmod(0o700)
            marker, wrapper = root/'exclusive-ran', root/'fake-slot'
            wrapper.write_text(shell_fixture('''import os,pathlib,sys
if '--exclusive' in sys.argv[1:]:
    pathlib.Path(os.environ['EXCLUSIVE_MARKER']).write_text('must not run')
    raise SystemExit(0)
release_build = 'cargo' in sys.argv and 'build' in sys.argv
if release_build:
    raise SystemExit(9 if os.environ['FAIL_PREBUILD']=='vqro' else 0)
if os.environ['FAIL_PREBUILD']=='scale':
    raise SystemExit(8)
print(pathlib.Path(os.environ['CARGO_TARGET_DIR'])/'release/deps/scale-selected')
''', sys.executable))
            wrapper.chmod(0o700)
            for failed in ('scale', 'vqro'):
                with self.subTest(failed=failed):
                    marker.unlink(missing_ok=True)
                    env = dict(HOME=str(root), PATH='/usr/bin:/bin', GATE_SLOT=str(wrapper),
                               CARGO_TARGET_DIR=str(target), EXCLUSIVE_MARKER=str(marker), FAIL_PREBUILD=failed)
                    result = subprocess.run(['/bin/bash', '-c', recipe], env=env, capture_output=True, text=True)
                    self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
                    self.assertFalse(marker.exists(), 'known failed prebuild must not enter exclusive timing')


if __name__ == '__main__':
    unittest.main()
