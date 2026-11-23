#!/usr/bin/env python3
"""Measure disposable setup copies. Configurable performance limits warn, never fail."""
import argparse
import copy
import json
import math
import os
from pathlib import Path
import shutil
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from fnmatch import fnmatchcase

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
from vqro_process import background_kwargs
from vqro_testing import (EXIT_CODES, RECEIPT_BYTES, STATUSES, PlainClient, Sandbox, canonical,
                          discover_scenarios, endpoints, fingerprint, is_settings_key, private_processes,
                          row, script_path, text, utc_now, write_receipt,
                          scenario_matches, run_scenarios, source_mark, source_untouched)
from testing_unit import offline_facts
from testing_setup import SetupStore, bind_start, bind_end, scan as setup_scan, rpc_rule_notice
from vqro_testing import has_rpc_notice, preserve_rpc_notice, RPC_RULE_NOTICE

DEFAULTS = {"relative_slowdown": 0.25, "noise_ms": 5, "rss_relative": 0.25, "noise_rss_mb": 10,
            "idle_new_activity": True, "size_relative": 2.0,
            "absolute": {"startup_core_ms": None, "idle_events_per_s": 0.5, "idle_cpu_percent": 40, "rss_mb": 90,
                         "interaction_ms": 170, "item_bytes": 1048576, "total_content_bytes": 8388608}}


def thresholds(snapshot):
    entries = snapshot.get("entries", {})
    settings = entries.get("doc/settings/test-perf", {}).get("value")
    record = {"source": "default", "key": "doc/settings/test-perf", "values": copy.deepcopy(DEFAULTS), "overrides": [], "ignored": []}
    if "doc/settings/test-perf" not in entries:
        return record, []
    try:
        if not isinstance(settings, dict):
            raise ValueError("settings must be an object")
        result = record["values"]
        for key, value in settings.items():
            if key not in result:
                record["ignored"].append(key)
                continue
            if key == "absolute":
                if not isinstance(value, dict):
                    raise ValueError("absolute thresholds must be an object")
                for name, number in value.items():
                    if name not in result[key]:
                        record["ignored"].append("absolute." + name)
                    else:
                        result[key][name] = number
                        record["overrides"].append("absolute." + name)
            else:
                result[key] = value
                record["overrides"].append(key)
        for key, value in result.items():
            if key == "idle_new_activity":
                if not isinstance(value, bool):
                    raise ValueError("idle_new_activity must be boolean")
                continue
            for number in value.values() if isinstance(value, dict) else [value]:
                if number is None and key == "absolute":
                    continue
                if isinstance(number, bool) or not isinstance(number, (int, float)) or not math.isfinite(number) or number < 0:
                    raise ValueError(f"invalid nonnegative finite threshold: {key}")
        record["source"] = "settings"
        return record, []
    except ValueError as error:
        record.update(source="default", values=copy.deepcopy(DEFAULTS), overrides=[])
        return record, [row("thresholds", "settings", "warn", f"malformed settings; using defaults: {error}")]


def measurement(ident, values, expected, unit, workload):
    valid = [v for v in values if isinstance(v, (int, float)) and not isinstance(v, bool) and math.isfinite(v) and v >= 0]
    good = expected > 0 and len(valid) >= expected and len(valid) == len(values)
    result = row(*ident.split("/", 1), "pass" if good else "fail",
                 f"{len(valid)}/{expected} finite samples" if good else f"broken measurement: {len(valid)}/{expected} finite samples")
    result["measure"] = {"n": len(valid), "median": statistics.median(valid) if valid else None,
                         "min": min(valid) if valid else None, "max": max(valid) if valid else None,
                         "samples": [v if isinstance(v, (int, float)) and not isinstance(v, bool) and math.isfinite(v) else None for v in values],
                         "unit": unit, "workload": workload}
    return result


def ceiling(ident, limits, window):
    absolute = limits["absolute"]
    if ident == "startup/core":
        return absolute["startup_core_ms"]
    if ident.startswith("idle/events"):
        return absolute["idle_events_per_s"]
    if ident.startswith("idle/cpu/"):
        value = absolute["idle_cpu_percent"]
        return None if value is None else value * window / 100
    if ident.startswith("idle/rss/"):
        return absolute["rss_mb"]
    if ident.startswith("interaction/"):
        return absolute["interaction_ms"]
    if ident == "size/total":
        return absolute["total_content_bytes"]
    if ident.startswith("size/"):
        return absolute["item_bytes"]
    return None


def absolute_rows(rows, limits, window):
    for current in rows:
        if current["status"] != "pass":
            continue
        limit = ceiling(current["id"], limits, window)
        if limit is not None and current["measure"]["median"] > limit:
            current.update(status="warn", evidence=text(f"over configured/default ceiling {limit:g} {current['measure']['unit']}"))
    return rows


def evaluate(baseline, candidate, limits, window):
    if candidate is None:
        return copy.deepcopy(baseline["rows"])
    before = {r["id"]: r for r in baseline["rows"]}
    after = {r["id"]: r for r in candidate["rows"]}
    failures = {}
    for label, section in (("baseline", baseline), ("candidate", candidate)):
        for item in section["rows"]:
            if item["status"] == "fail":
                failed = failures.setdefault(item["id"], copy.deepcopy(item))
                failed.setdefault("failed_runs", []).append(label)
    rows = list(failures.values())
    for ident in sorted(set(before) | set(after)):
        old, current = before.get(ident), after.get(ident)
        if any(item and item["status"] == "fail" for item in (old, current)):
            continue
        if old is None or current is None:
            available = (old or current)["measure"]
            missing = measurement(ident, [], available["workload"]["repeats"], available["unit"], available["workload"])
            missing["evidence"] = "broken measurement: baseline/candidate scope missing"
            rows.append(missing)
            continue
        bstats, cstats = old["measure"], current["measure"]
        status, messages = "pass", []
        ratio = difference = None
        if "unverified" in (old["status"], current["status"]):
            status, messages = "unverified", ["measurement unavailable or startup event never observed"]
        else:
            b, c = bstats["median"], cstats["median"]
            ratio, difference = c / b if b else None, c - b
            if current["status"] == "warn":
                status, messages = "warn", [current["evidence"]]
            if ident.startswith("idle/events") and limits["idle_new_activity"] and b <= 0.05 and c > 0.5:
                status = "warn"
                messages.append("new steady idle activity")
            elif ident.startswith("size/"):
                if c > b * limits["size_relative"]:
                    status = "warn"
                    messages.append("larger than baseline size multiplier")
            elif ident.startswith(("startup/", "interaction/", "idle/rss/")):
                rss = ident.startswith("idle/rss/")
                relative = limits["rss_relative"] if rss else limits["relative_slowdown"]
                noise = limits["noise_rss_mb"] if rss else limits["noise_ms"]
                if c > b * (1 + relative) and c - b > noise:
                    if cstats["min"] > bstats["max"]:
                        status = "warn"
                        messages.append("more memory than baseline" if rss else "slower than baseline")
                    else:
                        status = "unverified"
                        messages.append("can't tell (noise): baseline/candidate spreads overlap")
        result = row("compare", ident, status, "; ".join(messages) or "no threshold breach")
        result["measure"] = {"unit": cstats["unit"], "baseline": {k: bstats[k] for k in ("median", "min", "max")},
                             "candidate": {k: cstats[k] for k in ("median", "min", "max")}, "ratio": ratio, "difference": difference}
        rows.append(result)
    return rows


def verdict(rows, error=False):
    if error:
        return "error"
    if any(r["status"] == "fail" for r in rows):
        return "fail"
    counting = [r for r in rows if r["id"] != "live/untouched"]
    if not counting or any(r["status"] == "unverified" for r in counting):
        return "unverified"
    return "pass"


def machine():
    try:
        load = list(os.getloadavg())
    except (AttributeError, OSError):
        load = None
    return {"cpu_count": os.cpu_count(), "loadavg": load}


def parser():
    result = argparse.ArgumentParser(description=__doc__)
    for flag in ("json", "offline", "allow-external", "keep", "self-test"):
        result.add_argument("--" + flag, action="store_true")
    for flag in ("socket", "state-dir", "extensions-dir", "vqro", "candidate-ext", "candidate-ops", "root", "out"):
        result.add_argument("--" + flag)
    result.add_argument("--backend", choices=("fake", "private"), default="fake")
    result.add_argument("--repeats", default=5)
    result.add_argument("--window", default=10)
    return result


class Monitor:
    """Receive-time counters: the harness clock includes transport and delivery latency."""
    def __init__(self, sandbox, scenarios):
        self.sandbox, self.scenarios = sandbox, scenarios
        facts = offline_facts(sandbox.declarations, sandbox.ext)
        self.required = {name: [cap[8:].split("@")[0] for cap in (facts.get(name, {}).get("manifest") or {}).get("provides", [])
                                if cap.startswith("command/")] for name in sandbox.enabled()}
        self.running, self.commands, self.ready = set(), set(), {}
        self.activation = None
        self.window = None
        self.events, self.content = 0, 0
        self.ext_events = {name: 0 for name in sandbox.enabled()}
        self.ext_content = dict(self.ext_events)
        self.item_max, self.live_sizes = {}, {}
        self.total_max = 0
        self.active = None
        self.values = {}
        self.gap = False
        self.client = None

    def size(self, view, content):
        if content is None:
            self.live_sizes.pop(view, None)
        else:
            size = len(canonical(content))
            self.item_max[view] = max(self.item_max.get(view, 0), size)
            self.live_sizes[view] = size
        self.total_max = max(self.total_max, sum(self.live_sizes.values()))

    def snapshot(self, snapshot):
        self.live_sizes = {}
        for view, content in snapshot.get("live", {}).items():
            self.size(view, content)

    def __call__(self, message, received):
        method = message.get("method")
        if method == "sandbox.activating":
            self.activation = received
            return
        if method == "missed":
            self.gap = True
            return
        if method != "event":
            return
        event = message.get("params") or {}
        kind, data = event.get("type"), event.get("data") or {}
        if kind == "missed":
            self.gap = True
        if kind == "extension":
            name = data.get("name")
            if (data.get("state") or {}).get("status") == "running":
                self.running.add(name)
            else:
                self.running.discard(name)
        if kind == "command":
            if data.get("available", True):
                self.commands.add(data.get("name"))
            else:
                self.commands.discard(data.get("name"))
        if self.activation is not None:
            for name, patterns in self.required.items():
                if name not in self.ready and name in self.running and all(any(fnmatchcase(command, pattern) for command in self.commands if command) for pattern in patterns):
                    self.ready[name] = received
        if kind == "view.content":
            self.size(data["view"], data.get("content"))
        if self.window and self.window[0] <= received < self.window[1]:
            self.events += 1
            author = data.get("author", "")
            owner = author.removeprefix("extension:") if isinstance(author, str) and author.startswith("extension:") else None
            if owner in self.ext_events:
                self.ext_events[owner] += 1
            if kind == "view.content":
                size = len(canonical(data.get("content")))
                self.content += size
                if owner in self.ext_content:
                    self.ext_content[owner] += size
        if self.active and self.active["seen"] is None:
            predicate = self.active.get("predicate")
            observed = None
            if predicate and kind == "view.content" and data.get("view") == predicate.get("content"):
                observed = {"present": data.get("content") is not None, "value": data.get("content")}
            elif predicate and kind == "entries" and "entry" in predicate:
                for change in data.get("changes", []):
                    if change.get("key") == predicate["entry"]:
                        observed = {"present": change.get("version") is not None, "value": change.get("value")}
            if observed is not None:
                path = predicate.get("path", [])
                if isinstance(path, str):
                    path = path.split(".") if path else []
                for field in path:
                    try:
                        observed["value"] = observed["value"][int(field)] if isinstance(observed["value"], list) else observed["value"][field]
                    except (KeyError, IndexError, TypeError, ValueError):
                        observed = {"present": False, "value": None}
                        break
                if scenario_matches(observed, predicate):
                    self.active["seen"] = received

    def step(self, phase, name, index, step, scenario):
        if phase == "before" and "invoke" in step:
            following = scenario["steps"][index] if len(scenario["steps"]) > index else {}
            self.active = {"id": f"interaction/{name}/{index}", "started": time.monotonic(),
                           "predicate": following.get("wait"), "seen": None}
        elif phase == "before" and "wait" not in step:
            self.active = None
        elif phase == "reply" and self.active:
            self.values[self.active["id"] + "/reply"] = (time.monotonic() - self.active["started"]) * 1000
        elif self.active and (phase == "error" or phase == "after" and "wait" in step):
            if phase == "after" and self.active.get("predicate"):
                try:
                    while self.active["seen"] is None:
                        self.client.next_event(self.client.step_deadline)
                except TimeoutError:
                    pass
            if self.active.get("predicate") and self.active["seen"] is not None:
                self.values[self.active["id"] + "/content"] = (self.active["seen"] - self.active["started"]) * 1000
            self.active = None


def consume(client, deadline):
    while time.monotonic() < deadline:
        try:
            client.next_event(deadline)
        except TimeoutError:
            break


def process_samples(sandbox):
    try:
        processes = private_processes(sandbox.root, sandbox.started_ticks)
    except OSError:
        return None
    if processes is None:
        return None
    result = {name: {} for name in sandbox.enabled()}
    for process in processes:
        matches = []
        for name in result:
            declaration = sandbox.declarations["extension/" + name]
            script = script_path(declaration, sandbox.ext)
            argv = declaration.get("argv") or []
            command = process["command"].split()
            if (script and str(script) in process["command"]) or (not script and argv and command and Path(argv[0]).name.startswith("vqro-") and Path(command[0]).name == Path(argv[0]).name):
                matches.append(name)
        if len(matches) != 1:
            continue
        try:
            fields = Path(f"/proc/{process['pid']}/stat").read_text().rpartition(")")[2].split()
            if int(fields[19]) != process["start_ticks"] or fields[0] == "Z":
                continue
            result[matches[0]][(process["pid"], process["start_ticks"])] = {
                "cpu": (int(fields[11]) + int(fields[12])) / os.sysconf("SC_CLK_TCK"),
                "rss": int(fields[21]) * os.sysconf("SC_PAGE_SIZE") / (1024 * 1024)}
        except (OSError, ValueError, IndexError):
            continue
    return result


def expected_scopes(sandbox, scenarios):
    scopes = {"startup/core": "ms", "idle/events": "events_per_s", "idle/content": "bytes_per_s", "size/total": "bytes"}
    for name in sandbox.enabled():
        scopes.update({f"startup/{name}": "ms", f"idle/events/{name}": "events_per_s", f"idle/content/{name}": "bytes_per_s",
                       f"idle/cpu/{name}": "cpu_s", f"idle/rss/{name}": "mb"})
    for name, scenario in scenarios.items():
        for index, step in enumerate(scenario["steps"], 1):
            if "invoke" in step:
                scopes[f"interaction/{name}/{index}/reply"] = "ms"
                if len(scenario["steps"]) > index and "wait" in scenario["steps"][index]:
                    scopes[f"interaction/{name}/{index}/content"] = "ms"
    return scopes


def collect_copy(sandbox, scenarios, window):
    values, unavailable, errors = {}, {}, []
    monitor = Monitor(sandbox, scenarios)
    sandbox.observer = monitor
    scenarios_result = []
    try:
        started = time.monotonic()
        sandbox.start()
        sandbox.wait_running()
        values["startup/core"] = (time.monotonic() - started) * 1000
        monitor.client = sandbox.client
        for name in sandbox.enabled():
            if name in monitor.ready and monitor.activation is not None:
                values["startup/" + name] = (monitor.ready[name] - monitor.activation) * 1000
            else:
                unavailable["startup/" + name] = "no running/command readiness events observed; time-to-ready unavailable"
        monitor.snapshot(sandbox.client.call("workspace.snapshot"))
        consume(sandbox.client, time.monotonic() + 2)
        before = process_samples(sandbox)
        quiet = time.monotonic()
        monitor.window = (quiet, quiet + window)
        consume(sandbox.client, quiet + window)
        monitor.window = None
        after = process_samples(sandbox)
        values.update({"idle/events": monitor.events / window, "idle/content": monitor.content / window})
        for name in sandbox.enabled():
            values[f"idle/events/{name}"] = monitor.ext_events[name] / window
            values[f"idle/content/{name}"] = monitor.ext_content[name] / window
            if before is None or after is None or not before.get(name) and not after.get(name):
                for metric in ("cpu", "rss"):
                    unavailable[f"idle/{metric}/{name}"] = "unavailable: no attributable harness-owned process facts"
            elif before.get(name) and after.get(name) and set(before[name]) == set(after[name]):
                values[f"idle/cpu/{name}"] = sum(after[name][pid]["cpu"] - before[name][pid]["cpu"] for pid in before[name])
                values[f"idle/rss/{name}"] = sum(p["rss"] for p in after[name].values())
        scenarios_result = run_scenarios(sandbox.client, scenarios, observer=monitor.step)
        values.update(monitor.values)
        monitor.snapshot(sandbox.client.call("workspace.snapshot"))
        values["size/total"] = monitor.total_max
        values.update({f"size/item/{view}": size for view, size in monitor.item_max.items()})
        if monitor.gap:
            errors.append("event stream gap: rates and event timings are incomplete")
    except Exception as failure:
        errors.append(f"{type(failure).__name__}: {failure}")
    finally:
        sandbox.stop()
    return {"values": values, "unavailable": unavailable, "errors": errors, "scenarios": scenarios_result,
            "sandbox": sandbox.info, "leftovers": sandbox.leftovers}


def collect_section(sandboxes, scenarios, repeats, window, limits):
    records, scopes, unavailable = [], {}, {}
    for sandbox in sandboxes:
        scopes.update(expected_scopes(sandbox, scenarios))
        record = collect_copy(sandbox, scenarios, window)
        records.append(record)
        unavailable.update(record["unavailable"])
        for ident in record["values"]:
            if ident not in scopes:
                scopes[ident] = "bytes"
    rows = []
    workload = {"repeats": repeats, "window_s": window, "settle_s": 2}
    for ident, unit in sorted(scopes.items()):
        samples = [record["values"][ident] for record in records if ident in record["values"]]
        item = measurement(ident, samples, repeats, unit, workload)
        if not samples and ident in unavailable and all(ident in record["unavailable"] for record in records):
            item.update(status="unverified", evidence=unavailable[ident])
        rows.append(item)
    if any(record["errors"] for record in records):
        broken = measurement("measurement/sandbox", [], repeats, "ms", workload)
        broken["evidence"] = text([e for record in records for e in record["errors"]])
        rows.append(broken)
    return {"rows": absolute_rows(rows, limits, window), "runs": records}


def run(args):
    started, stamp = time.monotonic(), utc_now()
    initial_machine = machine()
    result = {"kind": "vqro.test-receipt", "version": 1, "suite": "perf", "source": "offline",
              "started_at": stamp, "fingerprint": {}, "inputs": [], "baseline": None, "candidate": None,
              "thresholds": thresholds({})[0], "not_covered": ["functional correctness: use test-integration", "rendering and real agent profiles"],
              "sandbox": {"started": False, "canary": {"status": "not run", "checked": {"env": [], "paths": {}}, "violations": []}}}
    rows, sandboxes, root, owned, live, before, error = [], [], None, False, None, None, False
    context, hello = None, {}
    endpoint, state, extensions = endpoints(args)
    try:
        args.repeats = int(args.repeats)
        args.window = float(args.window)
        if not 1 <= args.repeats <= 50 or not math.isfinite(args.window) or not 1 <= args.window <= 120:
            raise ValueError("repeats must be 1..50 and window must be 1..120 seconds")
        if not args.offline:
            try:
                live = PlainClient(endpoint)
            except OSError:
                live = None
        hello = {}
        if live:
            hello = live.call("hello", {"client": "test-perf"})
            snapshot, before = source_mark(live)
            result["source"] = "live"
            result["inputs"] += ["hello", "workspace.snapshot", "extension.list"]
        else:
            with (state / "workspace.json").open() as stream:
                snapshot = json.load(stream)
            result["inputs"].append(str(state / "workspace.json"))
        config, config_rows = thresholds(snapshot)
        result["thresholds"] = config
        rows += config_rows
        limits = config["values"]
        result["inputs"].append(str(extensions))
        declarations = {key: entry["value"] for key, entry in snapshot["entries"].items() if key.startswith("extension/")}
        info = dict(hello, epoch=snapshot.get("epoch", hello.get("epoch")))
        result["fingerprint"] = fingerprint(extensions, declarations, info,
                                            settings={key: entry["value"] for key, entry in snapshot["entries"].items() if is_settings_key(key)},
                                            state_dir=state, endpoint=endpoint)
        context = bind_start(result, result["fingerprint"], snapshot, state, "perf", args.out)
        scenarios = discover_scenarios(extensions)
        if args.candidate_ext:
            scenarios.update(discover_scenarios(args.candidate_ext))
        ops = []
        if args.candidate_ops:
            with Path(args.candidate_ops).open() as stream:
                ops = json.load(stream)
            if isinstance(ops, dict):
                ops = ops["ops"]
        root = Path(args.root).resolve() if args.root else Path(tempfile.mkdtemp(prefix="vqp-"))
        if args.root and root.exists() and any(root.iterdir()):
            raise ValueError("run root must be absent or empty")
        root.mkdir(parents=True, exist_ok=True, mode=0o700)
        owned = True
        common = {"binary": args.vqro, "backend": args.backend, "allow_external": args.allow_external, "keep": args.keep}
        groups = {"baseline": []}
        if args.candidate_ext or args.candidate_ops:
            groups["candidate"] = []
        for section, group in groups.items():
            for index in range(args.repeats):
                sandbox = Sandbox(extensions, snapshot, root=root / f"{section[0]}{index}",
                                  candidate_ext=args.candidate_ext if section == "candidate" else None,
                                  candidate_ops=ops if section == "candidate" else None, **common)
                sandboxes.append(sandbox)
                group.append(sandbox)
                result["sandbox"] = sandbox.info
                sandbox.prepare()
        rows.append(row("canary", "sandbox", "pass", "all repeat copies passed private environment/endpoint probes"))
        if initial_machine["loadavg"] and initial_machine["cpu_count"] and initial_machine["loadavg"][0] > initial_machine["cpu_count"]:
            rows.append(row("environment", "load", "warn", "busy machine; results noisy"))
        for section, group in groups.items():
            result[section] = collect_section(group, scenarios, args.repeats, args.window, limits)
            result[section]["fingerprint"] = fingerprint(group[0].ext, group[0].declarations, group[0].info.get("vqro") or {})
        rows += evaluate(result["baseline"], result["candidate"], limits, args.window)
        if args.self_test:
            broken = measurement("control/samples", [], args.repeats, "ms", {})
            rows.append(row("self-test", "samples", "pass" if broken["status"] == "fail" else "fail", "zero-sample measurement rejected"))
        binary_version = result["sandbox"].get("vqro") or {}
        if hello.get("version") and binary_version.get("version") != hello["version"]:
            rows.append(row("version", "sandbox", "warn", {"source": hello["version"], "sandbox": binary_version.get("version")}))
    except Exception as failure:
        error = True
        if result["sandbox"]["canary"]["status"] == "fail":
            rows.append(row("canary", "sandbox", "fail", str(failure)))
        rows.append(row("harness", "perf", "fail", f"{type(failure).__name__}: {failure}"))
    finally:
        bind_end(result, context, live, hello, state, extensions, endpoint)
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
            result["not_covered"] += [f"not copied: {item['key']}: {item['error']}" for item in sandbox.info["not_copied"]]
        if owned and not args.keep and root and root.exists():
            shutil.rmtree(root)
    final_machine = machine()
    result.update(rows=rows, verdict=verdict(rows, error), elapsed_ms=round((time.monotonic() - started) * 1000),
                  counts={status: sum(r["status"] == status for r in rows) for status in STATUSES},
                  machine={"cpu_count": initial_machine["cpu_count"], "loadavg_start": initial_machine["loadavg"], "loadavg_end": final_machine["loadavg"]},
                  skipped=[], leftovers=[process for box in sandboxes for process in box.leftovers])
    result["warnings"] = f"{result['counts']['warn']} performance warnings"
    if len(json.dumps(result, ensure_ascii=False, indent=2, allow_nan=False).encode()) > RECEIPT_BYTES:
        known_rpc = has_rpc_notice(result)
        result.update(verdict="error", baseline=None, candidate=None, fingerprint={}, inputs=[],
                      sandbox={key: value for key, value in result["sandbox"].items() if key in ("root", "backend", "started", "canary")},
                      not_covered=["complete receipt exceeded the size bound"], leftovers=result["leftovers"][:20],
                      rows=[row("harness", "receipt", "fail", "receipt exceeds size bound")],
                      counts={status: int(status == "fail") for status in STATUSES}, warnings="0 performance warnings")
        preserve_rpc_notice(result, known_rpc)
    return result


def panel(result=None, destination=None):
    items = [{"type": "text", "text": "Run test.perf to measure disposable setup copies."}]
    if result:
        styles = {"pass": "ok", "fail": "error", "warn": "warn", "skip": "dim", "unverified": "dim"}
        items = [{"type": "text", "text": f"{result['verdict'].upper()} · {result['warnings']}"},
                 {"type": "text", "text": f"Receipt: {destination}"}]
        for r in result["rows"][:380]:
            stats = r.get("measure", {})
            value = f" · {stats.get('median')} {stats.get('unit', '')}" if "median" in stats else ""
            items.append({"type": "text", "style": styles[r["status"]], "text": f"{r['id']}: {r['status']}{value} · {r['evidence']}"})
        items.append({"type": "text", "style": "dim", "text": "Limits warn only. Functional correctness, rendering and agent profiles not covered."})
        if has_rpc_notice(result):
            items.insert(0, {"type": "text", "text": RPC_RULE_NOTICE, "style": "warn"})
    return {"title": "Performance report", "items": items}


def extension_main():
    from vqro_ext import Extension
    ext = Extension("test-perf", event_types=[])
    lock = threading.Lock()
    last = {"process": None, "result": None, "receipt": None}

    @ext.command("test.perf", "Measure a disposable setup copy (args: candidate_ext, candidate_ops, repeats)")
    def start(args):
        with lock:
            if last["process"] is not None and last["process"].poll() is None:
                return {"started": False, "receipt": last["receipt"]}
            _, state, extensions = endpoints(parser().parse_args([]))
            state = Path(ext.info.get("state_dir") or state)
            directory = state / "test-perf"
            directory.mkdir(parents=True, exist_ok=True, mode=0o700)
            stamp = f"{time.time_ns():x}"
            destination = directory / f"receipt-{stamp}.json"
            command = [sys.executable, "-B", str(Path(__file__).resolve()), "--json", "--socket", ext.info["socket"],
                       "--state-dir", str(state), "--extensions-dir", str(extensions), "--root", str(directory / f"run-{stamp}"), "--out", str(destination)]
            for name in ("candidate_ext", "candidate_ops", "repeats"):
                if name in args:
                    command.extend(["--" + name.replace("_", "-"), str(args[name])])
            environment = {"HOME": os.environ.get("HOME", str(Path.home())), "PATH": os.environ.get("PATH", os.defpath)}
            snapshot, value = setup_scan(ext.vqro, ext.info, state, extensions)
            store = SetupStore(state)
            context = store.context(store.observe(value, snapshot))
            child = subprocess.Popen(command, env=environment, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                     stderr=subprocess.DEVNULL, start_new_session=True, **background_kwargs())
            store.pending("perf", destination, child.pid, context)
            last.update(process=child, receipt=str(destination))

            def finish():
                child.wait()
                try:
                    result = json.loads(destination.read_text())
                    if (not isinstance(result, dict) or result.get("kind") != "vqro.test-receipt" or result.get("suite") != "perf"
                            or result.get("verdict") not in ("pass", "fail", "unverified", "error")
                            or not isinstance(result.get("counts"), dict) or not isinstance(result.get("rows"), list)):
                        raise ValueError("subprocess did not produce a supported receipt object")
                except (OSError, ValueError) as failure:
                    result = {"kind": "vqro.test-receipt", "version": 1, "suite": "perf", "verdict": "error", "warnings": "0 performance warnings",
                              "counts": {status: int(status == "fail") for status in STATUSES}, "rows": [row("harness", "subprocess", "fail", str(failure))]}
                    preserve_rpc_notice(result, bool(rpc_rule_notice(snapshot)))
                    write_receipt(result, destination)
                with lock:
                    last["result"] = result
                    ext.publish("test-perf.report", panel(result, str(destination)))
            threading.Thread(target=finish, daemon=True, name="performance-report").start()
            return {"started": True, "receipt": str(destination)}

    @ext.command("test.perf.show", "Show the last performance report")
    def show(args):
        with lock:
            ext.publish("test-perf.report", panel(last["result"], last["receipt"]))
        return {"client": {"popup": {"view": "test-perf.report", "width": 120}}}

    ext.contribute({"view/test-perf.report": {"kind": "panel", **panel()}})
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
    if args.json:
        print(json.dumps(result, ensure_ascii=False, allow_nan=False))
    else:
        print(f"{result['verdict'].upper()} · {result['warnings']}")
        for item in result["rows"]:
            print(f"{item['status']:10} {item['id']}: {item['evidence']}")
    return EXIT_CODES[result["verdict"]]


if __name__ == "__main__":
    if os.environ.get("VQRO_EXTENSION_LIFETIME") == "stdin":
        extension_main()
    else:
        sys.exit(main())
