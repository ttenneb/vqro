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
from vqro_testing import (EXIT_CODES, cli_parser, endpoints, fingerprint, print_result, receipt,
                          resolve_program, row, save_receipt, script_path, text, utc_now, write_receipt)

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


def dependency_row(name, facts):
    dependencies = facts.get("dependencies")
    if facts.get("enabled") is False or not dependencies:
        return row("deps", name, "skip", "off" if facts.get("enabled") is False else "no dependency facts")
    status = dependencies.get("status")
    mapped = {"missing": "fail", "degraded": "warn", "ok": "pass"}.get(status, "unverified")
    unmet = [item for field in ("needs", "works_with") for item in dependencies.get(field, [])
             if item.get("status") != "satisfied"]
    return row("deps", name, mapped, {"status": status, "unmet": unmet},
               "; ".join(str(item["suggest"]) for item in unmet if item.get("suggest")))


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
            completed = subprocess.run(["python3", "-B", "-m", "unittest", "discover", "-s", str(directory.resolve()),
                                        "-p", "test_*.py"], cwd=directory,
                                       env={"PATH": os.environ.get("PATH", os.defpath), "HOME": home,
                                            "VQRO_TEST_EXTENSIONS_DIR": str(Path(extensions_dir).resolve())},
                                       stdout=output, stderr=subprocess.STDOUT, timeout=timeout, check=False)
            status = "pass" if completed.returncode == 0 else "fail"
            evidence = f"exit {completed.returncode}"
        except subprocess.TimeoutExpired:
            status, evidence = "fail", f"timeout after {timeout} s"
        output.seek(0, os.SEEK_END)
        output.seek(max(0, output.tell() - OUTPUT_BYTES))
        tail = output.read(OUTPUT_BYTES).decode("utf-8", "replace")
    return row("tests", name, status, evidence + ": " + tail[-250:])


def collect(state_dir, extensions_dir, *, client=None, hello=None, offline=False, tests=True,
            strict=False, timeout=TEST_TIMEOUT, connection_error=None, tests_budget=None):
    started, started_at = time.monotonic(), utc_now()
    rows, inputs, not_covered = [], [], [NOT_COVERED]
    source = "live" if client is not None and not offline else "offline"
    declarations, fp = {}, {}
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
        if hello.get("safe_mode") or snapshot.get("safe_mode") or any(f.get("safe_mode") for f in facts.values()):
            rows.append(row("core", "safe_mode", "warn", "core is in safe mode"))
        fp = fingerprint(extensions_dir, declarations, hello)
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
        return receipt(rows, source=source, started_at=started_at, started=started, fingerprint_value=fp,
                       inputs=inputs, not_covered=not_covered, extension_count=len(declarations), strict=strict)
    except Exception as error:
        rows.append(row("error", "harness", "fail", f"{type(error).__name__}: {error}"))
        return receipt(rows, source=source, started_at=started_at, started=started, fingerprint_value=fp,
                       inputs=inputs, not_covered=not_covered, extension_count=len(declarations), error=True)


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


def extension_main():
    from vqro_ext import Extension
    ext = Extension("test-unit", event_types=[])
    lock = threading.Lock()
    last = {"result": None, "receipt": None}

    @ext.command("test.unit", "Check installed extension health (args: tests, strict)")
    def run(args):
        with lock:
            _, state_dir, extensions_dir = endpoints(cli_parser().parse_args([]))
            state_dir = ext.info.get("state_dir") or state_dir
            result = collect(state_dir, extensions_dir, client=ext.vqro, hello=ext.info,
                             tests=args.get("tests", True), strict=args.get("strict", False), tests_budget=20)
            destination = save_receipt(result, state_dir)
            last.update(result=result, receipt=destination)
            ext.publish("test-unit.report", panel(result, destination))
            return {"verdict": result["verdict"], "receipt": destination, "counts": result["counts"],
                    "client": {"popup": {"view": "test-unit.report", "width": 120}}}

    @ext.command("test.unit.show", "Show the last unit health report")
    def show(args):
        with lock:
            ext.publish("test-unit.report", panel(last["result"], last["receipt"]))
        return {"client": {"popup": {"view": "test-unit.report", "width": 120}}}

    ext.contribute({"view/test-unit.report": {"kind": "panel", "title": "Unit health report",
                                              "items": panel()["items"]}})
    ext.run()


def main(argv=None):
    args = cli_parser().parse_args(argv)
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
        result = collect(state_dir, extensions_dir, client=client, hello=hello, offline=args.offline,
                         tests=not args.no_tests, strict=args.strict, connection_error=error)
    finally:
        if client is not None:
            client.close()
    if args.out:
        try:
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
