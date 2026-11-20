"""Owned native job-lifetime regressions: real check.sh bootstrap, no gate suites/backends/builds."""
import errno
import importlib.util
import json
import os
from pathlib import Path
import select
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import Mock, patch

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('check_lifetime', ROOT/'scripts/check_lifetime.py')
lifetime = importlib.util.module_from_spec(spec)
spec.loader.exec_module(lifetime)


def collected_or_empty_cgroup(events):
    """Only called after all owned-member pidfds reported exit; collection can race this read."""
    try:
        content = events.read_text()
    except OSError as error:
        if error.errno in (errno.ENOENT, errno.ENODEV):
            return True
        raise
    return 'populated 0' in content.splitlines()


class CgroupObservationTests(unittest.TestCase):
    def test_collected_or_empty_is_accepted(self):
        for content, error in (('populated 0\nfrozen 0\n', None), (None, errno.ENOENT), (None, errno.ENODEV)):
            with self.subTest(content=content, error=error):
                events = Mock()
                if error: events.read_text.side_effect = OSError(error, 'fixture collection race')
                else: events.read_text.return_value = content
                self.assertTrue(collected_or_empty_cgroup(events))
                events.read_text.assert_called_once_with()

    def test_populated_or_other_errors_are_not_collected(self):
        for content in ('populated 1\nfrozen 0\n', 'frozen 0\n', ''):
            self.assertFalse(collected_or_empty_cgroup(Mock(read_text=Mock(return_value=content))))
        for code in (errno.EACCES, errno.EIO, errno.EBADF, errno.ENOTDIR):
            with self.subTest(errno=code):
                with self.assertRaises(OSError) as caught:
                    collected_or_empty_cgroup(Mock(read_text=Mock(side_effect=OSError(code, 'fixture error'))))
                self.assertEqual(caught.exception.errno, code)


@unittest.skipUnless(sys.platform == 'linux' and hasattr(os, 'pidfd_open'), 'Linux pidfd supervision')
class LifetimeUnitTests(unittest.TestCase):
    def test_wrong_owner_generation_never_launches(self):
        with patch.object(lifetime.subprocess, 'Popen') as spawn:
            self.assertEqual(lifetime.supervise([(os.getpid(), 'wrong-generation')], ['/bin/true']), 143)
            spawn.assert_not_called()

    def test_missing_owner_never_launches(self):
        with patch.object(lifetime.os, 'pidfd_open', side_effect=ProcessLookupError), patch.object(lifetime.subprocess, 'Popen') as spawn:
            self.assertEqual(lifetime.supervise([(os.getpid(), 'gone')], ['/bin/true']), 143)
            spawn.assert_not_called()

    def test_quick_bootstrap_without_systemd_stays_unscoped(self):
        with tempfile.TemporaryDirectory(prefix='gcl-fallback-') as directory:
            root = Path(directory); (root/'scripts').mkdir(); (root/'bin').mkdir()
            fake = root/'bin/systemctl'; fake.write_text('#!/bin/sh\nexit 1\n'); fake.chmod(0o755)
            source = (ROOT/'scripts/check.sh').read_text()
            prefix = source[:source.index('BEFORE=$(snapshot')]
            script = root/'scripts/check.sh'
            script.write_text(prefix + '\ntest -z "$CGROUP" || exit 9\nrmdir "$RUN"\necho fixture-unscoped\n')
            script.chmod(0o755)
            env = {'HOME': str(root), 'PATH': str(root/'bin')+':/usr/bin:/bin', 'VQRO_CHECK_SCOPED': '1'}
            result = subprocess.run([str(script), 'quick'], env=env, text=True, capture_output=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn('UNSCOPED', result.stderr)
            self.assertIn('fixture-unscoped', result.stdout)

    def test_failed_service_launch_is_not_replayed_unscoped(self):
        with patch.object(lifetime.subprocess, 'call', return_value=17) as run:
            self.assertEqual(lifetime.launch(['/bin/true']), 17)
            self.assertEqual(run.call_count, 1)

    def test_service_uses_native_cleanup_and_does_not_forward_secrets(self):
        with patch.dict(os.environ, {'HOME': '/fixture', 'PATH': '/usr/bin:/bin', 'CHECK_JOBS': '2',
                                     'HERDR_SOCKET': 'do-not-forward', 'API_TOKEN': 'secret'}, clear=True), \
             patch.object(lifetime.subprocess, 'call', return_value=0) as run:
            self.assertEqual(lifetime.launch(['/bin/true']), 0)
        args = run.call_args.args[0]
        self.assertIn('--property=KillMode=control-group', args)
        self.assertIn('--property=ExitType=main', args)
        self.assertIn('--expand-environment=no', args)
        self.assertIn('-i', args)
        self.assertIn('CHECK_JOBS=2', args)
        self.assertFalse(any('secret' in arg or 'HERDR_' in arg for arg in args))


@unittest.skipUnless(sys.platform == 'linux' and hasattr(signal, 'pidfd_send_signal'), 'Linux pidfd integration')
class LifetimeServiceTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        runtime = f'/run/user/{os.getuid()}'
        cls.control = {'HOME': os.environ['HOME'], 'PATH': '/usr/bin:/bin', 'XDG_RUNTIME_DIR': runtime,
                       'DBUS_SESSION_BUS_ADDRESS': f'unix:path={runtime}/bus'}
        if not Path(runtime+'/bus').is_socket() or not shutil.which('systemd-run'):
            raise unittest.SkipTest('no accessible systemd user manager')
        probe = subprocess.run(['systemctl', '--user', 'show', '--property=Version'], env=cls.control,
                               capture_output=True, timeout=5)
        if probe.returncode:
            raise unittest.SkipTest('systemd user manager unavailable')

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='gcl-')
        self.root = Path(self.temp.name)
        self.state = self.root/'state'; self.state.mkdir()
        self.repo = self.root/'repo'; (self.repo/'scripts').mkdir(parents=True)
        self.env = dict(self.control, HOME=str(self.root), PYTHONDONTWRITEBYTECODE='1',
                        CHECK_RECEIPT=str(self.root/'receipt-${literal}'))
        self.log = (self.root/'controller.log').open('w+')
        self.process = self.driver = None
        self.unit = None
        self.fds = []
        self.groups = []  # Private sessions created by this fixture, never the caller's process group.
        self.sentinel = subprocess.Popen(['/bin/sleep', '300'], env=self.env, start_new_session=True)
        os.mkfifo(self.state/'ready-fifo'); os.mkfifo(self.state/'finish-fifo')
        forest = self.root/'forest.py'
        forest.write_text('''import os, pathlib, signal, sys
state = pathlib.Path(sys.argv[1])
child = os.fork()
if child == 0:
    os.setsid()
    if os.fork(): os._exit(0)
    if sys.argv[2] == 'resistant': signal.signal(signal.SIGTERM, signal.SIG_IGN)
    (state/'leaf-pid').write_text(str(os.getpid()))
    with (state/'ready-fifo').open('w') as ready: ready.write(str(os.getpid())+'\\n')
    while True: signal.pause()
os.waitpid(child, 0)
while True: signal.pause()
''')
        self.forest = forest

    def stage(self, resistant=True):
        source = (ROOT/'scripts/check.sh').read_text()
        prefix = source[:source.index('BEFORE=$(snapshot')]
        state = shlex.quote(str(self.state))
        body = f'''\nSTATE={state}
printf '%s\\n' "$$" > "$STATE/gate-pid"
printf '%s\\n' "$PPID" > "$STATE/supervisor-pid"
printf '%s\\n' "$RUN" > "$STATE/run-dir"
printf '%s\\n' "$VQRO_CHECK_UNIT" > "$STATE/unit"
printf '%s\\n' "$CGROUP" > "$STATE/cgroup"
printf '%s\\n' "$CHECK_RECEIPT" > "$STATE/forwarded-receipt"
python3 -I {shlex.quote(str(self.forest))} "$STATE" {'resistant' if resistant else 'normal'} &
printf '%s\\n' "$!" > "$STATE/forest-pid"
IFS= read -r leaf < "$STATE/ready-fifo"
scoped_leftovers > "$STATE/leftovers"
printf 'fixture-live-stdout\\n'
printf 'fixture-live-stderr\\n' >&2
touch "$STATE/ready"
IFS= read -r code < "$STATE/finish-fifo"
exit "$code"
'''
        script = self.repo/'scripts/check.sh'
        script.write_text(prefix + body); script.chmod(0o755)
        shutil.copy2(ROOT/'scripts/check_lifetime.py', self.repo/'scripts/check_lifetime.py')
        return script

    def ready(self):
        deadline = time.monotonic()+15
        while not (self.state/'ready').exists():
            if self.process and self.process.poll() is not None:
                self.fail((self.root/'controller.log').read_text())
            if time.monotonic() >= deadline:
                self.fail('private service startup timeout: '+(self.root/'controller.log').read_text())
            time.sleep(.01)
        output = (self.root/'controller.log').read_text()
        self.assertIn('fixture-live-stdout', output, 'gate output must stream before job completion')
        self.assertIn('fixture-live-stderr', output, 'gate errors must stream before job completion')
        self.unit = (self.state/'unit').read_text().strip()
        self.assertTrue(self.unit.startswith('vqro-check-') and self.unit.endswith('.service'))
        cgroup = (self.state/'cgroup').read_text().strip()
        self.assertTrue(cgroup.endswith('/'+self.unit), cgroup)
        self.assertEqual((self.state/'forwarded-receipt').read_text().strip(), self.env['CHECK_RECEIPT'])
        # The helper MainPID is an ancestor, not a leak. Only the already-detached leaf is reported.
        leaf = int((self.state/'leaf-pid').read_text())
        found = [int(line.split()[0]) for line in (self.state/'leftovers').read_text().splitlines()]
        self.assertEqual(found, [leaf])
        for name in ('gate-pid', 'supervisor-pid', 'forest-pid', 'leaf-pid'):
            self.fds.append(os.pidfd_open(int((self.state/name).read_text())))

    def launch(self, resistant=True):
        self.process = subprocess.Popen([str(self.stage(resistant)), 'quick'], cwd=self.repo, env=self.env,
                                        stdin=subprocess.DEVNULL, stdout=self.log, stderr=self.log, start_new_session=True)
        self.groups.append(self.process.pid)
        self.ready()

    def assert_stopped(self):
        for fd in self.fds:
            self.assertTrue(select.select([fd], [], [], 10)[0], 'owned job member survived teardown')
        events = Path('/sys/fs/cgroup'+(self.state/'cgroup').read_text().strip())/'cgroup.events'
        self.assertTrue(collected_or_empty_cgroup(events), 'owned cgroup is still populated')
        self.assertIsNone(self.sentinel.poll(), 'unrelated owned sentinel must not be touched')

    def tearDown(self):
        # Also reap launch shims/controllers if an assertion failed before a unit was created.
        for group in self.groups:
            try: os.killpg(group, signal.SIGKILL)
            except ProcessLookupError: pass
        if not self.unit and (self.state/'unit').exists(): self.unit = (self.state/'unit').read_text().strip()
        if self.unit:
            subprocess.run(['systemctl', '--user', 'stop', self.unit], env=self.control,
                           capture_output=True, timeout=10)  # Only this test's unique unit, never a pattern.
        for process in (self.process, self.driver, self.sentinel):
            if process:
                if process.poll() is None: process.terminate()
                try: process.wait(timeout=5)
                except subprocess.TimeoutExpired: process.kill(); process.wait(timeout=5)
        for fd in self.fds: os.close(fd)
        self.log.close()
        if (self.state/'run-dir').exists():
            run = Path((self.state/'run-dir').read_text().strip())
            if run.exists(): run.rmdir()  # Only this fixture's empty, recorded mktemp directory.
        self.temp.cleanup()

    def test_normal_exit_tears_down_detached_children(self):
        self.launch(resistant=False)
        (self.state/'finish-fifo').write_text('0\n')
        self.assertEqual(self.process.wait(timeout=10), 0, (self.root/'controller.log').read_text())
        self.assert_stopped()

    def test_term_int_hup_and_sigkill_of_controller_tears_down_its_job(self):
        # Separate tests are generated below so every case owns/reaps its own fixture.
        self.launch()
        self.process.send_signal(self.interrupt_signal)
        self.assertNotEqual(self.process.wait(timeout=10), 0)
        self.assert_stopped()

    def test_sigkill_of_actual_gate_is_also_native_job_completion(self):
        self.launch()
        signal.pidfd_send_signal(self.fds[0], signal.SIGKILL)
        self.assertNotEqual(self.process.wait(timeout=10), 0)
        self.assert_stopped()

    def test_parent_death_during_service_launch_never_starts_the_gate(self):
        script = self.stage()
        shimdir = self.root/'bin'; shimdir.mkdir()
        os.mkfifo(self.state/'launch-release')
        shim = shimdir/'systemd-run'
        shim.write_text('#!/bin/bash\nSTATE=' + shlex.quote(str(self.state)) + '''
for arg in "$@"; do
    case "$arg" in --unit=*) printf '%s\\n' "${arg#--unit=}" > "$STATE/unit" ;; esac
done
touch "$STATE/launch-paused"
IFS= read -r proceed < "$STATE/launch-release"
exec /usr/bin/systemd-run "$@"
''')
        shim.chmod(0o755)
        driver = self.root/'startup-driver.py'
        driver.write_text('''import pathlib, subprocess, sys
child = subprocess.Popen(sys.argv[1:])
pathlib.Path('controller-pid').write_text(str(child.pid))
raise SystemExit(child.wait())
''')
        self.driver = subprocess.Popen([sys.executable, '-I', str(driver), str(script), 'quick'], cwd=self.repo,
                                      env=dict(self.env, PATH=str(shimdir)+':/usr/bin:/bin'),
                                      stdin=subprocess.DEVNULL, stdout=self.log, stderr=self.log, start_new_session=True)
        self.groups.append(self.driver.pid)
        deadline = time.monotonic()+15
        while not (self.state/'launch-paused').exists():
            self.assertLess(time.monotonic(), deadline, (self.root/'controller.log').read_text())
            time.sleep(.01)
        fd = os.pidfd_open(int((self.repo/'controller-pid').read_text())); self.fds.append(fd)
        self.driver.kill(); self.driver.wait(timeout=5)
        (self.state/'launch-release').write_text('continue\n')
        self.assertTrue(select.select([fd], [], [], 10)[0], 'orphaned launcher survived startup rejection')
        self.assertFalse((self.state/'gate-pid').exists(), 'dead owner must be rejected before the gate starts')
        self.assertIsNone(self.sentinel.poll())

    def test_parent_death_stops_an_orphaned_controller_and_job(self):
        script = self.stage()
        driver = self.root/'driver.py'
        driver.write_text('''import pathlib, subprocess, sys
child = subprocess.Popen(sys.argv[1:])
pathlib.Path('controller-pid').write_text(str(child.pid))
raise SystemExit(child.wait())
''')
        self.driver = subprocess.Popen([sys.executable, '-I', str(driver), str(script), 'quick'], cwd=self.repo,
                                      env=self.env, stdin=subprocess.DEVNULL, stdout=self.log, stderr=self.log,
                                      start_new_session=True)
        self.groups.append(self.driver.pid)
        self.ready()
        controller = int((self.repo/'controller-pid').read_text())
        fd = os.pidfd_open(controller); self.fds.append(fd)
        self.driver.kill(); self.driver.wait(timeout=5)
        self.assert_stopped()


# Give each interruption a fresh fixture and a visible unittest result.
_interrupt = LifetimeServiceTests.test_term_int_hup_and_sigkill_of_controller_tears_down_its_job
del LifetimeServiceTests.test_term_int_hup_and_sigkill_of_controller_tears_down_its_job
for _name, _sig in (('term', signal.SIGTERM), ('int', signal.SIGINT), ('hup', signal.SIGHUP), ('kill', signal.SIGKILL)):
    def _case(self, sig=_sig):
        self.interrupt_signal = sig
        _interrupt(self)
    setattr(LifetimeServiceTests, 'test_controller_'+_name, _case)


if __name__ == '__main__': unittest.main()
