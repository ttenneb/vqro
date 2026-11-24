"""Gate outcome/receipt regressions: actual shell/Python blocks, private files, no backends."""
import sys
import unittest

if sys.platform == "win32":
    raise unittest.SkipTest("check.sh controls require POSIX shell, flock and executable-bit fixtures; native gate is check.ps1")

import fcntl
import hashlib
import json
import os
from pathlib import Path
import shlex
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
SOURCE = (ROOT / "scripts/check.sh").read_text()
RECEIPT_PROGRAM = SOURCE.rsplit("<<'PY'\n", 1)[1].rsplit("\nPY", 1)[0]
SCHEDULER = SOURCE[SOURCE.index("# Registration order"):SOURCE.index("sensitive()")]
MODEL = SOURCE[SOURCE.index("opentui_model() {"):SOURCE.index("\nstep opentui-model")]
RUNNER = SOURCE[SOURCE.index("# Registration order"):SOURCE.index("\nno_warnings()")]


def select_bash(path=None, initial='/bin/bash'):
    """Match check.sh: keep a suitable initial Bash, else first PATH Bash >= 4.4."""
    path = os.environ.get('PATH', '') if path is None else path
    candidates = [initial] + [str(Path(directory or '.')/'bash') for directory in path.split(os.pathsep)]
    for candidate in candidates:
        try:
            probe = subprocess.run([candidate, '-c',
                '(( BASH_VERSINFO[0] > 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] >= 4) ))'],
                capture_output=True, timeout=5)
            if probe.returncode == 0:
                return str(Path(candidate).resolve())
        except OSError:
            pass
    raise unittest.SkipTest('Bash 4.4+ required for gate shell fixtures')


# Resolve before fixtures narrow PATH to private fake tools or system-only directories.
BASH = select_bash()


def shell_function(name):
    start = SOURCE.index(name + "() {")
    return SOURCE[start:SOURCE.index("\n}", start) + 2]


class CheckTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="check-evidence-")
        self.root = Path(self.temporary.name)
        self.env = {"HOME": str(self.root), "PATH": str(Path(sys.executable).parent) + ":/usr/bin:/bin",
                    "PYTHONDONTWRITEBYTECODE": "1", "VQRO_PYTHON": sys.executable,
                    "PYTHON_BIN": str(Path(sys.executable).parent),
                    "CHECK_SYSTEM": "Linux"}  # Explicit fixture classification; never inferred by helpers.
        self.env["VQRO_GATE_SLOT_DIR"] = str(self.root / "slots")
        self.env["VQRO_GATE_SLOT_HELD"] = "slot-1.lock"
        self.env["VQRO_GATE_EXCLUSIVE"] = "0"  # Never inherit real caller admission facts.
        (self.root / "slots").mkdir()
        (self.root / "slots/slot-1.lock").touch()
        self.latest = self.root / "check-receipt.json"
        self.steps = self.root / "steps.tsv"

    def tearDown(self):
        self.temporary.cleanup()

    def receipt(self, statuses, stamp="20261120-000000-1", latest=None, provenance=()):
        self.steps.write_text("".join(f"step-{i}\t{status}\t0\t{self.root}/step-{i}.log\n"
                                      for i, status in enumerate(statuses)))
        return subprocess.run([sys.executable, "-", str(self.steps), str(latest or self.latest),
                               "quick", "fixture", str(self.root), "", "private fixture", stamp, *map(str, provenance)],
                              input=RECEIPT_PROGRAM, text=True, capture_output=True, env=self.env, umask=0o077)

    def gate(self, commands, mode="full", jobs=2, cleanup=""):
        """Actual scheduler -> atomic step results -> ordered table -> actual receipt/exit status.

        Only the supplied private commands run; never source the real gate's backend/cgroup setup.
        """
        body = '\n'.join(['set -u', 'CHECK_SYSTEM=Linux', 'SKIP_EXIT=77', f'MODE={shlex.quote(mode)}',
                          f'CHECK_JOBS={jobs}', f'REPO={shlex.quote(str(ROOT))}',
                          'LOGS=$1', 'STEPS=$LOGS/steps.tsv', ': > "$STEPS"',
                          RUNNER, shell_function('no_warnings'), commands, 'run_steps', cleanup,
                          'python3 - "$STEPS" "$LOGS/check-receipt.json" "$MODE" fixture "$LOGS" "${LEFT:-}" "${SCOPE:-fixture}" stamp <<\'RECEIPT\'',
                          RECEIPT_PROGRAM, 'RECEIPT'])
        logs = Path(tempfile.mkdtemp(dir=self.root))
        result = subprocess.run([BASH, '-c', body, 'gate-fixture', str(logs)],
                                capture_output=True, text=True, env=self.env, timeout=15)
        latest = logs/'check-receipt.json'
        receipt = json.loads(latest.read_text()) if latest.exists() else None
        return result, receipt, logs

    def scale_fixture(self, case, supplied='current', extra=0, mode='timing', cargo_success=False):
        root = self.root/case
        target, logs = root/'target with spaces', root/'logs'
        deps = target/'release/deps'
        deps.mkdir(parents=True)
        logs.mkdir()
        current, ran = deps/'scale-current', root/'ran'
        current.write_text('#!/bin/sh\nprintf current > ' + shlex.quote(str(ran)) +
                           '\nprintf \'{"failure_class":"pass"}\\n\' > "$VQRO_SCALE_RECEIPT"\n')
        current.chmod(0o700)
        for index in range(extra):
            stale = deps/f'scale-stale-{index}'
            stale.write_text('#!/bin/sh\nexit 49\n')
            stale.chmod(0o700)
        nonexec = deps/'scale-nonexec'
        nonexec.write_text('not executable')
        outside = root/'outside'
        outside.write_text('#!/bin/sh\nexit 48\n')
        outside.chmod(0o700)
        bin_dir = root/'bin'
        bin_dir.mkdir()
        cargo = bin_dir/'cargo'
        if cargo_success:
            messages = [dict(reason='compiler-artifact', manifest_path=str(ROOT/'Cargo.toml'),
                package_id='fixture-vqro', target=dict(name='scale', kind=['test'], src_path=str(ROOT/'tests/scale.rs')),
                profile=dict(test=True), executable=str(current), fresh=False), dict(reason='build-finished', success=True)]
            cargo.write_text('#!/bin/sh\nprintf \'%s\\n\' "$*" > ' + shlex.quote(str(root/'cargo-invoked')) +
                '\nprintf \'%s\\n\' ' + shlex.quote('\n'.join(map(json.dumps, messages))) + '\n')
        else:
            cargo.write_text('#!/bin/sh\nprintf forbidden > ' + shlex.quote(str(root/'cargo-invoked')) + '\nexit 81\n')
        cargo.chmod(0o700)
        path = {'current': current, 'nonexec': nonexec, 'outside': outside, 'missing': deps/'scale-absent'}.get(supplied)
        env = dict(self.env)
        if path is not None: env['VQRO_SCALE_TEST_EXE'] = str(path)
        body = '\n'.join(['set -u', f'MODE={mode}', f'REPO={shlex.quote(str(ROOT))}',
            f'TARGET={shlex.quote(str(target))}', f'LOGS={shlex.quote(str(logs))}', f'RUN={shlex.quote(str(root))}',
            f'CARGO=(env -i HOME="$HOME" PATH={shlex.quote(str(bin_dir))}:/usr/bin:/bin CARGO_TARGET_DIR="$TARGET")',
            shell_function('timing_python'), shell_function('scale_harness'), 'scale_harness'])
        result = subprocess.run([BASH, '-c', body], env=env, capture_output=True, text=True, timeout=10)
        return result, logs, target, current, ran

    def test_non_linux_clean_path_keeps_selected_bash_without_tmux(self):
        tools = self.root/'selected-bash'
        tools.mkdir()
        (tools/'bash').symlink_to(BASH)
        for name in ('env', 'dirname'):
            (tools/name).symlink_to('/usr/bin/' + name)
        arrays = SOURCE[SOURCE.index('CARGO=(env'):SOURCE.index('STEPS=$LOGS/steps.tsv')]
        body = '\n'.join(['set -u', 'CHECK_SYSTEM=Darwin', 'TARGET=$HOME/target',
                          'command -v tmux && exit 49', arrays,
                          '"${CLEAN[@]}" bash -c \'printf "%s\\n" "$BASH"\''])
        # A private directory holding only python3 keeps this about Bash selection; a bash beside the chosen Python
        # is covered by the shadow test below.
        python_dir = self.root/'selected-python'
        python_dir.mkdir()
        (python_dir/'python3').symlink_to(sys.executable)
        env = dict(self.env, PATH=str(tools),  # No tmux or system directories at discovery.
                   VQRO_PYTHON=str(python_dir/'python3'), PYTHON_BIN=str(python_dir))
        result = subprocess.run([str(tools/'bash'), '-c', body], env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(result.stdout.strip(), str(tools/'bash'))

    def test_non_linux_clean_path_selected_bash_wins_over_one_beside_python(self):
        tools = self.root/'selected-bash'
        tools.mkdir()
        (tools/'bash').symlink_to(BASH)
        for name in ('env', 'dirname'):
            (tools/name).symlink_to('/usr/bin/' + name)
        # The chosen Python's directory also holds an older bash (e.g. /usr/local/bin): it must not shadow the selection.
        python_dir = self.root/'python-with-old-bash'
        python_dir.mkdir()
        (python_dir/'python3').symlink_to(sys.executable)
        shadow = python_dir/'bash'
        shadow.write_text('#!/bin/sh\necho shadowed\n')
        shadow.chmod(0o700)
        private = self.root/'python-bin'
        private.mkdir()
        (private/'python3').symlink_to(sys.executable)
        arrays = SOURCE[SOURCE.index('CARGO=(env'):SOURCE.index('STEPS=$LOGS/steps.tsv')]
        for system, expected in (('Darwin', str(tools/'bash')), ('Linux', 'shadowed')):
            body = '\n'.join(['set -u', f'CHECK_SYSTEM={system}', 'TARGET=$HOME/target', arrays,
                              '"${CLEAN[@]}" bash -c \'printf "%s\\n" "$BASH"\''])
            env = dict(self.env, PATH=str(tools), VQRO_PYTHON=str(python_dir/'python3'), PYTHON_BIN=str(private))
            result = subprocess.run([str(tools/'bash'), '-c', body], env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, system + ': ' + result.stdout + result.stderr)
            # Linux keeps its pinned order (Python directories, then /usr/bin:/bin); only other systems put Bash first.
            self.assertEqual(result.stdout.strip(), expected, system)

    def test_zig_reaches_clean_build_arrays_and_windows_cross_check(self):
        fake = self.root/'bin'
        fake.mkdir()
        for name, body in [('rustup', 'echo x86_64-pc-windows-gnu'),
                           ('cargo', 'printf "%s\\n%s\\n%s\\n" "$ZIG" "$CARGO_TARGET_DIR" "$*" >> "$HOME/invocations"')]:
            path = fake/name
            path.write_text('#!/bin/sh\n' + body + '\n')
            path.chmod(0o700)
        env = dict(self.env, PATH=str(fake) + ':' + self.env['PATH'], ZIG='/fixture/compiler with spaces/zig')
        arrays = SOURCE[SOURCE.index('CARGO=(env'):SOURCE.index('STEPS=$LOGS/steps.tsv')]
        body = '\n'.join(['set -u', 'CHECK_SYSTEM=Linux', 'SKIP_EXIT=77', 'TARGET=$HOME/target', arrays,
                          '"${CARGO[@]}" cargo build', '"${CLEAN[@]}" /bin/sh -c \'test "$ZIG" = "/fixture/compiler with spaces/zig"\'',
                          shell_function('windows_check'), 'windows_check'])
        result = subprocess.run([BASH, '-c', body], env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        lines = (self.root/'invocations').read_text().splitlines()
        self.assertEqual(lines, ['/fixture/compiler with spaces/zig', str(self.root/'target'), 'build'] +
                         ['/fixture/compiler with spaces/zig', str(self.root/'target/windows'),
                          'check --target x86_64-pc-windows-gnu --all-targets'] * 2)

    def test_claude_launcher_platform_selection_preserves_linux_coverage(self):
        for name in ('e2e-claude-launcher', 'e2e-claude-restart'):
            for system, expected in [('Darwin', 'skip'), ('Linux', 'PASS')]:
                result, receipt, logs = self.gate('CHECK_SYSTEM=' + system + '\nstep build-zero-warnings true\nstep ' + name + ' bash -c \'echo linux-command-executed\'')
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(next(row['result'] for row in receipt['steps'] if row['name'] == name), expected)
                output = (logs/(name+'.log')).read_text()
                self.assertEqual('linux-command-executed' in output, system == 'Linux')

    def test_claude_launcher_non_linux_entrypoint_refuses_before_imports_or_effects(self):
        program = '''import sys,runpy
from unittest import mock
sys.platform='darwin'
sys.argv=['claude_launcher_e2e.py','--run']
sys.path.insert(0, TESTS)
with mock.patch('pathlib.Path.mkdir',side_effect=AssertionError('platform guard must precede local effects')), mock.patch('os.readlink',side_effect=AssertionError('platform guard must precede namespace probes')), mock.patch('subprocess.run',side_effect=AssertionError('platform guard must precede subprocesses')):
    runpy.run_path(ENTRY,run_name='__main__')
'''.replace('TESTS', repr(str(ROOT/'tests'))).replace('ENTRY', repr(str(ROOT/'tests/claude_launcher_e2e.py')))
        result = subprocess.run([sys.executable, '-c', program], env=self.env, capture_output=True, text=True, timeout=10)
        self.assertEqual(result.returncode, 77, result.stdout + result.stderr)
        self.assertIn('SKIP claude_launcher_e2e', result.stdout)

    def test_platform_skips_are_receipted_and_linux_commands_unchanged(self):
        commands = '''
step build-zero-warnings true
step opt-in:tmux bash -c 'exit 42'
step e2e-agent-stop bash -c 'exit 43'
step scale-harness bash -c 'exit 44'
step portable true
'''
        result, receipt, logs = self.gate('CHECK_SYSTEM=Darwin\n' + commands, mode='quick')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(receipt['skipped'], ['opt-in:tmux', 'e2e-agent-stop', 'scale-harness'])
        self.assertIn('Linux', (logs/'opt-in:tmux.log').read_text())
        result, receipt, _ = self.gate('CHECK_SYSTEM=Linux\n' + commands, mode='quick')
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(receipt['skipped'], [])
        self.assertIn('opt-in:tmux', receipt['failed'])

    def test_bash_resolver_rejects_43_and_selects_44_before_path_narrowing(self):
        old, modern = self.root/'old', self.root/'modern'
        old.mkdir(); modern.mkdir()
        (old/'bash').write_text('#!/bin/sh\nexit 1\n')  # Simulated 4.3 fails the 4.4 predicate.
        (modern/'bash').write_text('#!/bin/sh\nexit 0\n')
        (old/'bash').chmod(0o700); (modern/'bash').chmod(0o700)
        with self.assertRaises(unittest.SkipTest):
            select_bash(path=str(old), initial=str(old/'bash'))
        self.assertEqual(select_bash(path=str(old) + ':' + str(modern), initial=str(old/'bash')),
                         str((modern/'bash').resolve()))
        self.assertTrue(Path(BASH).is_absolute())
        result, _, _ = self.gate('step build-zero-warnings true')
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_failed_or_empty_uname_refused_before_work(self):
        fake = self.root/'uname-bin'
        fake.mkdir()
        uname = fake/'uname'
        for body in ('exit 1', 'exit 0'):
            uname.write_text('#!/bin/sh\n' + body + '\n')
            uname.chmod(0o700)
            env = dict(self.env, PATH=str(fake) + ':' + self.env['PATH'], CARGO_TARGET_DIR=str(self.root/'target'))
            result = subprocess.run([BASH, str(ROOT/'scripts/check.sh'), 'quick'],
                                    env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
            self.assertIn('cannot determine platform', result.stderr)
            self.assertFalse((self.root/'target').exists())

    def test_non_linux_timing_rejected_before_admission_or_steps(self):
        fake = self.root/'bin'
        fake.mkdir()
        uname = fake/'uname'
        uname.write_text('#!/bin/sh\necho Darwin\n')
        uname.chmod(0o700)
        env = dict(self.env, PATH=str(fake) + ':' + self.env['PATH'], CARGO_TARGET_DIR=str(self.root/'target'))
        result = subprocess.run([BASH, str(ROOT/'scripts/check.sh'), 'timing'],
                                env=env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        self.assertIn('timing mode requires Linux (/proc, pidfds)', result.stderr)
        self.assertFalse((self.root/'target').exists())
        self.assertNotIn('vqro check (', result.stdout)

    def test_non_linux_snapshot_and_leftovers_do_not_probe_proc(self):
        body = 'set -u\nCHECK_SYSTEM=Darwin\npython3() { exit 49; }\n' + shell_function('snapshot') + '\nsnapshot'
        result = subprocess.run([BASH, '-c', body], env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        classification = SOURCE[SOURCE.index('LEFT=${LEFT% }'):SOURCE.index('\nrm -rf "$RUN"')]
        result, receipt, _ = self.gate('CHECK_SYSTEM=Darwin\nstep build-zero-warnings true',
            cleanup='LEFT=\nCGROUP=\n' + classification)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(receipt['steps'][-1]['name'], 'no-leftover-processes')
        self.assertEqual(receipt['steps'][-1]['result'], 'skip')
        self.assertIn('/proc leftover scan requires Linux', result.stdout)

    def test_old_bash_bootstrap_reexecs_path_candidate_or_fails_clearly(self):
        bootstrap = SOURCE[:SOURCE.index('# Release gate.')]
        # Simulate the version values that Apple's Bash would expose. Candidate probes
        # remain untouched and must establish their own version before exec.
        bootstrap = bootstrap.replace('if (( BASH_VERSINFO[0] < 4 || (BASH_VERSINFO[0] == 4 && BASH_VERSINFO[1] < 4) )); then', 'if true; then', 1)
        script = self.root/'bootstrap'
        script.write_text(bootstrap + '\nexit 99\n')
        old = self.root/'bash43'
        old.mkdir()
        (old/'bash').write_text('#!/bin/sh\nexit 1\n')  # 4.3 fails the 4.4 version predicate.
        (old/'bash').chmod(0o700)
        env = dict(self.env, PATH=str(old))
        result = subprocess.run(['/bin/bash', str(script)], env=env, text=True, capture_output=True)
        self.assertEqual(result.returncode, 2)
        self.assertIn('Bash 4.4+', result.stderr)
        modern = self.root/'modern'
        modern.mkdir()
        candidate = modern/'bash'
        candidate.write_text('#!/bin/sh\nif [ "$1" = -c ]; then exit 0; fi\nprintf "%s\\n" "$@"\nexit 17\n')
        candidate.chmod(0o700)
        env['PATH'] = str(old) + ':' + str(modern)
        result = subprocess.run(['/bin/bash', str(script), 'quick', 'two words'], env=env, text=True, capture_output=True)
        self.assertEqual(result.returncode, 17)
        self.assertEqual(result.stdout.splitlines(), [str(script), 'quick', 'two words'])

    def test_scale_explicit_path_runs_current_amid_stale_binaries(self):
        result, logs, target, current, ran = self.scale_fixture('extras', extra=4)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(ran.read_text(), 'current')
        self.assertFalse((ran.parent/'cargo-invoked').exists(), 'timing mode must not invoke Cargo at all')
        selected = json.loads((logs/'scale-artifact.json').read_text())
        self.assertEqual(selected['path'], str(current.resolve()))
        self.assertEqual(selected['sha256'], hashlib.sha256(current.read_bytes()).hexdigest())
        self.assertEqual(selected['mtime_unix_ns'], current.stat().st_mtime_ns)

    def test_scale_explicit_path_missing_outside_or_non_executable_fails_without_execution(self):
        for supplied in (None, 'missing', 'outside', 'nonexec'):
            with self.subTest(supplied=supplied):
                result, logs, target, current, ran = self.scale_fixture(str(supplied), supplied=supplied)
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn('prebuild the release scale test and pass its path', result.stdout + result.stderr)
                self.assertFalse(ran.exists(), 'invalid/missing explicit path must never run any stale executable')
                self.assertFalse((ran.parent/'cargo-invoked').exists(), 'failure must not start a hidden build')

    def test_full_scale_explicit_prebuild_selects_and_runs_the_cargo_artifact(self):
        result, logs, target, current, ran = self.scale_fixture('full', supplied=None, extra=4,
                                                              mode='full', cargo_success=True)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(ran.read_text(), 'current')
        args = (ran.parent/'cargo-invoked').read_text()
        self.assertIn('--no-run --message-format=json', args)
        selection = json.loads((logs/'scale-artifact.json').read_text())
        self.assertEqual(Path(selection['path']).resolve(), current.resolve())
        self.assertEqual(selection['selection']['method'], 'cargo compiler-artifact')

    def test_failed_full_prebuild_never_runs_a_stale_binary(self):
        result, logs, target, current, ran = self.scale_fixture('failed-full', mode='full')
        self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
        self.assertFalse(ran.exists())
        self.assertFalse((logs/'scale-artifact.json').exists())

    def test_sampler_and_grader_receive_only_explicit_sanitized_environment(self):
        repo = self.root/'fake-repo'
        (repo/'scripts').mkdir(parents=True)
        (repo/'scripts/timing_contention.py').write_text('''import json,os,pathlib,sys
path=pathlib.Path(sys.argv[sys.argv.index('--output')+1])
with pathlib.Path(str(path)+'.env').open('a') as output:
    output.write(json.dumps(dict(os.environ))+'\\n')
if '--grade' not in sys.argv:
    path.write_text(json.dumps(dict(status='available',contended=False)))
else:
    sys.exit(0 if sys.argv[sys.argv.index('--grade')+1]=='0' else 1)
''')
        forbidden = dict(HERDR_TEST_SENTINEL='not-a-real-session', PYTHONPATH='/not-an-import-root',
                         PYTHONSTARTUP='/not-a-startup-file', TEST_CREDENTIAL='not-a-real-secret')
        self.env.update(forbidden)
        self.env['VQRO_GATE_EXCLUSIVE'] = '0'
        result, receipt, logs = self.gate(f'REPO={shlex.quote(str(repo))}\n'
            'step build-zero-warnings true\nstep scale-harness /bin/true')
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        observed = [json.loads(line) for line in (logs/'scale-harness-contention.json.env').read_text().splitlines()]
        self.assertEqual(len(observed), 2)
        for env in observed:
            self.assertFalse(set(env) & set(forbidden), env)
            self.assertEqual(env['HOME'], str(self.root))
            self.assertEqual(env['PATH'], f'{Path(sys.executable).parent}:{Path(sys.executable).parent}:/usr/bin:/bin')
            for key in ('VQRO_GATE_SLOT_DIR', 'VQRO_GATE_SLOT_HELD', 'VQRO_GATE_EXCLUSIVE'):
                self.assertEqual(env[key], self.env[key])

    def test_receipt_names_caller_attested_checkout_and_hashes_exact_artifacts(self):
        target = self.root/'build'
        release = target/'release'
        (release/'deps').mkdir(parents=True)
        binary, scale = release/'vqro', release/'deps/scale-fixture'
        binary.write_bytes(b'fake vqro artifact')
        scale.write_bytes(b'fake scale test artifact')
        scale.chmod(0o700)
        (release/'deps/scale-fixture.d').write_text('not an executable artifact')
        stale = release/'deps/scale-stale'
        stale.write_text('unselected stale executable')
        stale.chmod(0o700)
        (self.root/'scale-artifact.json').write_text(json.dumps(dict(kind='vqro.scale-test-artifact', path=str(scale))))
        result = self.receipt(['PASS'], provenance=(ROOT, target, 'a'*40))
        self.assertEqual(result.returncode, 0, result.stderr)
        attestation = json.loads(self.latest.read_text())['build_provenance']
        self.assertEqual(attestation['assurance'], 'caller-attested, not revision-bound')
        self.assertEqual(attestation['checkout'], str(ROOT.resolve()))
        self.assertEqual(attestation['checkout_commit'], 'a'*40)
        self.assertEqual(len(attestation['scale_test_artifacts']), 1)
        for row, path in [(attestation['release_vqro'], binary), (attestation['scale_test_artifacts'][0], scale)]:
            self.assertEqual(Path(row['path']).resolve(), path.resolve())
            self.assertEqual(row['sha256'], hashlib.sha256(path.read_bytes()).hexdigest())
            self.assertEqual(row['mtime_unix_ns'], path.stat().st_mtime_ns)

    def test_full_registers_all_testing_contracts_with_selected_target(self):
        registration = SOURCE[SOURCE.index('BEFORE=$(snapshot'):SOURCE.index('\nrun_steps\nsleep 2')]
        expected = ['e2e-testing-unit', 'e2e-testing-integration', 'e2e-testing-perf', 'e2e-testing-finish-gate']
        for mode in ('quick', 'full', 'timing'):
            body = '\n'.join(['set -u', f'MODE={mode}', 'RUN=fixture', 'TARGET="target with spaces"', 'LOGS=fixture',
                              'CARGO=()', 'CLEAN=()', 'snapshot() { :; }',
                              'step() { printf "%s\\t" "$@"; printf "\\n"; }', registration])
            result = subprocess.run([BASH, '-c', body], env=self.env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            rows = [line.rstrip('\t').split('\t') for line in result.stdout.splitlines() if line.startswith('e2e-testing-')]
            self.assertEqual([row[0] for row in rows], expected if mode == 'full' else [])
            for row in rows:
                self.assertEqual(row[1:3], ['CARGO_TARGET_DIR=target with spaces', 'python3'])
                self.assertEqual(row[3], 'tests/' + row[0].replace('-', '_') + '.py')

    def test_testing_perf_is_serial_and_exit_77_is_skip_not_pass(self):
        body = shell_function('sensitive') + '\n' + '\n'.join(
            f'if sensitive {name}; then echo serial:{name}; else echo independent:{name}; fi'
            for name in ('e2e-testing-unit', 'e2e-testing-integration', 'e2e-testing-perf', 'e2e-testing-finish-gate'))
        result = subprocess.run([BASH, '-c', body], env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), ['independent:e2e-testing-unit', 'independent:e2e-testing-integration',
                                                    'serial:e2e-testing-perf', 'independent:e2e-testing-finish-gate'])
        result, receipt, _ = self.gate("step build-zero-warnings true\nstep e2e-testing-perf bash -c 'exit 77'")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(receipt['steps'][-1]['result'], 'skip')
        self.assertEqual(receipt['skipped'], ['e2e-testing-perf'])

    def test_testing_contract_platform_skips_are_explicit_and_unverified(self):
        names = ('e2e-testing-unit', 'e2e-testing-integration', 'e2e-testing-perf', 'e2e-testing-finish-gate')
        commands = 'CHECK_SYSTEM=Darwin\nstep build-zero-warnings true\n' + '\n'.join(f'step {name} false' for name in names)
        result, receipt, _ = self.gate(commands)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(receipt['skipped'], list(names))
        self.assertTrue(all(step['result'] == 'skip' for step in receipt['steps'][1:]))

    def test_timing_mode_registers_only_the_three_budget_probes(self):
        registration = SOURCE[SOURCE.index('BEFORE=$(snapshot'):SOURCE.index('\nrun_steps\nsleep 2')]
        body = '\n'.join(['set -u', 'MODE=timing', 'RUN=fixture', 'TARGET=fixture', 'LOGS=fixture',
                          'CARGO=()', 'CLEAN=()', 'snapshot() { :; }',
                          'step() { printf "%s\\n" "$1"; }', registration])
        result = subprocess.run([BASH, '-c', body], env=self.env, capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([line for line in result.stdout.splitlines() if not line.startswith('vqro check')],
                         ['pi-bridge-memory', 'e2e-tui-scale-budgets', 'scale-harness'])

    def test_only_timing_budget_breaches_are_unverified_under_observed_contention(self):
        # The private step must stay alive until the REAL sampler has retained a
        # snapshot. A short sleep alone races interpreter startup/descheduling.
        repo = self.root/'observed-sampler'
        (repo/'scripts').mkdir(parents=True)
        (repo/'scripts/timing_contention.py').write_text(f'''from pathlib import Path
import runpy,sys
module=runpy.run_path({str(ROOT/'scripts/timing_contention.py')!r})
wait=module['select'].select
def observed_wait(read,write,error,timeout=None):
    if timeout is not None and timeout > 0:
        output=Path(sys.argv[sys.argv.index('--output')+1])
        (output.parent/'sample-retained').write_text('observed')
    return wait(read,write,error,timeout)
module['select'].select=observed_wait
raise SystemExit(module['main']())
''')
        workload = '''import pathlib,sys,time
ready=pathlib.Path(sys.argv[1]); end=time.monotonic()+10
while not ready.exists():
    if time.monotonic() >= end: raise RuntimeError('sampler did not retain a snapshot')
    time.sleep(.002)
start=time.monotonic(); time.sleep(.03)
sys.exit(78 if time.monotonic()-start > .001 else 0)
'''
        stub = shlex.quote(sys.executable) + ' -c ' + shlex.quote(workload) + ' "$LOGS/sample-retained"'
        with (self.root/'slots/quick-slot-1.lock').open('w') as other:
            fcntl.flock(other, fcntl.LOCK_EX)
            for mode, exclusive, expected in [('full', False, 'skip'), ('timing', False, 'FAIL'), ('full', True, 'FAIL')]:
                with self.subTest(mode=mode, exclusive=exclusive):
                    self.env['VQRO_GATE_EXCLUSIVE'] = str(int(exclusive))
                    prefix = '' if mode == 'timing' else 'step build-zero-warnings true\n'
                    result, receipt, _ = self.gate(f'REPO={shlex.quote(str(repo))}\n' + prefix + 'step scale-harness ' + stub, mode=mode)
                    step = receipt['steps'][-1]
                    self.assertEqual(step['result'], expected, result.stdout + result.stderr)
                    self.assertEqual(result.returncode, 0 if expected == 'skip' else 1)
                    self.assertEqual(step['contention']['status'], 'available')
                    self.assertIn('other_gate_slot_held', step['contention']['reasons'])
                    self.assertTrue(all(row['other_slots'] == ['quick-slot-1.lock']
                                        for row in step['contention']['samples']))
                    if expected == 'skip': self.assertEqual(step['reason'], 'unverified (contended)')

    def test_timing_passes_and_correctness_failures_are_not_downgraded(self):
        with (self.root/'slots/quick-slot-1.lock').open('w') as other:
            fcntl.flock(other, fcntl.LOCK_EX)
            for mode in ('full', 'timing'):
                for code, expected in [(0, 'PASS'), (1, 'FAIL')]:
                    with self.subTest(mode=mode, code=code):
                        prefix = '' if mode == 'timing' else 'step build-zero-warnings true\n'
                        command = "step scale-harness /bin/bash -c 'sleep .03; exit " + str(code) + "'"
                        result, receipt, _ = self.gate(prefix + command, mode=mode)
                        self.assertEqual(receipt['steps'][-1]['result'], expected, result.stderr)
                        self.assertEqual(result.returncode, int(code != 0))

    def test_missing_sampler_facts_never_pass_a_budget_breach(self):
        self.env.pop('VQRO_GATE_SLOT_DIR')
        for mode, expected in [('full', 'skip'), ('timing', 'FAIL')]:
            prefix = '' if mode == 'timing' else 'step build-zero-warnings true\n'
            result, receipt, _ = self.gate(prefix + "step scale-harness /bin/bash -c 'sleep .03; exit 78'", mode=mode)
            step = receipt['steps'][-1]
            self.assertEqual(step['result'], expected, result.stderr)
            self.assertEqual(step['contention']['status'], 'unavailable')
            self.assertNotEqual(step['result'], 'PASS')

    def test_failed_build_with_no_binary_does_not_start_dependents(self):
        for mode in ('quick', 'full'):
            with self.subTest(mode=mode):
                result, receipt, logs = self.gate('''
step licenses true
step build-zero-warnings test -x "$LOGS/missing-binary"
step e2e-dependent touch "$LOGS/dependent-ran"
step cargo-test touch "$LOGS/sensitive-ran"
''', mode=mode)
                self.assertEqual(result.returncode, 1, result.stderr)
                self.assertEqual(receipt['result'], 'fail')
                self.assertIn('build-zero-warnings', receipt['failed'])
                self.assertFalse((logs/'dependent-ran').exists(), 'missing build must stop dependent commands')
                self.assertFalse((logs/'sensitive-ran').exists(), 'missing build must stop timing probes too')
                self.assertEqual([s['name'] for s in receipt['steps']],
                                 ['licenses', 'build-zero-warnings', 'e2e-dependent', 'cargo-test'])
                for name in ('e2e-dependent', 'cargo-test'):
                    self.assertIn(name, receipt['failed'])
                    self.assertEqual((logs/f'{name}.log').read_text(), 'not run: build failed\n')

    def test_missing_build_registration_fails_the_outer_gate_with_an_ordered_census(self):
        cases = [
            ('step licenses touch "$LOGS/license-ran"\nstep dependent touch "$LOGS/dependent-ran"',
             ['licenses', 'dependent']),
            ('step licenses touch "$LOGS/license-ran"\nstep renamed-build touch "$LOGS/build-ran"\nstep dependent touch "$LOGS/dependent-ran"',
             ['licenses', 'renamed-build', 'dependent']),
            ('', ['build-zero-warnings']),
        ]
        for mode in ('quick', 'full'):
            for commands, expected in cases:
                with self.subTest(mode=mode, registered=expected):
                    result, receipt, logs = self.gate(commands, mode=mode)
                    self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                    self.assertEqual(receipt['result'], 'fail')
                    self.assertEqual(receipt['failed'], expected)
                    self.assertEqual([s['name'] for s in receipt['steps']], expected)
                    self.assertEqual(receipt['skipped'], [])
                    self.assertFalse(list(logs.glob('*-ran')), 'invalid registration must not run any command')
                    self.assertNotIn('PASS: 0/0', result.stdout)
                    for step in receipt['steps']:
                        self.assertEqual(step['result'], 'FAIL')
                        self.assertIn('not registered', Path(step['log']).read_text())

    def test_skipped_build_does_not_qualify_dependent_steps(self):
        result, receipt, logs = self.gate('''
step build-zero-warnings /bin/bash -c 'exit 77'
step dependent touch "$LOGS/dependent-ran"
''')
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(receipt['skipped'], ['build-zero-warnings'])
        self.assertEqual(receipt['failed'], ['dependent'])
        self.assertFalse((logs/'dependent-ran').exists())

    def test_missing_build_result_fails_closed_with_the_full_census(self):
        publish = shell_function('run_step').replace('run_step()', 'publish()', 1)
        commands = publish + '''
run_step() {
    if [ "${STEP_NAMES[$1]}" = "$BUILD_STEP" ]; then return; fi
    publish "$@"
}
step licenses true
step build-zero-warnings true
step dependent touch "$LOGS/dependent-ran"
'''
        result, receipt, logs = self.gate(commands)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(receipt['failed'], ['build-zero-warnings', 'dependent'])
        self.assertFalse((logs/'dependent-ran').exists())
        self.assertIn('build stopped before recording its result', (logs/'build-zero-warnings.log').read_text())

    def test_failing_command_fails_the_gate_and_receipt(self):
        result, receipt, logs = self.gate('''
step build-zero-warnings true
step broken /bin/bash -c 'echo fixture-failure; exit 7'
step healthy true
step cargo-test true
''')
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(receipt['result'], 'fail')
        self.assertEqual(receipt['failed'], ['broken'])
        self.assertIn('fixture-failure', (logs/'broken.log').read_text())
        self.assertEqual([s['name'] for s in receipt['steps']],
                         ['build-zero-warnings', 'broken', 'healthy', 'cargo-test'])

    def test_exit_77_through_the_scheduler_is_not_counted_as_passed(self):
        result, receipt, _ = self.gate('''
step build-zero-warnings true
step optional /bin/bash -c 'exit 77'
''')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(receipt['skipped'], ['optional'])
        self.assertEqual(receipt['steps'][1]['result'], 'skip')
        self.assertIn('PASS: 1/2, 1 skipped', result.stdout)
        self.assertIn('SKIP', result.stdout)

    def test_worker_killed_before_result_is_a_named_failure_not_a_missing_row(self):
        # This command only kills its own private run_step parent, never the test runner or a real service.
        result, receipt, logs = self.gate('''
step build-zero-warnings true
step crashed /bin/bash -c 'kill -KILL "$PPID"'
step healthy true
step cargo-test true
''')
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertEqual(receipt['failed'], ['crashed'])
        self.assertEqual([s['name'] for s in receipt['steps']],
                         ['build-zero-warnings', 'crashed', 'healthy', 'cargo-test'])
        self.assertIn('worker stopped before recording its result', (logs/'crashed.log').read_text())

    def test_zero_warning_build_rejects_warnings_and_compiler_errors(self):
        for output, code, expected in (('', 0, 'pass'), ('warning: fixture', 0, 'fail'), ('', 9, 'fail')):
            with self.subTest(output=output, code=code):
                commands = ("CARGO=()\ncargo() { printf '%s\\n' " + shlex.quote(output) +
                            f'; return {code}; }}\nstep build-zero-warnings no_warnings\n')
                result, receipt, _ = self.gate(commands)
                self.assertEqual(receipt['result'], expected, result.stderr)
                self.assertEqual(result.returncode, 0 if expected == 'pass' else 1)

    def test_leftover_classification_is_fail_only_when_scoped_and_never_kills_unscoped(self):
        classification = SOURCE[SOURCE.index('LEFT=${LEFT% }'):SOURCE.index('\nrm -rf "$RUN"')]
        for scoped, leaked, expected in ((False, False, 'UNSCOPED'), (False, True, 'UNSCOPED'),
                                         (True, False, 'PASS'), (True, True, 'FAIL')):
            with self.subTest(scoped=scoped, leaked=leaked):
                # Fixture PID is the owned shell itself, not a leak. Intercept kill: no real signal is sent.
                cleanup = '\n'.join(['CGROUP=' + ('fixture' if scoped else ''),
                                     'LEFT=' + ('$$' if leaked else ''), 'SCOPE=fixture',
                                     'kill() { printf "%s\\n" "$*" >> "$LOGS/kill-calls"; }', classification])
                result, receipt, logs = self.gate('step build-zero-warnings true', cleanup=cleanup)
                self.assertEqual(receipt['steps'][-1]['result'], expected, result.stderr)
                self.assertEqual(result.returncode, 1 if expected == 'FAIL' else 0)
                self.assertEqual((logs/'kill-calls').exists(), scoped and leaked)
                if scoped and leaked:
                    self.assertEqual((logs/'kill-calls').read_text().strip(), '-9 ' + receipt['leftover_pids'][0])
                if not scoped:
                    self.assertNotEqual(receipt['steps'][-1]['result'], 'PASS', 'UNSCOPED is not verified clean')

    def test_concurrency_bound_and_drained_serial_phase(self):
        worker = self.root/'worker.py'
        worker.write_text('''import fcntl, json, pathlib, sys, time
root, kind = pathlib.Path(sys.argv[1]), sys.argv[2]
def update(start):
    with (root/'work.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        path = root/'work.json'
        data = json.loads(path.read_text()) if path.exists() else dict(active=0, peak=0, done=0, sensitive=[])
        if kind == 'sensitive':
            assert data['active'] == 0 and data['done'] == 6, data
            data['sensitive'].append(sys.argv[3])
        else:
            data['active'] += 1 if start else -1
            data['peak'] = max(data['peak'], data['active'])
            if not start: data['done'] += 1
        path.write_text(json.dumps(data))
update(True)
if kind != 'sensitive':
    time.sleep(.05)  # Workload overlap, not a wall-time assertion: only counts/order are checked.
    update(False)
''')
        for mode in ('quick', 'full'):
            with self.subTest(mode=mode):
                command = shlex.quote(sys.executable) + ' ' + shlex.quote(str(worker)) + ' "$LOGS" '
                steps = ['step build-zero-warnings true']
                steps.extend(f'step worker-{i} {command}work' for i in range(6))
                sensitive = [f'step {name} {command}sensitive {name}' for name in ('cargo-test', 'history-budget')]
                if mode == 'full': steps[2:2] = sensitive  # Registration order must not choose phase order.
                else: steps.extend(sensitive)
                result, receipt, logs = self.gate('\n'.join(steps), mode=mode, jobs=2)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertEqual(receipt['failed'], [])
                data = json.loads((logs/'work.json').read_text())
                self.assertLessEqual(data['peak'], 2 if mode == 'full' else 1)
                self.assertEqual(data['done'], 6)
                self.assertEqual(data['sensitive'], ['cargo-test', 'history-budget'])

    def scheduled(self, names, mode="full"):
        """Run check.sh's own run_steps over fake steps: the build step marks the binaries ready, the others say
        whether they ran before or after it."""
        body = "\n".join(["set -u", f"MODE={mode}", "CHECK_JOBS=6", "LOGS=$1",
                          "STEP_NAMES=(" + " ".join(names) + ")", "show_ready(){ :; }",
                          'run_step(){ local i=$1 name=${STEP_NAMES[$1]}',
                          '  if [ "$name" = build-zero-warnings ]; then sleep 0.2; touch "$LOGS/built"; echo "BUILT"; '
                          '  elif [ -f "$LOGS/built" ]; then echo "AFTER $name"; else echo "BEFORE $name"; fi',
                          '  printf "%s\\tPASS\\t0\\t-\\n" "$name" > "$LOGS/step-$i.tsv"; }',
                          SOURCE[SOURCE.index("BUILD_STEP="):SOURCE.index("\n", SOURCE.index("BUILD_STEP="))],
                          shell_function("sensitive"), shell_function("run_steps"), "run_steps"])
        logs = Path(tempfile.mkdtemp(dir=self.root))
        r = subprocess.run([BASH, "-c", body, "sched", str(logs)], capture_output=True, text=True, env=self.env)
        return r.returncode, r.stdout.split("\n")

    def test_the_build_runs_before_every_concurrent_step_wherever_steps_are_inserted(self):
        # Steps registered before the build (licenses, vendor checks, ...) run serially with it; every later step,
        # concurrent or serial, sees the built binaries, whatever position the build has.
        for names in (["licenses", "manifest-deps", "build-zero-warnings", "e2e-a", "e2e-b", "cargo-test"],
                      ["licenses", "vendor-libghostty", "manifest-deps", "new-check", "build-zero-warnings", "e2e-a", "cargo-test"]):
            for mode in ("full", "quick"):
                rc, out = self.scheduled(names, mode)
                self.assertEqual(rc, 0, out)
                after = names[names.index("build-zero-warnings") + 1:]
                self.assertEqual(sorted(l for l in out if l.startswith("AFTER")), sorted(f"AFTER {n}" for n in after), (names, mode, out))
        rc, out = self.scheduled(["licenses", "e2e-a"])
        self.assertNotEqual(rc, 0, "a gate without the build step refuses to run")

    def test_exit_77_is_skip_not_pass_or_failure_in_the_table(self):
        body = f"set -u\nSKIP_EXIT=77\nLOGS={shlex.quote(str(self.root))}\nSTEPS=$LOGS/steps.tsv\n: > \"$STEPS\"\n"
        body += SCHEDULER
        body += "step passed bash -c 'exit 0'\nstep skipped bash -c 'exit 77'\nstep failed bash -c 'exit 7'\n"
        body += 'for i in 0 1 2; do run_step "$i"; show_ready; done\n'
        result = subprocess.run([BASH, "-c", body], text=True, capture_output=True, env=self.env)
        self.assertEqual(result.returncode, 0, result.stderr)
        rows = [line.split("\t") for line in self.steps.read_text().splitlines()]
        self.assertEqual([r[1] for r in rows], ["PASS", "SKIP", "FAIL"])
        self.assertIn("SKIP", result.stdout)

    def test_skip_is_non_failing_explicit_and_not_counted_as_passed(self):
        result = self.receipt(["PASS"] * 39 + ["SKIP", "SKIP"])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("PASS: 39/41, 2 skipped", result.stdout)
        receipt = json.loads(self.latest.read_text())
        self.assertEqual(receipt["result"], "pass")
        self.assertEqual(receipt["failed"], [])
        self.assertEqual(receipt["skipped"], ["step-39", "step-40"])
        self.assertEqual(receipt["steps"][-1]["result"], "skip")
        archive = self.root / "check-receipt-fixture-20261120-000000-1.json"
        self.assertEqual(archive.read_bytes(), self.latest.read_bytes())
        self.assertEqual(archive.stat().st_mode & 0o777, 0o400, "do not widen a restrictive umask")

    def test_real_failure_still_fails_beside_a_skip(self):
        result = self.receipt(["PASS", "SKIP", "FAIL"])
        self.assertEqual(result.returncode, 1)
        receipt = json.loads(self.latest.read_text())
        self.assertEqual(receipt["result"], "fail")
        self.assertEqual(receipt["failed"], ["step-2"])
        self.assertEqual(receipt["skipped"], ["step-1"])
        self.assertIn("FAIL: 1/3, 1 skipped", result.stdout)

    def test_latest_updates_but_each_archive_is_preserved_and_exclusive(self):
        self.assertEqual(self.receipt(["PASS"]).returncode, 0)
        first = self.root / "check-receipt-fixture-20261120-000000-1.json"
        original = first.read_bytes()
        second = self.receipt(["SKIP"], stamp="20261120-000000-2")
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertEqual(first.read_bytes(), original)
        self.assertNotEqual(self.latest.read_bytes(), original)
        latest = self.latest.read_bytes()
        collision = self.receipt(["FAIL"], stamp="20261120-000000-1")
        self.assertNotEqual(collision.returncode, 0)
        self.assertIn("FileExistsError", collision.stderr)
        self.assertEqual(first.read_bytes(), original)
        self.assertEqual(self.latest.read_bytes(), latest, "collision must not replace latest evidence")
        self.assertEqual(len(list(self.root.glob("check-receipt-fixture-*.json"))), 2)

    def test_same_second_runs_have_distinct_stamps(self):
        assignment = next(line for line in SOURCE.splitlines() if line.startswith("STAMP="))
        body = "date() { printf fixed; }\n" + assignment + '\nprintf "%s\\n" "$STAMP"\n'
        stamps = [subprocess.check_output([BASH, "-c", body], env=self.env, text=True).strip() for _ in range(2)]
        self.assertNotEqual(*stamps)

    def test_custom_latest_receipt_gets_an_immutable_sibling(self):
        folder = self.root / "custom"
        folder.mkdir()
        latest = folder / "accepted.json"
        result = self.receipt(["PASS"], latest=latest)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((folder / "check-receipt-fixture-20261120-000000-1.json").read_bytes(), latest.read_bytes())

    def test_absent_bun_skips_but_a_present_failing_bun_does_not(self):
        missing = 'command() { return 1; }\nSKIP_EXIT=77\n' + MODEL + "\nopentui_model\n"
        result = subprocess.run([BASH, "-c", missing], text=True, capture_output=True, env=self.env, cwd=ROOT)
        self.assertEqual(result.returncode, 77)
        self.assertIn("SKIP", result.stdout)
        bun = self.root / "bun"
        bun.write_text("#!/bin/sh\nexit 9\n")
        bun.chmod(0o755)
        present = f"command() {{ printf '%s\\n' {shlex.quote(str(bun))}; }}\nSKIP_EXIT=77\n" + MODEL + "\nopentui_model\n"
        result = subprocess.run([BASH, "-c", present], text=True, capture_output=True, env=self.env, cwd=ROOT)
        self.assertEqual(result.returncode, 9, "a genuine model-test failure must not become SKIP")

    def test_opentui_missing_dependencies_exit_77_before_starting_anything(self):
        staged = self.root / "tests/e2e_opentui.py"
        staged.parent.mkdir()
        staged.write_text((ROOT / "tests/e2e_opentui.py").read_text())
        result = subprocess.run([sys.executable, str(staged), str(self.root / "no-binaries")],
                                text=True, capture_output=True, env=self.env)
        self.assertEqual(result.returncode, 77, result.stderr)
        self.assertIn("SKIP OpenTUI", result.stdout)
        self.assertFalse((self.root / "no-binaries").exists())


if __name__ == "__main__":
    unittest.main()
