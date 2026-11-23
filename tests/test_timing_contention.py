"""Timing grading controls: deterministic facts/private locks, never real load or backends."""
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location('timing_contention', ROOT/'scripts/timing_contention.py')
sampler = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(sampler)


class TimingFacts(unittest.TestCase):
    def test_contention_thresholds_and_unavailable_facts(self):
        def facts(running, slots=()):
            return [dict(procs_running=n, other_slots=list(slots)) for n in running]
        self.assertEqual(sampler.classify(facts([2, 4, 100]), 8, 10, 10, []), (False, [], 4))
        self.assertIn('median_procs_running_above_half_cores', sampler.classify(facts([2, 5, 100]), 8, 10, 10, [])[1])
        self.assertIn('runqueue_wait_exceeds_runtime', sampler.classify(facts([1]), 8, 10, 11, [])[1])
        self.assertIn('other_gate_slot_held', sampler.classify(facts([1], ['quick-slot-1.lock']), 8, 10, 0, [])[1])
        self.assertIn('unavailable', sampler.classify([], 8, 0, 0, ['unsupported platform'])[1])

    def test_strict_breaches_fail_and_only_breaches_can_be_unverified(self):
        for facts in [dict(status='available', contended=True), dict(status='unavailable', contended=True)]:
            for strict in (False, True):
                self.assertEqual(sampler.grade(0, facts, strict)[0], 0)
                self.assertEqual(sampler.grade(1, facts, strict)[0], 1)
                self.assertEqual(sampler.grade(78, facts, strict)[0], 1 if strict else 77)
                self.assertEqual(sampler.grade(77, facts, strict)[0], 77)
        self.assertEqual(sampler.grade(78, dict(status='available', contended=False), False)[0], 1)

    def test_tree_accounts_for_worker_threads_and_their_children(self):
        with tempfile.TemporaryDirectory() as temp:
            proc = Path(temp)
            for pid in (1, 2, 3):
                (proc/str(pid)).mkdir()
                (proc/str(pid)/'stat').write_text(f'{pid} (task) ' + ' '.join(['S']+['0']*18+[str(pid+100)]))
            for process, tid, children, runtime, wait in [(1, 1, '', 10, 20), (1, 3, '2', 30, 40), (2, 2, '', 50, 60)]:
                task = proc/str(process)/'task'/str(tid)
                task.mkdir(parents=True)
                (task/'children').write_text(children)
                (task/'schedstat').write_text(f'{runtime} {wait} 1')
            rows = sampler.tree_schedstat(1, proc)
            self.assertEqual(len(rows), 3)
            self.assertEqual(sum(row[0] for row in rows.values()), 90)
            self.assertEqual(sum(row[1] for row in rows.values()), 120)


@unittest.skipUnless(os.environ.get('VQRO_TEST_GATE_SLOT'), 'external gate-slot wrapper not supplied')
class ExclusiveSlots(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='exclusive-slots-')
        self.root = Path(self.temp.name)
        self.wrapper = os.environ['VQRO_TEST_GATE_SLOT']
        self.env = dict(HOME=str(self.root), PATH='/usr/bin:/bin', VQRO_GATE_SLOT_DIR=str(self.root),
                        GATE_SLOTS='1', GATE_QUICK_SLOTS='1')

    def tearDown(self):
        self.temp.cleanup()

    def run_slot(self, *args):
        return subprocess.run(['/bin/bash', self.wrapper, *args], env=self.env, text=True,
                              capture_output=True, timeout=5)

    def test_exclusive_timeout_is_never_success_or_optional_skip_and_releases_partial_locks(self):
        import fcntl
        with (self.root/'quick-slot-1.lock').open('w') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX)
            result = self.run_slot('--exclusive', '--wait-seconds', '0', '/bin/true')
            self.assertEqual(result.returncode, 75, result.stderr)
            self.assertIn('could not get exclusive slots', result.stderr)
            with (self.root/'slot-1.lock').open('w') as full:
                fcntl.flock(full, fcntl.LOCK_EX | fcntl.LOCK_NB)
        self.assertEqual(self.run_slot('--exclusive', '--wait-seconds', '0', '/bin/true').returncode, 0)

    def test_exclusive_metadata_survives_clean_env_and_fixture_does_not_inherit_locks(self):
        program = '''import fcntl,os,pathlib
assert os.environ['VQRO_GATE_EXCLUSIVE']=='1'
root=pathlib.Path(os.environ['VQRO_GATE_SLOT_DIR'])
assert set(os.environ['VQRO_GATE_SLOT_HELD'].split(','))=={'slot-1.lock','quick-slot-1.lock'}
# All slots really are locked while the command runs.
for name in ('slot-1.lock','quick-slot-1.lock'):
    with (root/name).open('w') as lock:
        try: fcntl.flock(lock,fcntl.LOCK_EX|fcntl.LOCK_NB)
        except BlockingIOError: pass
        else: raise AssertionError('exclusive admission did not own '+name)
assert not any('slot-' in os.readlink(fd) for fd in pathlib.Path('/proc/self/fd').iterdir() if fd.exists())
'''
        result = self.run_slot('--exclusive', '--wait-seconds', '0', 'env', '-i', 'PATH=/usr/bin:/bin',
                               'python3', '-c', program)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_normal_quick_call_and_command_failure_keep_their_status(self):
        result = self.run_slot('env', '-i', 'PATH=/usr/bin:/bin', '/bin/bash', '-c',
                               'test "$VQRO_GATE_SLOT_HELD" = quick-slot-1.lock || exit 1; exit 9', 'quick')
        self.assertEqual(result.returncode, 9, result.stderr)
        result = self.run_slot('env', '-i', 'PATH=/usr/bin:/bin', '/bin/bash', '-c',
                               'test "$VQRO_GATE_SLOT_HELD" = slot-1.lock')
        self.assertEqual(result.returncode, 0, result.stderr)


if __name__ == '__main__':
    unittest.main()
