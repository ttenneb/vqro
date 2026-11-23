#!/usr/bin/env python3
"""On-demand, read-only installed-setup health report; also usable as an offline CLI."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time

# CLI reporting must not write __pycache__ into the installed extensions directory.
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
from vqro_process import background_kwargs
from vqro_testing import (EXIT_CODES, cli_parser, dependency_row, endpoints, fingerprint, is_settings_key, is_setup_key, print_result, receipt,
                          resolve_program, row, save_receipt, script_path, text, utc_now, write_receipt)
from testing_setup import SetupStore, POLICY_KEY, RPC_RULE_NOTICE, rpc_rule_notice, read_snapshot, scan as setup_scan, stamp_receipt

LOG_BYTES = 64 * 1024
TEST_TIMEOUT = 120
OUTPUT_BYTES = 4096
NOT_COVERED = "behaviour: unit report reads status only; see test-integration (behaviour) and test-perf (performance)"


def declarations_in(snapshot):
    return {key: entry["value"] for key, entry in snapshot.get("entries", {}).items()
            if key.startswith("extension/")}


def offline_facts(declarations, extensions_dir):
    result = {}
    for key, declaration in declarations.items():
        name = key.removeprefix("extension/")
        facts = {"enabled": declaration.get("enabled", True), "display_status": "offline",
                 "manifest": None, "dependencies": None}
        path = Path(declaration.get("manifest") or f"manifests/{name}.json")
        if not path.is_absolute():
            path = Path(extensions_dir) / path
        try:
            with path.open("rb") as stream:
                data = stream.read(32 * 1024 + 1)
            if len(data) > 32 * 1024:
                raise ValueError("manifest exceeds 32 KiB")
            manifest = json.loads(data)
            if not isinstance(manifest, dict) or manifest.get("name") != name:
                raise ValueError("manifest name does not match declaration")
            facts["manifest"] = manifest
        except FileNotFoundError:
            pass
        except (OSError, ValueError) as error:
            facts["manifest_error"] = str(error)
        result[name] = facts
    return result


def status_row(name, facts):
    status = facts.get("display_status", "unknown")
    if facts.get("enabled") is False or status == "off":
        return row("status", name, "skip", "off")
    if status == "stopped-by-safe-mode":
        return row("status", name, "skip", status)
    if status == "running":
        crashes = facts.get("crashes", 0)
        return row("status", name, "warn" if crashes else "pass", f"running, {crashes} crashes")
    if status in ("failed", "provider-not-connected"):
        return row("status", name, "fail", {"status": status, "failure": facts.get("failure"),
                                          "last_exits": facts.get("last_exits")})
    return row("status", name, "unverified", status)


def dev_tree(path, extensions_dir):
    path = Path(path)
    if not path.is_absolute() or path.is_relative_to(Path(extensions_dir).resolve()):
        return False
    return ".worktrees" in path.parts or any(
        (parent / ".git").exists() and (parent / "extensions/vqro_ext.py").is_file()
        for parent in (path, *path.parents))


def argv_row(name, declaration, extensions_dir, *, binary=None, path=None):
    argv = declaration.get("argv") or []
    if not argv:
        return row("argv", name, "fail", "missing argv")
    cwd = Path(declaration.get("cwd") or extensions_dir)
    program = resolve_program(argv[0], cwd, binary=binary, path=path)
    if program is None:
        return row("argv", name, "fail", f"missing program: {argv[0]}")
    if not cwd.is_dir():
        return row("argv", name, "fail", f"missing cwd: {cwd}")
    script = script_path(declaration, extensions_dir)
    if script is not None and not script.is_file():
        return row("argv", name, "fail", f"missing script: {script}")
    candidates = [program, cwd, script]
    if declaration.get("manifest"):
        candidates.append(Path(declaration["manifest"]))
    external = [str(p) for p in candidates if p is not None and dev_tree(p, extensions_dir)]
    if external:
        return row("argv", name, "warn", "works only from a dev tree? " + ", ".join(external))
    return row("argv", name, "pass", f"{program}" + (f" {script}" if script else ""))


def log_row(name, state_dir):
    path = Path(state_dir) / f"ext-{name}.log"
    try:
        with path.open("rb") as stream:
            stream.seek(0, os.SEEK_END)
            stream.seek(max(0, stream.tell() - LOG_BYTES))
            data = stream.read(LOG_BYTES)
    except FileNotFoundError:
        return row("log", name, "skip", "no log")
    matches = [line for line in data.decode("utf-8", "replace").splitlines()
               if "Traceback" in line or "panicked" in line]
    return row("log", name, "warn" if matches else "pass", matches[-1] if matches else "no traceback or panic in last 64 KiB")


def tests_row(name, extensions_dir, *, enabled=True, timeout=TEST_TIMEOUT):
    directory = Path(extensions_dir) / "tests" / name
    if not enabled:
        return row("tests", name, "skip", "tests disabled by caller")
    if not directory.is_dir() or not any(directory.glob("test_*.py")):
        return row("tests", name, "skip", "no tests installed: no coverage")
    if timeout <= 0:
        return row("tests", name, "unverified", "not run: command time budget; run the CLI for full tests")
    # The environment is cooperative isolation, not a security sandbox for arbitrary test code.
    with tempfile.TemporaryDirectory(prefix="vqro-unit-") as home, tempfile.TemporaryFile() as output:
        try:
            completed = subprocess.run([sys.executable, "-B", "-m", "unittest", "discover", "-s", str(directory.resolve()),
                                        "-p", "test_*.py"], cwd=directory,
                                       env={"PATH": os.environ.get("PATH", os.defpath), "HOME": home,
                                            "PYTHONIOENCODING": "utf-8", "VQRO_TEST_EXTENSIONS_DIR": str(Path(extensions_dir).resolve())},
                                       stdout=output, stderr=subprocess.STDOUT, timeout=timeout, check=False, **background_kwargs())
            status = "pass" if completed.returncode == 0 else "fail"
            evidence = f"exit {completed.returncode}"
        except subprocess.TimeoutExpired:
            status, evidence = "fail", f"timeout after {timeout} s"
        output.seek(0, os.SEEK_END)
        output.seek(max(0, output.tell() - OUTPUT_BYTES))
        tail = output.read(OUTPUT_BYTES).decode("utf-8", "replace")
    return row("tests", name, status, evidence + ": " + tail[-250:])


def collect(state_dir, extensions_dir, *, client=None, hello=None, offline=False, tests=True,
            strict=False, timeout=TEST_TIMEOUT, connection_error=None, tests_budget=None, record=False, endpoint=None):
    started, started_at = time.monotonic(), utc_now()
    rows, inputs, not_covered = [], [], [NOT_COVERED]
    tracker, context, harness_error = SetupStore(state_dir), None, False
    recording_error, recording_active = None, False
    source = "live" if client is not None and not offline else "offline"
    declarations, fp, snapshot = {}, {}, {}
    rows.append(row("core", "reachable", "pass" if source == "live" else "warn",
                    "live" if source == "live" else "offline: " + (connection_error or "requested file report")))
    try:
        if source == "live":
            inputs.extend(["hello", "extension.list", "workspace.snapshot"])
            hello = hello if hello is not None else client.call("hello", {"client": "test-unit"})
            facts = client.call("extension.list")["extensions"]
            snapshot = client.call("workspace.snapshot")
            declarations = declarations_in(snapshot)
        else:
            path = Path(state_dir) / "workspace.json"
            inputs.append(str(path))
            with path.open() as stream:
                snapshot = json.load(stream)
            declarations = declarations_in(snapshot)
            facts = offline_facts(declarations, extensions_dir)
            hello = {}
        scope_notice = rpc_rule_notice(snapshot)
        if scope_notice:
            rows.append(row("coverage", "rpc-pi-rule", "unverified", scope_notice))
            not_covered.append(scope_notice)
        if hello.get("safe_mode") or snapshot.get("safe_mode") or any(f.get("safe_mode") for f in facts.values()):
            rows.append(row("core", "safe_mode", "warn", "core is in safe mode"))
        info = dict(hello or {}, epoch=snapshot.get("epoch", (hello or {}).get("epoch")))
        fp = fingerprint(extensions_dir, declarations, info,
                         settings={key: entry["value"] for key, entry in snapshot.get("entries", {}).items() if is_settings_key(key)},
                         state_dir=state_dir, endpoint=endpoint)
        context = {"setup_id": fp.get("setup_id"), "generation": 0, "instance": fp["instance"], "setup_observer": None}
        if record:
            try:
                context = tracker.begin(tracker.observe(fp, snapshot))
                recording_active = True
            except Exception as failure:
                # Recorder/observer portability is separate from ordinary health checks.
                recording_error = text(f"recording unsupported or unavailable: {failure}")
                try:
                    tracker.invalidate("latest recorded Unit attempt could not be established; old acknowledgement invalidated")
                except Exception:
                    pass  # The in-app observer latches this failure until invalidation can succeed.
                rows.append(row("recording", "completion", "unverified", recording_error))
                not_covered.append(recording_error + "; finish evidence NOT VERIFIED")
        started_at = utc_now()
        inputs.append(str(extensions_dir))
        for key, declaration in sorted(declarations.items()):
            name = key.removeprefix("extension/")
            observed = facts.get(name, {})
            rows.append(status_row(name, observed))
            if observed.get("manifest_error"):
                rows.append(row("manifest", name, "warn", observed["manifest_error"]))
            elif not observed.get("manifest"):
                rows.append(row("manifest", name, "warn", "no manifest: no description"))
            else:
                rows.append(row("manifest", name, "pass", "manifest available"))
            argv = argv_row(name, declaration, extensions_dir)
            if observed.get("enabled") is False:
                if argv["status"] == "fail":
                    argv["status"] = "warn"
                rows.append(argv)
                not_covered.append(f"{name}: off; runtime, logs and tests not checked")
                continue
            rows.extend([dependency_row(name, observed), argv, log_row(name, state_dir)])
            inputs.append(str(Path(state_dir) / f"ext-{name}.log"))
            test_started = time.monotonic()
            test_timeout = timeout if tests_budget is None else min(timeout, max(0, tests_budget))
            test = tests_row(name, extensions_dir, enabled=tests, timeout=test_timeout)
            if test_timeout < timeout and test["status"] == "fail" and test["evidence"].startswith("timeout"):
                # Cut short by the shared command budget, not by its own limit: no verdict either way.
                test = row("tests", name, "unverified", "not finished: command time budget; run the CLI for full tests")
            if tests_budget is not None:
                tests_budget -= time.monotonic() - test_started
            rows.append(test)
            inputs.append(str(Path(extensions_dir) / "tests" / name))
            if test["status"] in ("skip", "unverified"):
                not_covered.append(f"{name}: {test['evidence']}")
    except Exception as error:
        rows.append(row("error", "harness", "fail", f"{type(error).__name__}: {error}"))
        harness_error = True
    if record and not tests:
        rows.append(row("tests", "policy", "unverified", "installed tests were disabled by caller; not completion evidence"))
        not_covered.append("recorded Unit verification requires installed tests to be enabled")
    result = receipt(rows, source=source, started_at=started_at, started=started, fingerprint_value=fp,
                     inputs=inputs, not_covered=not_covered, extension_count=len(declarations), strict=strict, error=harness_error)
    result["health_verdict"] = result["verdict"]
    result["recorded"] = recording_active
    result["record_requested"] = bool(record)
    result["recording"] = {"state": "unsupported" if recording_error else "active" if recording_active else "not-requested", "reason": recording_error}
    result["tests_enabled"] = bool(tests)
    if (recording_error or record and not tests) and result["verdict"] == "pass":
        result["verdict"] = "unverified"
    if context is not None:
        try:
            after_snapshot, after_fp = setup_scan(client if source == "live" else None, hello, state_dir, extensions_dir, endpoint)
            after = tracker.context(tracker.observe(after_fp, after_snapshot)) if recording_active else {
                "setup_id": after_fp.get("setup_id"), "generation": 0, "instance": after_fp["instance"], "setup_observer": None}
            matched = stamp_receipt(result, context, after)
            if not matched:
                result["rows"].append(row("setup", "closure", "unverified", "setup changed during the run or an input is unknown; no current verification"))
                result["counts"]["unverified"] += 1
                if context.get("setup_id") != after.get("setup_id") or context["generation"] != after["generation"] or result["verdict"] == "pass":
                    result["verdict"] = "unverified"
            if recording_active:
                result["receipt"] = context["receipt"]
        except Exception as failure:
            result.update(setup_id=context.get("setup_id"), generation=context["generation"], instance=context.get("instance"), closure={"matched": False})
            result["rows"].append(row("setup", "closure", "unverified", f"could not close verification: {failure}"))
            result["counts"]["unverified"] += 1
            result["verdict"] = "unverified" if result["verdict"] == "pass" else result["verdict"]
            if recording_active:
                result["receipt"] = context["receipt"]
    else:
        result.update(setup_id=None, generation=0, instance=fp.get("instance"), closure={"matched": False})
    return result


def panel(result=None, destination=None):
    if result is None:
        return {"title": "Unit health report", "items": [{"type": "text", "text": "Run test.unit to check this setup."}]}
    style = {"pass": "ok", "warn": "warn", "fail": "error", "skip": "dim", "unverified": "dim"}
    items = [{"type": "text", "text": f"{result['verdict'].upper()} · {result['counts']}"},
             {"type": "text", "text": f"Receipt: {destination or '(not written)'}"}]
    items += [{"type": "text", "style": style[r["status"]],
               "text": f"{r['status']} {r['id']}: {r['evidence']}"} for r in result["rows"][:375]]
    if len(result["rows"]) > 375:
        items.append({"type": "text", "style": "dim", "text": "More rows in the receipt."})
    items.append({"type": "text", "text": "Not covered", "style": "warn"})
    items += [{"type": "text", "style": "dim", "text": text(gap)} for gap in result["not_covered"][:20]]
    return {"title": "Unit health report", "items": items}


def error_status(reason, snapshot=None):
    # Only this call's successfully read snapshot can establish policy, never cached state.
    policy = None
    if isinstance(snapshot, dict) and isinstance(snapshot.get("entries"), dict):
        entry = snapshot["entries"].get(POLICY_KEY, {"value": False})
        if isinstance(entry, dict) and type(entry.get("value")) is bool:
            policy = entry["value"]
    reasons = [text(reason)]
    try:
        scope_notice = rpc_rule_notice(snapshot) if isinstance(snapshot, dict) else None
    except (AttributeError, TypeError):
        scope_notice = None  # Malformed auxiliary metadata cannot erase a separately valid policy read.
    if scope_notice:
        reasons.insert(0, scope_notice)
    return {"kind": "vqro.setup-status-error", "version": 1,
            "setup_id": None, "generation": 0, "require_unit": policy is True,
            "policy_state": "unknown" if policy is None else "on" if policy else "off",
            "unit": {"state": "error", "receipt": None, "verdict": None, "exception": None},
            "integration": {"state": "unverified", "receipt": None, "skip_reason": None},
            "performance": {"state": "unverified", "recommended": True, "why": "checker unavailable"},
            "finish_ok": False, "not_verified_reasons": reasons, "coverage_gaps": [], "instance": None, "error": text(reason)}


def setup_status(state_dir, extensions_dir, *, client=None, hello=None, endpoint=None):
    snapshot = None
    try:
        snapshot = read_snapshot(client, state_dir)
        snapshot, value = setup_scan(client, hello, state_dir, extensions_dir, endpoint, snapshot=snapshot)
        return SetupStore(state_dir).status(value, snapshot)
    except Exception as failure:
        return error_status(f"setup status unavailable: {failure}", snapshot)


def notice(status):
    states = [status[suite]["state"] for suite in ("unit", "integration", "performance")]
    bad = any(state in ("fail", "error", "unverified", "missing", "stale", "pending") for state in states)
    bad = bad or bool(status["not_verified_reasons"]) or status["require_unit"] and not status["finish_ok"] or status["setup_id"] is None
    gaps = bool(status.get("coverage_gaps"))
    if RPC_RULE_NOTICE in status["not_verified_reasons"]:
        label = RPC_RULE_NOTICE
    elif bad:
        label = "NOT VERIFIED"
        if gaps and status["unit"]["state"] == "pass":
            label += " · Unit PASS · coverage gaps"
    else:
        label = "Unit PASS · coverage gaps" if gaps else "Testing verified"
    style = "error" if any(state in ("fail", "error") for state in states) else "warn" if bad or gaps else "ok"
    return {"label": label, "style": style}


def status_segment(status):
    """An empty live label clears the segment; full diagnostics remain in the menu/page."""
    current_failure = status.get("setup_id") is not None and any(
        status[suite].get("current") is True and status[suite]["state"] == "fail" for suite in ("unit", "integration"))
    if status["require_unit"]:
        if not status["finish_ok"]:
            return {"label": "tests !", "style": "warn"}
        if RPC_RULE_NOTICE in status["not_verified_reasons"]:
            return {"label": "not verified", "style": "warn"}
        return {"label": ""}
    return {"label": "tests !", "style": "warn"} if current_failure else {"label": ""}


def setup_page(status):
    segment = notice(status)
    items = [{"type": "text", "text": segment["label"], "style": segment["style"]},
             {"type": "text", "text": f"Unit policy: {status.get('policy_state') or ('on' if status['require_unit'] else 'off')} · finish evidence: {'accepted' if status['finish_ok'] else 'not verified'}"},
             {"type": "text", "text": f"Generation {status['generation']} · setup {status['setup_id'] or 'unknown'}", "style": "dim"}]
    for suite in ("unit", "integration", "performance"):
        result = status[suite]
        items.append({"type": "text", "text": f"{suite.title()}: {result['state']} · {result.get('receipt') or 'no receipt'}",
                      "style": "ok" if result["state"] == "pass" else "warn"})
        if result.get("skip_reason"):
            items.append({"type": "text", "text": "Integration skipped: " + result["skip_reason"], "style": "warn"})
    exception = status["unit"].get("exception")
    if exception:
        items.append({"type": "text", "text": f"Unit exception accepted by {exception['accepted_by']}: {exception['reason']}", "style": "warn"})
    items += [{"type": "text", "text": "Coverage gap: " + text(gap), "style": "warn"} for gap in status.get("coverage_gaps", [])[:20]]
    items += [{"type": "text", "text": text(reason), "style": "warn"} for reason in status["not_verified_reasons"][:20]]
    items += [{"type": "text", "text": "Performance is recommended, never a finish gate. No prose veto or universal runtime enforcement.", "style": "dim"},
              {"type": "chips", "chips": [{"text": "Run Unit", "command": "test.unit"},
                                               {"text": "Integration", "command": "test.integration"},
                                               {"text": "Performance", "command": "test.perf"}]}]
    return {"title": "Testing", "items": items}


def extension_main():
    from vqro_ext import Extension, VqroError
    ext = Extension("test-unit", event_types=["entries", "view.content"],
                    view_content={"names": ["test-integration.report", "test-perf.report"]})
    lock = threading.RLock()
    _, state_dir, extensions_dir = endpoints(cli_parser().parse_args([]))
    state_dir = ext.info.get("state_dir") or state_dir
    tracker = SetupStore(state_dir)
    last = {"result": None, "receipt": None, "status": None, "observer_error": None}
    wake, stopping = threading.Event(), threading.Event()

    def policy_on():
        return ext.value(POLICY_KEY) is True

    def refresh():
        with lock:
            if last["observer_error"] and policy_on():
                try:
                    tracker.invalidate("previous setup observation failed; evidence invalidated")
                    last["observer_error"] = None
                except Exception as failure:
                    try:
                        snapshot = read_snapshot(ext.vqro, state_dir)
                    except Exception:
                        snapshot = None
                    status = error_status(f"setup observation could not be persisted: {failure}", snapshot)
                else:
                    status = setup_status(state_dir, extensions_dir, client=ext.vqro, hello=ext.info)
            else:
                if not policy_on():
                    last["observer_error"] = None
                status = setup_status(state_dir, extensions_dir, client=ext.vqro, hello=ext.info)
            entries = getattr(ext, "entries", {})
            if isinstance(entries, dict) and rpc_rule_notice({"entries": entries}) and RPC_RULE_NOTICE not in status["not_verified_reasons"]:
                status["not_verified_reasons"].insert(0, RPC_RULE_NOTICE)
            last["status"] = status
            ext.publish("testing.status", status_segment(status))
            ext.publish("settings.page.testing", setup_page(status))
            return status

    @ext.command("test.setup.status", "Inspect current setup identity, generation and finish evidence")
    def status_command(args):
        return refresh()

    @ext.command("test.setup.skip", "Explain omitted integration evidence (args: suite, reason)")
    def skip(args):
        try:
            snapshot, value = setup_scan(ext.vqro, ext.info, state_dir, extensions_dir)
            tracker.skip(value, snapshot, args.get("suite"), args.get("reason"))
        except ValueError as failure:
            raise VqroError(str(failure), "invalid") from failure
        return refresh()

    @ext.command("test.setup.accept", "Workspace owner accepts a current Unit exception (args: reason)")
    def accept(args):
        try:
            snapshot, value = setup_scan(ext.vqro, ext.info, state_dir, extensions_dir)
            owner = ext.owner()
            if snapshot.get("entries", {}).get("doc/principals/owner", {}).get("value") != owner:
                raise PermissionError("workspace owner changed; inspect setup status before accepting")
            tracker.accept(value, snapshot, args.get("reason"), ext.caller(), owner)
        except (ValueError, PermissionError) as failure:
            raise VqroError(str(failure), "forbidden" if isinstance(failure, PermissionError) else "invalid") from failure
        return refresh()

    @ext.command("test.unit", "Check installed extension health (args: tests, strict)")
    def run(args):
        with lock:
            snapshot = ext.vqro.call("workspace.snapshot")
            recording_on = snapshot.get("entries", {}).get(POLICY_KEY, {}).get("value", False) is True
            ext.publish("testing.status", status_segment(error_status("Unit checking", snapshot)))
            result = collect(state_dir, extensions_dir, client=ext.vqro, hello=ext.info,
                             tests=args.get("tests", True), strict=args.get("strict", False), tests_budget=20, record=recording_on)
            destination = None
            if recording_on and not result.get("recorded"):
                last["observer_error"] = result["recording"].get("reason") or "latest armed Unit attempt did not record completion evidence"
            if result["recording"]["state"] != "unsupported":
                try:
                    destination = save_receipt(result, state_dir, result.get("receipt"))
                except Exception as failure:
                    result["not_covered"].append(text(f"health report was not saved: {failure}"))
            last.update(result=result, receipt=destination)
            ext.publish("test-unit.report", panel(result, destination))
        refresh()
        return {"verdict": result["verdict"], "health_verdict": result["health_verdict"], "recorded": result["recorded"],
                "recording": result["recording"], "receipt": destination, "counts": result["counts"],
                "client": {"popup": {"view": "test-unit.report", "width": 120}}}

    @ext.command("test.unit.show", "Show the last unit health report and current setup evidence")
    def show(args):
        status = refresh()
        with lock:
            content = panel(last["result"], last["receipt"])
            content["items"] = setup_page(status)["items"] + content["items"]
            if len(content["items"]) > 400:
                content["items"] = content["items"][:399] + [{"type": "text", "style": "dim", "text": "More rows and coverage details in the receipt."}]
            ext.publish("test-unit.report", content)
        return {"client": {"popup": {"view": "test-unit.report", "width": 120}}}

    @ext.on_event
    def changed(kind, data):
        keys = [change["key"] for change in data.get("changes", []) if is_setup_key(change["key"])]
        if kind == "missed" or keys:
            if policy_on():
                try:
                    tracker.invalidate("missed setup events" if kind == "missed" else "observed setup entries changed", keys)
                except Exception as failure:
                    last["observer_error"] = str(failure)
            wake.set()
        elif kind == "view.content":
            wake.set()

    def worker():
        while not stopping.is_set():
            status = last["status"] or {}
            pending = status.get("unit", {}).get("state") == "pending" or status.get("require_unit") and not status.get("finish_ok")
            wake.wait(timeout=5 if pending else None)
            wake.clear()
            if stopping.is_set():
                break
            try:
                refresh()
            except Exception:
                pass  # broken transport cannot establish verified evidence; SDK exits on disconnect.

    ext.contribute({"view/test-unit.report": {"kind": "panel", "title": "Unit health report", "items": panel()["items"]},
                    "menu/testing.status": {"where": "status", "label": "Testing", "view": "testing.status", "command": "test.unit.show", "order": 6}})
    if policy_on():
        try:
            tracker.invalidate("testing observer restarted; current evidence requires reconciliation")
        except Exception as failure:
            last["observer_error"] = str(failure)
    refresh()
    thread = threading.Thread(target=worker, daemon=True, name="setup-evidence")
    thread.start()
    try:
        ext.run()
    finally:
        stopping.set()
        wake.set()
        thread.join(timeout=1)


def main(argv=None):
    parser = cli_parser()
    args = parser.parse_args(argv)
    if args.record and args.no_tests:
        parser.error("--record requires installed tests; --no-tests is only for ad-hoc reports")
    if args.setup_status and (args.record or args.out):
        parser.error("--setup-status cannot be combined with --record or --out")
    socket, state_dir, extensions_dir = endpoints(args)
    client, hello, error = None, None, None
    if not args.offline:
        try:
            from vqro_testing import PlainClient
            client = PlainClient(socket)
            hello = client.call("hello", {"client": "test-unit"})
        except Exception as failure:
            error = str(failure)
            if client is not None:
                client.close()
            client = None
    try:
        if args.setup_status:
            status = (error_status("live setup is unavailable: " + str(error)) if client is None and not args.offline else
                      setup_status(state_dir, extensions_dir, client=client, hello=hello, endpoint=socket))
            if args.json:
                print(json.dumps(status, ensure_ascii=False))
            else:
                print(notice(status)["label"])
                for reason in status["not_verified_reasons"]:
                    print(reason)
            return 2 if status.get("error") else 0
        result = collect(state_dir, extensions_dir, client=client, hello=hello, offline=args.offline,
                         tests=not args.no_tests, strict=args.strict, connection_error=error, record=args.record, endpoint=socket)
    finally:
        if client is not None:
            client.close()
    if args.record or args.out:
        try:
            if args.record and result.get("recorded"):
                save_receipt(result, state_dir, result.get("receipt"))
            elif args.record:
                pass  # Health report remains available; unsupported recording grants no receipt.
            else:
                write_receipt(result, args.out)
        except OSError as failure:
            result["verdict"] = "error"
            result["rows"].append(row("output", "harness", "fail", str(failure)))
            result["counts"]["fail"] += 1
    print_result(result, args.json)
    return EXIT_CODES[result["verdict"]]


if __name__ == "__main__":
    if os.environ.get("VQRO_EXTENSION_LIFETIME") == "stdin":
        extension_main()
    else:
        sys.exit(main())
