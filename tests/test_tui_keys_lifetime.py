# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Execute the keys harness's real helpers without starting any process/backend."""
import ast
import os
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import Mock

SOURCE = Path(__file__).with_name('e2e_tui_keys.py').read_text()
TREE = ast.parse(SOURCE)


def helpers():
    names = {'CoreLost', 'check_core', 'wait', 'diagnostics'}
    nodes = [node for node in TREE.body if isinstance(node, (ast.ClassDef, ast.FunctionDef)) and node.name in names]
    clock = SimpleNamespace(now=0)
    def sleep(seconds):
        clock.now += seconds
    namespace = {'core': None, 'core_ready': False, 'core_stopping': False, 'R': '/private/fixture',
                 'ENV': {'VQRO_SOCKET': '/private/fixture/p.sock'},
                 'os': SimpleNamespace(path=SimpleNamespace(exists=Mock(return_value=True))),
                 'time': SimpleNamespace(time=lambda: clock.now, sleep=Mock(side_effect=sleep))}
    exec(compile(ast.Module(body=nodes, type_ignores=[]), '<keys helpers>', 'exec'), namespace)
    return namespace


class CoreEvidenceTests(unittest.TestCase):
    def test_dead_core_fails_before_a_label_predicate_or_sleep(self):
        ns = helpers()
        ns['core'] = SimpleNamespace(pid=123, poll=lambda: -15)
        predicate = Mock(return_value=False)
        with self.assertRaisesRegex(ns['CoreLost'], 'pid=123 exit=-15'):
            ns['wait'](predicate, 5)
        predicate.assert_not_called()
        ns['time'].sleep.assert_not_called()

    def test_vanished_socket_with_live_process_is_also_fatal(self):
        ns = helpers()
        ns.update(core=SimpleNamespace(pid=123, poll=lambda: None), core_ready=True)
        ns['os'].path.exists.return_value = False
        with self.assertRaisesRegex(ns['CoreLost'], 'exit=None socket=False'):
            ns['wait'](lambda: False, 5)

    def test_core_loss_inside_predicate_cannot_be_swallowed(self):
        ns = helpers()
        def predicate():
            raise ns['CoreLost']('vanished during RPC')
        with self.assertRaisesRegex(ns['CoreLost'], 'vanished during RPC'):
            ns['wait'](predicate, 5)

    def test_intentional_cleanup_can_wait_after_core_exit(self):
        ns = helpers()
        ns.update(core=SimpleNamespace(pid=123, poll=lambda: 0), core_stopping=True)
        self.assertTrue(ns['wait'](lambda: True, 10))

    def test_only_successful_runs_delete_their_private_root(self):
        trial = next(n for n in TREE.body if isinstance(n, ast.Try))
        delete = next(n for n in trial.finalbody if isinstance(n, ast.If) and any(
            isinstance(c, ast.Attribute) and c.attr == 'rmtree' for c in ast.walk(n)))
        for left, results, error, removed in ((True, [True], None, True), (True, [False], None, False),
                                              (True, [True], RuntimeError, False), (False, [True], None, False),
                                              (True, [], None, False)):
            with self.subTest(left=left, results=results, error=error):
                remove = Mock()
                ns = {'left': left, 'results': results, 'R': '/private/fixture', 'print': Mock(),
                      'shutil': SimpleNamespace(rmtree=remove), 'sys': SimpleNamespace(exc_info=lambda: (error, None, None))}
                exec(compile(ast.Module(body=[delete], type_ignores=[]), '<keys root retention>', 'exec'), ns)
                self.assertEqual(remove.called, removed)

    def test_original_core_log_survives_an_unlinked_path_without_moving_child_offset(self):
        ns = helpers()
        output = Mock()
        ns.update(os=os, core_log=None, print=output, glob=SimpleNamespace(glob=lambda pattern: []))
        with tempfile.TemporaryFile() as log:
            log.write(b'original core stop reason\n')
            log.flush()
            position = log.tell()
            ns['core_log'] = log
            ns['diagnostics']()
            self.assertEqual(log.tell(), position)
        self.assertTrue(any('original core stop reason' in c.args[0] for c in output.call_args_list))

    def test_root_is_printed_before_startup_and_exception_diagnostics_precede_cleanup(self):
        trial = next(n for n in TREE.body if isinstance(n, ast.Try))
        root_print = next(n for n in TREE.body if isinstance(n, ast.Expr) and isinstance(n.value, ast.Call)
                          and isinstance(n.value.func, ast.Name) and n.value.func.id == 'print')
        self.assertLess(root_print.lineno, trial.lineno)
        first = trial.finalbody[0]
        self.assertIsInstance(first, ast.If)
        output = []
        ns = {'sys': SimpleNamespace(exc_info=lambda: (RuntimeError, None, None)),
              'diagnostics': lambda: output.append('diagnostics')}
        exec(compile(ast.Module(body=[first], type_ignores=[]), '<keys pre-cleanup diagnostics>', 'exec'), ns)
        self.assertEqual(output, ['diagnostics'])


if __name__ == '__main__':
    unittest.main()
