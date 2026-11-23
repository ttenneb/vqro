#!/usr/bin/env python3
"""Disposable integration checks for a setup, never a live-workspace mutation."""
import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import threading
import time

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
from vqro_process import background_kwargs
from vqro_testing import (EXIT_CODES, RECEIPT_BYTES, STATUSES, PlainClient, Sandbox, canonical,
                          compare_runs, discover_scenarios, endpoints, fingerprint, print_result,
                          row, run_scenarios, smoke, source_mark, source_untouched, text, utc_now, write_receipt,
                          FAKE_BACKEND_COVERAGE)


STATUSES = STATUSES + ("weak",)


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    for flag in ("json", "offline", "allow-external", "keep", "self-test"):
        result.add_argument("--" + flag, action="store_true")
    for flag in ("socket", "state-dir", "extensions-dir", "vqro", "candidate-ext", "candidate-ops", "root", "out"):
        result.add_argument("--" + flag)
    result.add_argument("--backend", choices=("fake", "private"), default="fake")
    return result


def summarize(rows, counting_run, *, error=False):
    if error:
        return "error"
    if any(r["status"] == "fail" for r in rows):
        return "fail"
    if any(r["status"] == "weak" for r in rows) or not any(
            r["id"].startswith("start/") and r["status"] == "pass" for r in counting_run):
        return "unverified"
    return "pass"


def run_copy(sandbox, scenarios):
    rows = list(sandbox.audit_rows)
    error = False
    try:
        sandbox.start()
        smoke_rows = smoke(sandbox)
        rows += smoke_rows
        if any(r["id"] == "recovery/restart" and r["status"] == "pass" for r in smoke_rows):
            rows += run_scenarios(sandbox.client, scenarios)
        else:
            rows += [row("scenario", name, "fail", "not run: post-restart extension/command readiness failed") for name in sorted(scenarios)]
    except Exception as failure:
        rows.append(row("harness", "sandbox", "fail", f"{type(failure).__name__}: {failure}"))
        error = True
    finally:
        try:
            sandbox.stop()
        except Exception as failure:
            rows.append(row("harness", "cleanup", "fail", str(failure)))
            error = True
    # Restart and final shutdown can observe the same lifetime id; retain its worst outcome.
    lifetime = {}
    priority = {"pass": 0, "skip": 1, "unverified": 2, "warn": 3, "fail": 4}
    for item in sandbox.lifetime_rows:
        if item["id"] not in lifetime or priority[item["status"]] > priority[lifetime[item["id"]]["status"]]:
            lifetime[item["id"]] = item
    rows += list(lifetime.values())
    rows.append(row("leftovers", "sandbox", "fail" if sandbox.leftovers else "pass", sandbox.leftovers or "none"))
    return {"rows": rows, "sandbox": sandbox.info, "leftovers": sandbox.leftovers, "error": error,
            "not_covered": ([f"{r['id']}: {r['evidence']}" for r in rows if r["id"].startswith("views/") and "view family needs live resources" in r["evidence"]]
                            + [f"{r['id']}: {FAKE_BACKEND_COVERAGE}: {text(r['backend_unavailable'])}" for r in rows if r.get("backend_unavailable")]),
            "fingerprint": fingerprint(sandbox.ext, sandbox.declarations, sandbox.info.get("vqro") or {}),
            "verdict": summarize(rows, rows, error=error)}


def run(args):
    started, stamp = time.monotonic(), utc_now()
    socket, state, extensions = endpoints(args)
    live, hello, before = None, {}, None
    baseline = candidate = None
    sandboxes, rows, root, owned, error = [], [], None, False, False
    result = {"kind": "vqro.test-receipt", "version": 1, "suite": "integration", "source": "offline",
              "started_at": stamp, "fingerprint": {}, "inputs": [], "not_covered": [
                  "rendering", "performance: see test-perf", "agent profiles and real runtimes: offline follow-up not built"],
              "sandbox": {"started": False, "canary": {"status": "not run", "checked": {"env": [], "paths": {}}, "violations": []}}}
    try:
        if not args.offline:
            try:
                live = PlainClient(socket)
            except OSError:
                live = None
        if live:
            hello = live.call("hello", {"client": "test-integration"})
            snapshot, before = source_mark(live)
            result["source"] = "live"
            result["inputs"] += ["hello", "workspace.snapshot", "extension.list"]
        else:
            with (state / "workspace.json").open() as stream:
                snapshot = json.load(stream)
            result["inputs"].append(str(state / "workspace.json"))
        declarations = {key: entry["value"] for key, entry in snapshot["entries"].items() if key.startswith("extension/")}
        result["fingerprint"] = fingerprint(extensions, declarations, hello)
        result["inputs"].append(str(extensions))
        scenarios = discover_scenarios(extensions)
        if args.candidate_ext:
            scenarios.update(discover_scenarios(args.candidate_ext))
        operations = []
        if args.candidate_ops:
            with Path(args.candidate_ops).open() as stream:
                operations = json.load(stream)
            if isinstance(operations, dict):
                operations = operations["ops"]
        root = Path(args.root).resolve() if args.root else Path(tempfile.mkdtemp(prefix="vqi-"))
        if args.root and root.exists() and any(root.iterdir()):
            raise ValueError("run root must be absent or empty")
        root.mkdir(parents=True, exist_ok=True, mode=0o700)
        owned = True
        common = {"binary": args.vqro, "backend": args.backend, "allow_external": args.allow_external, "keep": args.keep}
        baseline_box = Sandbox(extensions, snapshot, root=root / "baseline", **common)
        sandboxes.append(baseline_box)
        if args.candidate_ext or args.candidate_ops:
            sandboxes.append(Sandbox(extensions, snapshot, root=root / "candidate", candidate_ext=args.candidate_ext,
                                     candidate_ops=operations, **common))
        # Audit and canary both copies before either core starts.
        for sandbox in sandboxes:
            result["sandbox"] = sandbox.info
            sandbox.prepare()
        rows.append(row("canary", "sandbox", "pass", "all copied runs passed environment and endpoint probes"))
        baseline = run_copy(baseline_box, scenarios)
        if len(sandboxes) > 1:
            candidate = run_copy(sandboxes[1], scenarios)
            rows += compare_runs(baseline, candidate, scenarios)
            error = baseline["error"] or candidate["error"]
            for section in (baseline, candidate):
                if section["error"]:
                    rows += [r for r in section["rows"] if r["id"].startswith("harness/")]
        else:
            rows += baseline["rows"]
            error = baseline["error"]
        installed = result["sandbox"].get("vqro") or {}
        if hello.get("version") and installed.get("version") != hello["version"]:
            rows.append(row("version", "sandbox", "warn", {"live": hello["version"], "sandbox": installed.get("version")}))
        if args.self_test:
            control_snapshot = {"entries": {"extension/crashing-control": {"value": {
                "argv": ["python3", "crashing_control.py"], "enabled": True}}}}
            control = Sandbox(extensions, control_snapshot, root=root / "control", **common)
            sandboxes.append(control)
            control.prepare()
            (control.ext / "crashing_control.py").write_text("raise RuntimeError('intentional crashing control')\n")
            control_result = run_copy(control, {})
            crash_failed = any(r["id"] == "start/crashing-control" and r["status"] == "fail" for r in control_result["rows"])
            zero_control = Sandbox(extensions, {"entries": {}}, root=root / "zero", **common)
            sandboxes.append(zero_control)
            zero_control.prepare()
            zero_result = run_copy(zero_control, {})
            zero_failed = zero_result["verdict"] == "fail" and any(r["id"] == "start/harness" and r["status"] == "fail" for r in zero_result["rows"])
            rows += [row("self-test", "crash", "pass" if crash_failed else "fail", "crashing control rejected" if crash_failed else control_result),
                     row("self-test", "zero", "pass" if zero_failed else "fail", "zero-row coverage rejected" if zero_failed else zero_result)]
    except Exception as failure:
        error = True
        if result["sandbox"].get("canary", {}).get("status") == "fail":
            rows.append(row("canary", "sandbox", "fail", str(failure)))
        rows.append(row("harness", "integration", "fail", f"{type(failure).__name__}: {failure}"))
    finally:
        if live:
            try:
                _, after = source_mark(live)
                rows.append(source_untouched(before, after))
            except Exception as failure:
                rows.append(row("live", "untouched", "unverified", str(failure)))
            live.close()
        for sandbox in sandboxes:
            try:
                sandbox.cleanup()
            except Exception as failure:
                error = True
                rows.append(row("harness", "cleanup", "fail", str(failure)))
        if owned and not args.keep and root and root.exists():
            shutil.rmtree(root)
    for section in (baseline, candidate):
        if section:
            result["not_covered"] += section.get("not_covered", [])
    for box in sandboxes:
        result["not_covered"] += [f"not copied: {item['key']}: {item['error']}" for item in box.info["not_copied"]]
    counting = (candidate or baseline or {}).get("rows", [])
    result.update(baseline=baseline, candidate=candidate, rows=rows,
                  verdict=summarize(rows, counting, error=error), elapsed_ms=round((time.monotonic() - started) * 1000),
                  counts={status: sum(r["status"] == status for r in rows) for status in STATUSES},
                  skipped=[{"id": r["id"], "why": r["evidence"]} for r in rows if r["status"] == "skip"],
                  leftovers=[p for box in sandboxes for p in box.leftovers])
    if len(json.dumps(result, ensure_ascii=False, indent=2).encode()) > RECEIPT_BYTES:
        result.update(verdict="error", baseline=None, candidate=None, fingerprint={}, inputs=[],
                      sandbox={key: value for key, value in result["sandbox"].items() if key in ("root", "backend", "started", "canary")},
                      rows=[row("harness", "receipt", "fail", "receipt exceeds size bound")],
                      not_covered=["complete report exceeded receipt bound; no complete result"],
                      counts={status: int(status == "fail") for status in STATUSES}, skipped=[], leftovers=result["leftovers"][:20])
    return result


def panel(result=None, destination=None):
    items = [{"type": "text", "text": "Run test.integration to check a disposable copy."}]
    if result:
        styles = {"pass": "ok", "fail": "error", "warn": "warn", "weak": "warn", "skip": "dim", "unverified": "dim"}
        items = [{"type": "text", "text": f"{result['verdict'].upper()} · {result['counts']}"},
                 {"type": "text", "text": f"Receipt: {destination}"}]
        items += [{"type": "text", "style": styles[r["status"]], "text": f"{r['id']}: {r['status']} · {r['evidence']}"}
                  for r in result["rows"][:380]]
        items += [{"type": "text", "style": "dim", "text": "Not covered: rendering, performance, agent profiles"}]
    return {"title": "Integration report", "items": items}


def extension_main():
    from vqro_ext import Extension
    ext = Extension("test-integration", event_types=[])
    lock = threading.Lock()
    last = {"process": None, "result": None, "receipt": None}

    @ext.command("test.integration", "Test a disposable setup copy (args: candidate_ext, candidate_ops, backend)")
    def start(args):
        with lock:
            if last["process"] is not None and last["process"].poll() is None:
                return {"started": False, "receipt": last["receipt"]}
            _, state, extensions = endpoints(parser().parse_args([]))
            state = Path(ext.info.get("state_dir") or state)
            directory = state / "test-integration"
            directory.mkdir(parents=True, exist_ok=True, mode=0o700)
            stamp = time.strftime("%Y%m%dT%H%M%S", time.gmtime()) + f"-{time.time_ns()}"
            destination = directory / f"receipt-{stamp}.json"
            root = directory / f"run-{time.time_ns():x}"
            command = [sys.executable, "-B", str(Path(__file__).resolve()), "--json", "--socket", ext.info["socket"],
                       "--state-dir", str(state), "--extensions-dir", str(extensions), "--root", str(root), "--out", str(destination)]
            for name in ("candidate_ext", "candidate_ops", "backend"):
                if args.get(name):
                    command.extend(["--" + name.replace("_", "-"), str(args[name])])
            environment = {"HOME": os.environ.get("HOME", str(Path.home())), "PATH": os.environ.get("PATH", os.defpath)}
            child = subprocess.Popen(command, env=environment, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                     stderr=subprocess.DEVNULL, start_new_session=True, **background_kwargs())
            last.update(process=child, receipt=str(destination))

            def finish():
                child.wait()
                try:
                    result = json.loads(destination.read_text())
                except (OSError, ValueError) as failure:
                    result = {"kind": "vqro.test-receipt", "version": 1, "suite": "integration", "verdict": "error",
                              "counts": {status: int(status == "fail") for status in STATUSES},
                              "rows": [row("harness", "subprocess", "fail", str(failure))],
                              "not_covered": ["subprocess ended without a complete receipt"]}
                    write_receipt(result, destination)
                with lock:
                    last["result"] = result
                    ext.publish("test-integration.report", panel(result, str(destination)))
            threading.Thread(target=finish, daemon=True, name="integration-report").start()
            return {"started": True, "receipt": str(destination)}

    @ext.command("test.integration.show", "Show the last integration report")
    def show(args):
        with lock:
            ext.publish("test-integration.report", panel(last["result"], last["receipt"]))
        return {"client": {"popup": {"view": "test-integration.report", "width": 120}}}

    ext.contribute({"view/test-integration.report": {"kind": "panel", **panel()}})
    ext.run()


def main(argv=None):
    args = parser().parse_args(argv)
    result = run(args)
    if args.out:
        try:
            write_receipt(result, args.out)
        except OSError as failure:
            result["verdict"] = "error"
            result["rows"].append(row("harness", "output", "fail", str(failure)))
            result["counts"]["fail"] += 1
    print_result(result, args.json)
    return EXIT_CODES[result["verdict"]]


if __name__ == "__main__":
    if os.environ.get("VQRO_EXTENSION_LIFETIME") == "stdin":
        extension_main()
    else:
        sys.exit(main())
