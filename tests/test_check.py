"""Gate outcome/receipt regressions: actual shell/Python blocks, private files, no backends."""
import json
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


class CheckTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="check-evidence-")
        self.root = Path(self.temporary.name)
        self.env = {"HOME": str(self.root), "PATH": "/usr/bin:/bin", "PYTHONDONTWRITEBYTECODE": "1"}
        self.latest = self.root / "check-receipt.json"
        self.steps = self.root / "steps.tsv"

    def tearDown(self):
        self.temporary.cleanup()

    def receipt(self, statuses, stamp="20261120-000000-1", latest=None):
        self.steps.write_text("".join(f"step-{i}\t{status}\t0\t{self.root}/step-{i}.log\n"
                                      for i, status in enumerate(statuses)))
        return subprocess.run([sys.executable, "-", str(self.steps), str(latest or self.latest),
                               "quick", "fixture", str(self.root), "", "private fixture", stamp],
                              input=RECEIPT_PROGRAM, text=True, capture_output=True, env=self.env, umask=0o077)

    def scheduled(self, names, mode="full"):
        """Run check.sh's own run_steps over fake steps: the build step marks the binaries ready, the others say
        whether they ran before or after it."""
        def function(name):
            start = SOURCE.index(name + "() {")
            return SOURCE[start:SOURCE.index("\n}", start) + 2]
        body = "\n".join(["set -u", f"MODE={mode}", "CHECK_JOBS=6", "LOGS=$1",
                          "STEP_NAMES=(" + " ".join(names) + ")", "show_ready(){ :; }",
                          'run_step(){ local i=$1 name=${STEP_NAMES[$1]}',
                          '  if [ "$name" = build-zero-warnings ]; then sleep 0.2; touch "$LOGS/built"; echo "BUILT"; '
                          '  elif [ -f "$LOGS/built" ]; then echo "AFTER $name"; else echo "BEFORE $name"; fi',
                          '  printf "%s\\tPASS\\t0\\t-\\n" "$name" > "$LOGS/step-$i.tsv"; }',
                          SOURCE[SOURCE.index("BUILD_STEP="):SOURCE.index("\n", SOURCE.index("BUILD_STEP="))],
                          function("sensitive"), function("run_steps"), "run_steps"])
        logs = Path(tempfile.mkdtemp(dir=self.root))
        r = subprocess.run(["/bin/bash", "-c", body, "sched", str(logs)], capture_output=True, text=True, env=self.env)
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
        result = subprocess.run(["bash", "-c", body], text=True, capture_output=True, env=self.env)
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
        stamps = [subprocess.check_output(["bash", "-c", body], env=self.env, text=True).strip() for _ in range(2)]
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
        result = subprocess.run(["bash", "-c", missing], text=True, capture_output=True, env=self.env, cwd=ROOT)
        self.assertEqual(result.returncode, 77)
        self.assertIn("SKIP", result.stdout)
        bun = self.root / "bun"
        bun.write_text("#!/bin/sh\nexit 9\n")
        bun.chmod(0o755)
        present = f"command() {{ printf '%s\\n' {shlex.quote(str(bun))}; }}\nSKIP_EXIT=77\n" + MODEL + "\nopentui_model\n"
        result = subprocess.run(["bash", "-c", present], text=True, capture_output=True, env=self.env, cwd=ROOT)
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
