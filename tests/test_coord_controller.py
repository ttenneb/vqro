# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Controller bounds/reporting only: no real namespace, core, runtime or model calls."""
import argparse
import contextlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

import run_coord_conformance as cli
from agent_conformance.part_b.runner import write_controller_timeout
from agent_conformance.results import Case, Result


class ControllerTimeout(unittest.TestCase):
    cases = [Case('I1', 'first'), Case('I2', 'second'), Case('I3', 'third')]

    def journal(self, root, index, result):
        path = root / f'{index:02}' / 'result.json'
        path.parent.mkdir()
        if result.fixture_root is None:
            result.fixture_root = str(path.parent)
        path.write_text(json.dumps(result.json()))

    def test_missing_partial_and_wrong_identity_journals_never_become_pass_or_na(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.journal(root, 0, Result('I1', 'first', 'pass'))
            (root / '01').mkdir()
            (root / '01/result.json').write_text('{')
            self.journal(root, 2, Result('OTHER', 'wrong case', 'pass'))
            document = write_controller_timeout(root, {}, self.cases, self.cases, 1800)
            self.assertEqual(document['summary']['pass'], 1)
            self.assertEqual(document['summary']['fail'], 2)
            self.assertEqual(document['not_run'], ['I2', 'I3'])
            for row in document['results'][1:]:
                self.assertEqual((row['status'], row['detail']), ('fail', 'not run: controller timeout'))
                self.assertEqual(row['reason'], 'controller_timeout')
            self.assertFalse(document['complete_inventory'])
            self.assertFalse(document['qualification_complete'])
            self.assertFalse(document['passed_selection'])
            self.assertIn('not run: controller timeout', (root / 'results.md').read_text())
            self.assertTrue((root / 'parent-timeout.json').is_file())

    def test_malformed_declarations_are_invalid_journals_not_reporting_crashes(self):
        for declarations in (None, [], 'invalid'):
            with self.subTest(declarations=declarations), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                (root / '00').mkdir()
                (root / '00/result.json').write_text(json.dumps({
                    'case': 'I1', 'title': 'first', 'status': 'not-applicable', 'declarations': declarations}))
                document = write_controller_timeout(root, {}, self.cases, self.cases, 1800)
                self.assertEqual(document['not_run'], ['I1', 'I2', 'I3'])
                self.assertEqual(document['summary']['fail'], 3)
                self.assertTrue((root / 'results.json').is_file())
                self.assertTrue((root / 'results.md').is_file())
                self.assertTrue((root / 'parent-timeout.json').is_file())

    def test_malformed_evidence_is_not_a_completed_case(self):
        for evidence in (None, {}, 'invalid'):
            with self.subTest(evidence=evidence), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                self.journal(root, 0, Result('I1', 'first', 'pass', evidence=evidence))
                document = write_controller_timeout(root, {}, self.cases, self.cases, 1800)
                self.assertEqual(document['not_run'], ['I1', 'I2', 'I3'])
                self.assertEqual(document['summary']['pass'], 0)

    def test_same_case_journal_from_a_different_run_is_not_recovered(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.journal(root, 0, Result('I1', 'first', 'pass', fixture_root='/different-run/00'))
            document = write_controller_timeout(root, {}, self.cases, self.cases, 1800)
            self.assertEqual(document['not_run'], ['I1', 'I2', 'I3'])
            self.assertEqual(document['summary']['pass'], 0)

    def test_only_a_completed_na_with_its_exclusion_is_retained(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            self.journal(root, 0, Result('I1', 'first', 'not-applicable', declarations={
                'exclusion': {'path': 'fixture.optional', 'actual': False}}))
            document = write_controller_timeout(root, {}, self.cases, self.cases, 900)
            self.assertEqual(document['summary']['not-applicable'], 1)
            self.assertEqual([row['status'] for row in document['results'][1:]], ['fail', 'fail'])

    def test_parent_timeout_is_failure_even_if_all_case_journals_completed(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for index, case in enumerate(self.cases):
                self.journal(root, index, Result(case.id, case.title, 'pass'))
            document = write_controller_timeout(root, {}, self.cases, self.cases, 900)
            self.assertEqual(document['not_run'], [])
            self.assertFalse(document['passed_selection'])
            self.assertFalse(document['complete_inventory'])
            self.assertFalse(document['qualification_complete'])

    def invoke(self, root, extra, process):
        if sys.platform != 'linux':
            self.skipTest('controller isolation probe reads Linux /proc/self/ns/net')
        spec = root / 'spec.md'
        spec.write_text('fixture inventory is mocked, not qualification')
        manifest = root / 'manifest.json'
        manifest.write_text(json.dumps({'name': 'private fixture', 'extensions': str(root / 'ext'),
            'specs': {key: str(spec) for key in ('agent', 'coordination', 'conformance')}}))
        argv = ['--run', '--implementation', str(manifest), '--vqro', str(root / 'core'),
                '--scratch-parent', str(root), *extra]
        output = io.StringIO()
        with patch.object(cli, 'cases_from_spec', return_value=self.cases), \
             patch.object(cli, 'python', return_value=sys.executable), \
             patch.object(cli.subprocess, 'run', **process) as run, contextlib.redirect_stdout(output):
            code = cli.main(argv)
        return code, json.loads(output.getvalue()), run.call_args

    def test_default_and_configured_limits_preserve_isolation_and_publish_only_real_reports(self):
        for seconds, extra in ((900, []), (1800, ['--selection-timeout', '1800'])):
            with self.subTest(seconds=seconds), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                code, output, call = self.invoke(root, extra, {
                    'side_effect': subprocess.TimeoutExpired(['not executed'], seconds)})
                self.assertEqual(code, 124)
                self.assertEqual(call.kwargs['timeout'], seconds)
                command = call.args[0]
                self.assertEqual(command[:4], ['/usr/bin/unshare', '-Urnpmf', '--kill-child=KILL', '--mount-proc'])
                self.assertEqual(command[command.index('--selection-timeout') + 1], str(seconds))
                self.assertFalse(any(key.startswith('HERDR_') for key in call.kwargs['env']))
                for key in ('results', 'table', 'timeout'):
                    self.assertTrue(Path(output[key]).is_file())
                document = json.loads(Path(output['results']).read_text())
                self.assertEqual(document['not_run'], ['I1', 'I2', 'I3'])
                self.assertEqual(document['summary']['fail'], 3)
                self.assertEqual(document['implementation']['controller_timeout_seconds'], seconds)
                self.assertNotIn('controller_timeout_seconds', json.loads((root / 'manifest.json').read_text()))

    def test_child_crash_does_not_publish_nonexistent_result_links(self):
        with tempfile.TemporaryDirectory() as directory:
            code, output, _ = self.invoke(Path(directory), [], {
                'return_value': subprocess.CompletedProcess(['not executed'], 7)})
            self.assertEqual(code, 7)
            self.assertFalse({'results', 'table', 'timeout'} & output.keys())

    def test_invalid_limits_are_rejected(self):
        for value in ('0', '-1', 'nan', 'inf', '1.5'):
            with self.assertRaises(argparse.ArgumentTypeError):
                cli.positive_seconds(value)


if __name__ == '__main__':
    unittest.main()
