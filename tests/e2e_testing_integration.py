#!/usr/bin/env python3
"""Independent slice-2 sandbox controls. Never connects to an installed/live core.

Run through gate-slot.sh (see the slice-2 brief). --source-only checks the fixture
source core without requiring the implementation. --cases selects independent
controls. Successful runs remove their scratch root; failures retain evidence.
"""

import sys
if __name__ == "__main__" and sys.platform != "linux":
    print("SKIP e2e_testing_integration: requires Linux /proc and namespace/process probes")
    raise SystemExit(77)

import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import subprocess
import sys
import tempfile
import time

from e2e_testing_unit import PrivateCore, wait_for
from python_interpreter import shell_fixture

REPO = Path(__file__).resolve().parents[1]


def worktree_state():
    result = subprocess.run(["git", "rev-parse", "HEAD"], cwd=REPO,
                            env={"HOME": os.environ["HOME"], "PATH": os.environ.get("PATH", "/usr/bin:/bin")},
                            capture_output=True, text=True, timeout=10)
    return {"head": result.stdout.strip(), "sha256": {
        name: hashlib.sha256((REPO / "extensions" / name).read_bytes()).hexdigest()
        for name in ("testing_integration.py", "vqro_testing.py")
        if (REPO / "extensions" / name).is_file()}}


INSPECT = '''import json, os
from pathlib import Path
from vqro_ext import Extension
ext = Extension(event_types=[])
parent = os.getppid()
proof = {"credential_absent": ext.value("doc/settings/x") is None,
         "ordinary_doc_present": ext.value("doc/settings/control") == {"visible": "yes"},
         "contribution_absent": ext.value("doc/ctl-owned") is None,
         "no_herdr": not any(k.startswith(("HERDR_", "TMUX")) for k in os.environ),
         "pid": os.getpid(), "ppid": parent, "pgid": os.getpgrp(),
         "parent_pgid": os.getpgid(parent), "sid": os.getsid(0),
         "state": os.environ["VQRO_STATE_DIR"], "socket": os.environ["VQRO_SOCKET"]}
with (Path(os.environ["VQRO_STATE_DIR"]) / "control-audit.jsonl").open("a") as output:
    output.write(json.dumps(proof) + "\\n")
ext.apply([{"op": "set", "key": "doc/ctl-proof", "value": proof}], "independent sandbox proof")
@ext.command("ctl.feature", "Independent feature control")
def feature(args):
    return {"value": "old"}
@ext.command("ctl.regression", "Independent regression control")
def regression(args):
    return {"value": "stable"}
ext.contribute({})
ext.run()
'''
CANDIDATE = INSPECT.replace('return {"value": "old"}', 'return {"value": "new"}')
REGRESSION = CANDIDATE.replace('return {"value": "stable"}', 'return {"value": "broken"}')
HEALTHY = 'from vqro_ext import Extension\next = Extension(event_types=[])\next.contribute({})\next.run()\n'
LINGER = '''import subprocess, sys
subprocess.Popen([sys.executable, "-c", "import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(600)"])
''' + HEALTHY
BIGVIEW = '''from vqro_ext import Extension
ext = Extension(event_types=[])
ext.contribute({"view/ctl-bigview.panel": {"kind": "panel", "title": "Oversized negative control"}})
ext.publish("ctl-bigview.panel", {"items": [{"text": "x" * (1024 * 1024 + 1024)}]})
ext.run()
'''


class Checks:
    def __init__(self):
        self.results = []

    def check(self, name, ok, actual=None):
        item = {"check": name, "ok": bool(ok)}
        if not ok:
            item["actual"] = actual
        self.results.append(item)

    def row(self, label, rows, ident, status):
        selected = [row for row in rows if row.get("id") == ident]
        self.check(label + ": " + ident + " = " + status,
                   len(selected) == 1 and selected[0].get("status") == status, selected)


class Source(PrivateCore):
    """Core #1 is deliberately safe: declarations are copied, never run here."""
    def start(self):
        self.log = (self.root / "core.log").open("w")
        self.proc = subprocess.Popen([str(self.binary), "serve", "--fake", "--safe"], env=self.env,
                                     cwd=self.root, stdout=self.log, stderr=subprocess.STDOUT,
                                     start_new_session=True)
        wait_for(lambda: self.sock.exists() and self.call("extension.list"), "source safe core", timeout=60)
        self.base = self.call("workspace.snapshot")
        self.controls = set()

    def fixture(self, name, script=HEALTHY, provides=(), **extra):
        path = self.ext / (name + ".py")
        path.write_text(script)
        manifest = {"name": name, "title": name, "version": "0.1.0", "license": "Apache-2.0",
                    "author": "Independent test", "summary": "Disposable oracle fixture",
                    "provides": list(provides)}
        (self.ext / "manifests" / (name + ".json")).write_text(json.dumps(manifest))
        self.controls.add(name)
        value = {"argv": ["python3", str(path)], "cwd": str(self.ext), "enabled": True}
        value.update(extra)
        return {"op": "set", "key": "extension/" + name, "value": value}

    def configure(self, fixtures, scenarios=()):
        # The source is our own core; changing its declarations between experiments
        # is deliberate. Snapshots surrounding each invocation must remain identical.
        ops = []
        for name in self.controls:
            ops.append({"op": "set", "key": "extension/" + name,
                        "value": {"argv": ["python3", str(self.ext / (name + ".py"))],
                                  "cwd": str(self.ext), "enabled": False}})
        ops.extend(fixtures)
        ops += [{"op": "set", "key": "doc/settings/x", "value": {"token": "private-control-never-copy"}},
                {"op": "set", "key": "doc/settings/control", "value": {"visible": "yes"}},
                {"op": "set", "key": "doc/ctl-owned", "value": {"owner": "ctl-ghost", "visible": True}}]
        # Only the final op per key is needed; workspace.apply rejects duplicate keys.
        dedup = {op["key"]: op for op in ops}
        self.call("workspace.apply", {"ops": list(dedup.values()), "note": "independent source fixtures"})
        folder = self.ext / "tests" / "ctl-inspect" / "scenarios"
        folder.mkdir(parents=True, exist_ok=True)
        for path in folder.glob("*.json"):
            path.unlink()
        for scenario in scenarios:
            (folder / (scenario["name"] + ".json")).write_text(json.dumps(scenario))


def all_rows(value):
    if isinstance(value, dict):
        for key, child in value.items():
            if key == "rows" and isinstance(child, list):
                yield from (row for row in child if isinstance(row, dict))
            else:
                yield from all_rows(child)
    elif isinstance(value, list):
        for child in value:
            yield from all_rows(child)


def root_paths(value):
    if isinstance(value, dict):
        for key, child in value.items():
            if key == "root" and isinstance(child, str):
                yield Path(child)
            else:
                yield from root_paths(child)
    elif isinstance(value, list):
        for child in value:
            yield from root_paths(child)


def matching(rows, text):
    return [row for row in rows if text in str(row.get("id", "")) or text in str(row.get("subject", ""))]


def process_group_members(group):
    found = []
    if not Path("/proc").is_dir():
        return found
    for directory in Path("/proc").iterdir():
        if directory.name.isdigit():
            try:
                if os.getpgid(int(directory.name)) == group:
                    status = (directory / "stat").read_text().rsplit(")", 1)[1].split()[0]
                    if status != "Z":
                        found.append(int(directory.name))
            except (ProcessLookupError, PermissionError, FileNotFoundError):
                pass
    return found


def validate_launcher(launcher, command):
    """Validate both layers of our literal shell_fixture, or legacy Python source."""
    source = launcher.read_text()
    header, _, body = source.partition("\n")
    interpreter = shlex.split(header[2:]) if header.startswith("#!") else []
    shell = interpreter[-1] if interpreter else ""
    if Path(shell).name in ("sh", "bash", "dash", "ksh"):
        command([shell, "-n", str(launcher)], timeout=10).check_returncode()
        words = shlex.split(body, comments=True)
        # shell_fixture deliberately emits one literal exec <python> -c <source>
        # "$@". Refuse an unfamiliar wrapper rather than silently miss a payload.
        if len(words) != 5 or words[0] != "exec" or words[2] != "-c" or words[4] != "$@":
            raise ValueError("unrecognized audit shell wrapper; embedded Python was not validated")
        compile(words[3], str(launcher) + ":embedded-python", "exec")
    else:
        compile(source, str(launcher), "exec")


class Driver:
    def __init__(self, root, source, checks, timeout):
        self.root, self.source, self.checks, self.timeout = root, source, checks, timeout
        self.index = 0
        self.groups = set()
        self.launch_log = root / "binary-launches.jsonl"
        self.launcher = root / "vqro-audit"
        # A transparent private exec shim records actual core launches independently
        # of what the implementation claims in its receipt. It always execs the
        # requested worktree build, never a binary from the installed setup.
        self.launcher.write_text(shell_fixture('''import json, os, sys
record = {"argv": sys.argv[1:], "pid": os.getpid(), "pgid": os.getpgrp(), "sid": os.getsid(0),
          "env": {k: v for k, v in os.environ.items() if k.startswith(("VQRO_", "HC_", "HERDR_", "XDG_", "TMUX"))}}
with open(''' + repr(str(self.launch_log)) + ''', "a") as output:
    output.write(json.dumps(record) + "\\n")
os.execv(''' + repr(str(source.binary)) + ''', [''' + repr(str(source.binary)) + ''', *sys.argv[1:]])
''', sys.executable))
        self.launcher.chmod(0o700)

    def launches(self):
        records = [json.loads(line) for line in self.launch_log.read_text().splitlines()] if self.launch_log.exists() else []
        for record in records:
            if "serve" in record["argv"] and record["pid"] == record["pgid"] == record["sid"]:
                self.groups.add(record["pgid"])
        return records

    def run(self, label, extra=(), keep=False, deep=False):
        self.index += 1
        sandbox = self.root / ("r" + str(self.index))
        if deep:
            sandbox = sandbox / ("too-deep-" * 20)
        before = self.source.snapshot()
        # Ambient endpoints deliberately name core #1. Integration must read it
        # as the source, never use it as its sandbox execution endpoint.
        env = dict(self.source.env, HERDR_PANE_ID="control-source-pane", TMUX="control-source-tmux")
        launch_count = len(self.launches())
        args = ["python3", str(self.source.ext / "testing_integration.py"), "--json", "--vqro",
                str(self.launcher), "--backend", "fake", "--root", str(sandbox)]
        if keep:
            args.append("--keep")
        args.extend(map(str, extra))
        started = time.monotonic()
        process = subprocess.run(args, env=env, cwd=self.source.root, capture_output=True,
                                 text=True, timeout=self.timeout)
        elapsed = time.monotonic() - started
        launches = [record for record in self.launches()[launch_count:] if "serve" in record["argv"]]
        self.checks.check(label + ": actual core launches audited", not launches if deep else bool(launches), launches)
        for launch in launches:
            self.checks.check(label + ": core in new session/group", launch["pid"] == launch["pgid"] == launch["sid"], launch)
            env = launch["env"]
            self.checks.check(label + ": no ambient pane/backend variables at exec", not any(
                key.startswith(("HERDR_", "TMUX")) for key in env), env)
            for key in ("VQRO_STATE_DIR", "VQRO_SOCKET", "VQRO_EXTENSIONS_DIR", "HC_SOCKET", "XDG_RUNTIME_DIR"):
                self.checks.check(label + ": exec environment private " + key,
                                  Path(env.get(key, "/")).is_relative_to(sandbox), env.get(key))
        (self.root / (label + ".stdout")).write_text(process.stdout)
        (self.root / (label + ".stderr")).write_text(process.stderr)
        try:
            receipt = json.loads(process.stdout)
        except ValueError:
            self.checks.check(label + ": receipt JSON only", False,
                              {"exit": process.returncode, "stdout": process.stdout[-3000:], "stderr": process.stderr[-3000:]})
            receipt = {}
        after = self.source.snapshot()
        self.checks.check(label + ": source seq unchanged", before[0]["seq"] == after[0]["seq"],
                          [before[0]["seq"], after[0]["seq"]])
        self.checks.check(label + ": source entries unchanged", before[0]["entries"] == after[0]["entries"])
        self.checks.check(label + ": source generations unchanged",
                          {n: e.get("generation") for n, e in before[1].items()} ==
                          {n: e.get("generation") for n, e in after[1].items()})
        self.checks.check(label + ": integration receipt schema", receipt.get("kind") == "vqro.test-receipt"
                          and receipt.get("version") == 1 and receipt.get("suite") == "integration", receipt)
        rows = list(all_rows(receipt))
        self.checks.check(label + ": source untouched row", any(row.get("id") == "live/untouched"
                          and row.get("status") == "pass" for row in rows), matching(rows, "live/untouched"))
        canary = receipt.get("sandbox", {}).get("canary", {})
        self.checks.check(label + ": canary receipt shape", isinstance(canary, dict)
                          and isinstance(canary.get("checked", {}).get("env"), list)
                          and isinstance(canary.get("checked", {}).get("paths"), dict)
                          and isinstance(canary.get("violations"), list), canary)
        if not deep:
            self.checks.check(label + ": canary passed", canary.get("status") == "pass"
                              and not canary.get("violations"), canary)
            self.checks.check(label + ": canary pass row", any(row.get("id") == "canary/sandbox"
                              and row.get("status") == "pass" for row in rows), matching(rows, "canary/sandbox"))
        for path in root_paths(receipt):
            self.checks.check(label + ": sandbox root private", path.is_relative_to(self.root), str(path))
            if not keep:
                self.checks.check(label + ": sandbox root removed", not path.exists(), str(path))
        self.checks.check(label + ": requested root cleaned", keep or not sandbox.exists(), str(sandbox))
        return process, receipt, rows, sandbox, elapsed

    def audit(self, label, sandbox):
        files = list(sandbox.rglob("control-audit.jsonl")) if sandbox.exists() else []
        self.checks.check(label + ": independent extension audit exists", bool(files), str(sandbox))
        for path in files:
            for line in path.read_text().splitlines():
                observed = json.loads(line)
                for key in ("credential_absent", "ordinary_doc_present", "contribution_absent", "no_herdr"):
                    self.checks.check(label + ": " + key, observed.get(key) is True, observed)
                grouped = observed["pgid"] == observed["parent_pgid"] == observed["ppid"] == observed["sid"]
                self.checks.check(label + ": extension in new core session/group", grouped, observed)
                self.groups.add(observed["pgid"])
                for key in ("state", "socket"):
                    self.checks.check(label + ": private " + key, Path(observed[key]).is_relative_to(sandbox), observed)
        # A stopped sandbox must not leave any of its process group alive, even --keep.
        for group in self.groups:
            self.checks.check(label + ": no surviving sandbox group " + str(group), not process_group_members(group),
                              process_group_members(group))


def isolation(driver, source, checks):
    marker = driver.root / "provider-must-not-start"
    external = driver.root / "outside.py"
    external.write_text('import os, sys\nsys.path.insert(0, os.environ["VQRO_EXTENSIONS_DIR"])\n' + HEALTHY)
    fixtures = [source.fixture("ctl-inspect", INSPECT, provides=("command/ctl.feature", "command/ctl.regression")),
                source.fixture("ctl-provider", "from pathlib import Path\nPath(" + repr(str(marker)) + ").write_text('unsafe provider ran')\n" + HEALTHY,
                               kind="provider"),
                source.fixture("ctl-external", argv=["python3", str(external)])]
    scenario = {"name": "credential-exclusion", "steps": [
        {"expect": {"entry": "doc/ctl-proof", "path": "credential_absent", "equals": True}},
        {"expect": {"entry": "doc/ctl-proof", "path": "ordinary_doc_present", "equals": True}},
        {"expect": {"entry": "doc/ctl-proof", "path": "contribution_absent", "equals": True}}]}
    source.configure(fixtures, [scenario])
    process, receipt, rows, sandbox, _ = driver.run("isolation", keep=True)
    checks.check("isolation: verdict pass", receipt.get("verdict") == "pass" and process.returncode == 0,
                 {"verdict": receipt.get("verdict"), "exit": process.returncode})
    checks.check("isolation: provider never started", not marker.exists())
    checks.check("isolation: scenario tree copied and executed", any(row.get("id") ==
                 "scenario/ctl-inspect/credential-exclusion" and row.get("status") == "pass" for row in rows),
                 matching(rows, "credential-exclusion"))
    excluded = receipt.get("sandbox", {}).get("excluded_keys", receipt.get("sandbox", {}).get("excluded", []))
    checks.check("isolation: receipt lists credential exclusion", "doc/settings/x" in json.dumps(excluded), excluded)
    external_rows = matching(rows, "ctl-external")
    checks.check("isolation: external declaration unverified", any(row.get("status") == "unverified" for row in external_rows), external_rows)
    # Direct retained-state proof independent of the suite's own assertions.
    snapshots = list(sandbox.rglob("workspace.json"))
    checks.check("isolation: retained sandbox state exists", bool(snapshots), str(sandbox))
    for state in snapshots:
        text = state.read_text()
        checks.check("isolation: credential value never persisted", "private-control-never-copy" not in text)
    driver.audit("isolation", sandbox)
    allowed_process, allowed_receipt, allowed_rows, _, _ = driver.run("external-opt-in", ["--allow-external"])
    checks.check("external-opt-in: external extension runs", any(row.get("id") == "start/ctl-external"
                 and row.get("status") == "pass" for row in allowed_rows), matching(allowed_rows, "ctl-external"))
    checks.check("external-opt-in: fake provider remains neutralized", not marker.exists())
    deep_process, deep_receipt, deep_rows, deep_root, _ = driver.run("canary-negative", deep=True)
    checks.check("canary-negative: error exit 2", deep_process.returncode == 2 and deep_receipt.get("verdict") == "error",
                 {"exit": deep_process.returncode, "receipt": deep_receipt})
    checks.check("canary-negative: sandbox.started false", deep_receipt.get("sandbox", {}).get("started") is False,
                 deep_receipt.get("sandbox"))
    checks.check("canary-negative: canary failed", deep_receipt.get("sandbox", {}).get("canary", {}).get("status") == "fail",
                 deep_receipt.get("sandbox"))
    checks.check("canary-negative: no core or extension audit started", not list(deep_root.rglob("workspace.json"))
                 and not list(deep_root.rglob("control-audit.jsonl")))
    # --keep preserves diagnostics on failure; main removes our root on success.


def smoke(driver, source, checks):
    source.configure([source.fixture("ctl-inspect", INSPECT),
                      source.fixture("ctl-crash", "raise SystemExit('intentional integration crash')\n"),
                      source.fixture("ctl-linger", LINGER),
                      source.fixture("ctl-command", provides=("command/ctl.never.registered",)),
                      source.fixture("ctl-bigview", BIGVIEW, provides=("view/ctl-bigview.panel",))])
    process, receipt, rows, sandbox, _ = driver.run("smoke-negative", keep=True)
    checks.check("smoke-negative: fail exit 1", receipt.get("verdict") == "fail" and process.returncode == 1,
                 {"exit": process.returncode, "verdict": receipt.get("verdict")})
    checks.check("smoke-negative: start/ctl-crash fails", any(row.get("id") == "start/ctl-crash" and row.get("status") == "fail" for row in rows), matching(rows, "ctl-crash"))
    checks.check("smoke-negative: commands/ctl-command fails", any(row.get("id") == "commands/ctl-command" and row.get("status") == "fail" for row in rows), matching(rows, "ctl-command"))
    lifetime = [row for row in rows if str(row.get("id", "")).startswith("lifetime/")]
    checks.check("smoke-negative: lifetime survivor reported as failure", any(row.get("status") == "fail"
                 and (row.get("id") == "lifetime/ctl-linger" or (row.get("id") == "lifetime/unattributed"
                      and "time.sleep(600)" in row.get("evidence", ""))) for row in lifetime), lifetime)
    checks.check("smoke-negative: oversize view fails or explicit core rejection", any(row.get("status") == "fail" and
                 (row.get("id") == "views/ctl-bigview" or any(term in json.dumps(row).lower()
                  for term in ("too_large", "too large", "1 mib", "1048576"))) for row in rows), matching(rows, "ctl-bigview"))
    driver.audit("smoke-negative", sandbox)


def comparison(driver, source, checks):
    candidate = driver.root / "candidate"
    candidate.mkdir()
    (candidate / "ctl-inspect.py").write_text(CANDIDATE)
    normal = source.fixture("ctl-inspect", INSPECT, provides=("command/ctl.feature", "command/ctl.regression"))
    weak = {"name": "weak-control", "new": True, "steps": [
        {"expect": {"entry": "doc/ctl-proof", "path": "ordinary_doc_present", "equals": True}}]}
    source.configure([normal], [weak])
    process, receipt, rows, _, _ = driver.run("weak", ["--candidate-ext", candidate])
    comparison_rows = matching(receipt.get("rows", []), "weak-control")
    checks.row("weak", comparison_rows, "compare/scenario/ctl-inspect/weak-control", "weak")
    checks.check("weak: verdict unverified and exit 77", process.returncode == 77 and receipt.get("verdict") == "unverified",
                 {"exit": process.returncode, "verdict": receipt.get("verdict")})
    feature = {"name": "feature-control", "new": True, "steps": [
        {"invoke": "ctl.feature", "args": {}, "result": {"value": "new"}}]}
    source.configure([normal], [feature])
    process, receipt, rows, _, _ = driver.run("feature", ["--candidate-ext", candidate])
    checks.check("feature: baseline failure and candidate pass yields pass", process.returncode == 0 and receipt.get("verdict") == "pass",
                 {"exit": process.returncode, "verdict": receipt.get("verdict")})
    checks.check("feature: comparison passes", any(row.get("status") == "pass" for row in matching(receipt.get("rows", []), "feature-control")), receipt.get("rows"))
    baseline = matching(receipt.get("baseline", {}).get("rows", []), "feature-control")
    candidate_rows = matching(receipt.get("candidate", {}).get("rows", []), "feature-control")
    checks.check("feature: baseline evidence fails", any(row.get("status") == "fail" for row in baseline), baseline)
    checks.check("feature: candidate evidence passes", any(row.get("status") == "pass" for row in candidate_rows), candidate_rows)
    (candidate / "ctl-inspect.py").write_text(REGRESSION)
    regression = {"name": "regression-control", "steps": [
        {"invoke": "ctl.regression", "args": {}, "result": {"value": "stable"}}]}
    source.configure([normal], [regression])
    process, receipt, rows, _, _ = driver.run("regression", ["--candidate-ext", candidate])
    checks.check("regression: candidate break fails", process.returncode == 1 and receipt.get("verdict") == "fail",
                 {"exit": process.returncode, "verdict": receipt.get("verdict")})
    checks.check("regression: comparison fails", any(row.get("status") == "fail" for row in matching(receipt.get("rows", []), "regression-control")), receipt.get("rows"))
    missing = {"name": "deadline-control", "steps": [
        {"apply": [{"op": "set", "key": "doc/ctl-wait", "value": "not-ready"}]},
        {"wait": {"entry": "doc/ctl-wait", "equals": "ready"}, "within_ms": 300}]}
    source.configure([normal], [missing])
    process, receipt, rows, _, elapsed = driver.run("deadline")
    failures = [row for row in matching(rows, "deadline-control") if row.get("status") == "fail"]
    checks.check("deadline: timeout fails with last observed value", process.returncode == 1 and
                 any("not-ready" in json.dumps(row) for row in failures), failures)
    checks.check("deadline: complete run bounded", elapsed < 180, elapsed)


def self_test(driver, source, checks):
    source.configure([source.fixture("ctl-inspect", INSPECT)])
    process, receipt, rows, _, _ = driver.run("self-test", ["--self-test"])
    for ident in ("self-test/crash", "self-test/zero"):
        controls = [row for row in rows if row.get("id") == ident]
        checks.check("self-test: " + ident + " negative rejected", bool(controls) and all(
            row.get("status") == "pass" and "rejected" in row.get("evidence", "").lower()
            for row in controls), controls)


def main():
    if sys.platform != "linux":
        print("SKIP e2e_testing_integration: cleanup evidence requires Linux /proc process-group probes")
        raise SystemExit(77)
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--vqro", type=Path, default=Path(os.environ.get("CARGO_TARGET_DIR") or REPO / "target") / "debug" / "vqro")
    parser.add_argument("--cases", default="isolation,smoke,comparison,self-test")
    parser.add_argument("--source-only", action="store_true")
    parser.add_argument("--timeout", type=int, default=600)
    args = parser.parse_args()
    checks = Checks()
    scratch = REPO / ".run"  # ignored scratch space (AGENTS.md)
    scratch.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="ti-", dir=scratch))
    source = None
    driver = None
    started = time.monotonic()
    state_before = worktree_state()
    executed = {}
    try:
        source = Source(root, args.vqro.resolve(), "s")
        executed = {name: hashlib.sha256((source.ext / name).read_bytes()).hexdigest()
                    for name in ("testing_integration.py", "vqro_testing.py") if (source.ext / name).is_file()}
        source.start()
        checks.check("source: safe mode active", source.call("workspace.snapshot")["safe_mode"] is True)
        source.configure([source.fixture("ctl-inspect", INSPECT)])
        checks.check("source: all controls stopped", all(not row.get("running") for row in source.call("extension.list")["extensions"].values()))
        driver = Driver(root, source, checks, args.timeout)
        for name, script in (("inspect", INSPECT), ("candidate", CANDIDATE), ("regression", REGRESSION),
                             ("linger", LINGER), ("bigview", BIGVIEW)):
            compile(script, "control-fixture-" + name, "exec")
        validate_launcher(driver.launcher, source.command)
        checks.check("source: fixture and launcher syntax valid", True)
        version = source.command([str(driver.launcher), "--version"])
        checks.check("source: audited exec shim reaches worktree binary", version.returncode == 0 and "vqro" in version.stdout,
                     {"exit": version.returncode, "stdout": version.stdout, "stderr": version.stderr})
        if not args.source_only:
            if not (REPO / "extensions/testing_integration.py").is_file():
                raise RuntimeError("implementation not present: extensions/testing_integration.py")
            runners = {"isolation": isolation, "smoke": smoke, "comparison": comparison, "self-test": self_test}
            for case in args.cases.split(","):
                try:
                    runners[case](driver, source, checks)
                except Exception as error:
                    checks.check(case + ": harness execution", False, repr(error))
    except Exception as error:
        checks.check("harness setup", False, repr(error))
    finally:
        if source is not None:
            source.stop()
    leftovers = []
    if driver:
        driver.launches()
        for group in driver.groups:
            members = process_group_members(group)
            if members:
                leftovers += members
                # Emergency cleanup only for positively observed owned sandbox groups.
                os.killpg(group, signal.SIGKILL)
    checks.check("cleanup: no sandbox survivors", not leftovers, leftovers)
    checks.check("cleanup: source core reaped", source is None or source.proc is None or source.proc.poll() is not None)
    failures = [item for item in checks.results if not item["ok"]]
    states = {"before": state_before, "after": worktree_state(), "executed_sha256": executed}
    (root / "worktree-state.json").write_text(json.dumps(states, indent=2))
    summary = {"ok": not failures, "checks": len(checks.results), "failures": failures,
               "elapsed_seconds": round(time.monotonic() - started, 3), "leftovers": leftovers,
               "worktree": states, "evidence_root": str(root) if failures else None}
    if not failures:
        shutil.rmtree(root)
    print(json.dumps(summary, indent=2))
    return int(bool(failures))


if __name__ == "__main__":
    sys.exit(main())
