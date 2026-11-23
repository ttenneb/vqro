"""Deterministic, cooperative setup-evidence policy; no model calls or workspace writes."""
from contextlib import contextmanager
import datetime
import hashlib
import json
import os
from pathlib import Path
import threading
import uuid

from vqro_testing import (canonical, digest, fingerprint, process_identity, relevant_entries,
                          is_settings_key, text, utc_now, write_receipt)

POLICY_KEY = "doc/settings/testing.require_unit"
STATE_VERSION = 1
RECEIPT_LIMIT = 1024 * 1024
_LOCAL_LOCK = threading.RLock()


def scan(client, hello, state_dir, extensions_dir, endpoint=None):
    if client is not None:
        snapshot = client.call("workspace.snapshot")
    else:
        with (Path(state_dir) / "workspace.json").open() as stream:
            snapshot = json.load(stream)
    entries = relevant_entries(snapshot)
    declarations = {key: entry["value"] for key, entry in entries.items() if key.startswith("extension/")}
    settings = {key: entry["value"] for key, entry in entries.items() if is_settings_key(key)}
    info = dict(hello or {})
    info["epoch"] = snapshot.get("epoch", info.get("epoch"))
    value = fingerprint(extensions_dir, declarations, info, settings=settings, state_dir=state_dir, endpoint=endpoint)
    return snapshot, value


def _date(value):
    return datetime.datetime.fromisoformat(value.replace("Z", "+00:00"))


class SetupStore:
    def __init__(self, state_dir):
        self.state_dir = Path(state_dir)
        self.directory = self.state_dir / "test-unit"
        self.path = self.directory / "setup-state.json"

    @contextmanager
    def locked(self):
        # A separate lock inode survives atomic replacement of the state/receipt files.
        import fcntl
        self.directory.mkdir(parents=True, exist_ok=True, mode=0o700)
        with _LOCAL_LOCK, (self.directory / "setup-state.lock").open("a+b") as lock:
            os.fchmod(lock.fileno(), 0o600)
            fcntl.flock(lock.fileno(), fcntl.LOCK_EX)
            try:
                yield
            finally:
                fcntl.flock(lock.fileno(), fcntl.LOCK_UN)

    def read(self):
        try:
            with self.path.open("rb") as stream:
                data = stream.read(RECEIPT_LIMIT + 1)
            if len(data) > RECEIPT_LIMIT:
                raise ValueError("setup state exceeds size bound")
            state = json.loads(data)
            if type(state.get("version")) is not int or state["version"] != STATE_VERSION or type(state.get("generation")) is not int or state["generation"] < 0:
                raise ValueError("unsupported setup state")
            return state
        except FileNotFoundError:
            return {"version": STATE_VERSION, "observer": uuid.uuid4().hex, "generation": 0,
                    "pending": {}, "changed_extensions": [], "changed_at": utc_now()}

    def invalidate(self, reason, keys=()):
        with self.locked():
            state = self.read()
            state.update(generation=state["generation"] + 1, dirty=True, changed_at=utc_now(), reason=text(reason))
            state["changed_extensions"] = sorted(set(state.get("changed_extensions", [])) |
                                                 {key.removeprefix("extension/") for key in keys if key.startswith("extension/")})
            state["exception"] = state["skip"] = None
            write_receipt(state, self.path)

    def observe(self, value, snapshot):
        entries = relevant_entries(snapshot)
        versions = {key: entry.get("version") for key, entry in entries.items()}
        signature = digest([canonical({key: value.get(key) for key in ("setup_id", "declarations", "settings", "extensions", "executables", "instance", "unknown")})])
        with self.locked():
            state = self.read()
            changed = signature != state.get("signature") or versions != state.get("versions")
            if changed and not state.get("dirty"):
                state.update(generation=state["generation"] + 1, changed_at=utc_now())
                state["exception"] = state["skip"] = None
                old = state.get("fingerprint", {})
                names = {key.removeprefix("extension/") for key, entry in entries.items()
                         if key.startswith("extension/") and entry["value"].get("enabled", True)}
                individual = {name for name in names if old.get("extensions", {}).get(name) != value.get("extensions", {}).get(name)}
                state["changed_extensions"] = sorted(individual or names)
            elif state.get("dirty") and not state.get("changed_extensions"):
                state["changed_extensions"] = sorted(key.removeprefix("extension/") for key, entry in entries.items()
                                                     if key.startswith("extension/") and entry["value"].get("enabled", True))
            state.update(signature=signature, setup_id=value.get("setup_id"), fingerprint=value, versions=versions,
                         instance=value["instance"], dirty=False)
            write_receipt(state, self.path)
            return state

    def context(self, state):
        return {"setup_id": state.get("setup_id"), "generation": state["generation"], "instance": state.get("instance"),
                "setup_observer": state["observer"], "changed_at": state["changed_at"]}

    def begin(self, state):
        stamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
        destination = self.directory / f"receipt-{stamp}-{uuid.uuid4().hex[:8]}.json"
        context = self.context(state)
        context["receipt"] = str(destination)
        with self.locked():
            current = self.read()
            if all(current.get(key) == context.get(key) for key in ("setup_id", "generation")) and current["observer"] == context["setup_observer"]:
                current.setdefault("pending", {})["unit"] = {**context, "pid": os.getpid(), "process_identity": process_identity(os.getpid())}
                write_receipt(current, self.path)
        return context

    def pending(self, suite, destination, pid, context):
        with self.locked():
            state = self.read()
            if any(state.get(key) != context.get(key) for key in ("setup_id", "generation")) or state["observer"] != context.get("setup_observer"):
                return False
            state.setdefault("pending", {})[suite] = {**context, "receipt": str(destination), "pid": pid,
                                                     "process_identity": process_identity(pid)}
            write_receipt(state, self.path)
            return True

    def receipt(self, suite, state):
        directory = self.directory if suite == "unit" else self.state_dir / ("test-perf" if suite == "perf" else "test-integration")
        paths = sorted(directory.glob("receipt-*.json"), key=lambda path: path.name, reverse=True)[:40]
        pending = state.get("pending", {}).get(suite)
        current_pending = pending and pending.get("setup_id") == state.get("setup_id") and pending.get("generation") == state["generation"]
        if current_pending:
            pending_path = Path(pending["receipt"])
            if not pending_path.exists():
                identity = process_identity(pending["pid"])
                recorded_identity = pending.get("process_identity")
                running = identity and recorded_identity and identity[0] == recorded_identity[0] and identity[1] != "Z"
                return {"state": "pending" if running else "error" if suite == "unit" else "unverified", "receipt": str(pending_path), "verdict": None,
                        "why": "run has not completed" if running else "run ended without a complete receipt; never automatically replayed"}
            paths = [pending_path] + [path for path in paths if path != pending_path]
        stale = None
        for path in paths:
            try:
                if path.parent.resolve() != directory.resolve() or path.is_symlink():
                    raise ValueError("receipt is not a regular file in the suite's evidence directory")
                with path.open("rb") as stream:
                    data = stream.read(RECEIPT_LIMIT + 1)
                if len(data) > RECEIPT_LIMIT:
                    raise ValueError("receipt exceeds size bound")
                result = json.loads(data)
                if not isinstance(result, dict):
                    raise ValueError("receipt must be an object")
                if result.get("kind") != "vqro.test-receipt" or type(result.get("version")) is not int or result["version"] != 1 or result.get("suite") != suite:
                    stale = stale or {"state": "unverified", "receipt": str(path), "verdict": None, "why": "unsupported receipt kind/version/suite"}
                    if current_pending and path == Path(pending["receipt"]):
                        return stale
                    continue
                if suite == "unit" and result.get("recorded") is not True:
                    if current_pending and path == Path(pending["receipt"]):
                        return {"state": "unverified", "receipt": str(path), "verdict": None, "why": "pending Unit run did not record its receipt"}
                    continue  # --out is deliberately not an acknowledgement channel.
                if not isinstance(result.get("rows"), list) or not result["rows"] or not isinstance(result.get("counts"), dict):
                    raise ValueError("incomplete receipt rows/counts")
                statuses = ("pass", "warn", "fail", "skip", "unverified", "weak")
                if any(not isinstance(item, dict) or item.get("status") not in statuses or not isinstance(item.get("id"), str) for item in result["rows"]):
                    raise ValueError("malformed receipt check row")
                if any(type(result["counts"].get(status, 0)) is not int or result["counts"].get(status, 0) != sum(item["status"] == status for item in result["rows"]) for status in statuses):
                    raise ValueError("receipt counts do not match its checks")
                if result.get("verdict") == "pass" and any(r.get("status") == "fail" for r in result["rows"]):
                    raise ValueError("pass receipt contains a failed check")
                if suite == "unit" and result.get("verdict") == "pass" and not any(r["id"].startswith("status/") and r["status"] == "pass" for r in result["rows"]):
                    raise ValueError("Unit pass has no observed running extension")
                closure = result.get("closure") or {}
                matched = (state.get("setup_id") is not None and result.get("setup_id") == state["setup_id"]
                           and type(result.get("generation")) is int and result["generation"] == state["generation"] and result.get("instance") == state.get("instance")
                           and result.get("setup_observer") == state["observer"] and closure.get("matched") is True
                           and closure.get("before") == closure.get("after") == state["setup_id"]
                           and type(closure.get("before_generation")) is int and type(closure.get("after_generation")) is int
                           and closure["before_generation"] == closure["after_generation"] == state["generation"])
                try:
                    matched = matched and _date(result["started_at"]) >= _date(state["changed_at"])
                except (KeyError, TypeError, ValueError):
                    matched = False
                if not matched:
                    stale = stale or {"state": "stale", "receipt": str(path), "verdict": result.get("verdict"), "why": "receipt is not for this setup and observed generation"}
                    if current_pending and path == Path(pending["receipt"]):
                        return stale
                    continue
                verdict = result.get("verdict")
                if verdict not in ("pass", "fail", "error", "unverified"):
                    raise ValueError("invalid receipt verdict")
                assessed = {"state": verdict, "receipt": str(path), "verdict": verdict,
                            "receipt_sha256": hashlib.sha256(data).hexdigest(), "why": None, "current": True}
                if suite == "unit" and result.get("tests_enabled") is not True:
                    assessed.update(state="unverified", why="installed tests were disabled or not attested; not completion evidence")
                    return assessed
                if suite == "unit" and verdict == "pass":
                    tests = {r.get("subject"): r for r in result.get("rows", []) if str(r.get("id", "")).startswith("tests/")}
                    gaps, unfinished = [], []
                    for name in state.get("changed_extensions", []):
                        test = tests.get(name, {})
                        if test.get("status") == "unverified":
                            unfinished.append(name)
                        elif test.get("status") != "pass":
                            why = str(test.get("evidence") or "no tests installed")
                            gaps.append(f"no tests installed for {name}" if why.startswith("no tests installed") else f"{name}: {why}")
                    assessed["coverage_gaps"] = gaps
                    if unfinished:
                        assessed.update(state="unverified", why="installed tests did not complete: " + ", ".join(unfinished))
                return assessed
            except (OSError, ValueError, TypeError, AttributeError, RecursionError) as error:
                stale = stale or {"state": "unverified", "receipt": str(path), "verdict": None, "why": text(f"incomplete/unreadable receipt: {error}")}
                if current_pending and path == Path(pending["receipt"]):
                    return stale
        return stale or {"state": "missing", "receipt": None, "verdict": None, "why": "no current recorded receipt"}

    def status(self, value, snapshot):
        state = self.observe(value, snapshot)
        unit, integration, performance = (self.receipt(suite, state) for suite in ("unit", "integration", "perf"))
        reasons = list(value.get("unknown", []))
        policy = snapshot.get("entries", {}).get(POLICY_KEY, {}).get("value", False)
        required = policy is True
        invalid_policy = not isinstance(policy, bool)
        if invalid_policy:
            reasons.append("require_unit setting is not boolean; enforcement cannot be established")
        exception = state.get("exception")
        if not (isinstance(exception, dict) and exception.get("scope") == "unit"
                and isinstance(exception.get("reason"), str) and bool(exception["reason"].strip())
                and exception.get("setup_id") == state.get("setup_id") and type(exception.get("generation")) is int and exception["generation"] == state["generation"]
                and exception.get("receipt_sha256") == unit.get("receipt_sha256")
                and exception.get("accepted_by") == snapshot.get("entries", {}).get("doc/principals/owner", {}).get("value")):
            exception = None
        unit["exception"] = exception
        skip = state.get("skip")
        integration["skip_reason"] = None
        if (isinstance(skip, dict) and isinstance(skip.get("reason"), str) and skip["reason"].strip()
                and skip.get("setup_id") == state.get("setup_id") and type(skip.get("generation")) is int and skip["generation"] == state["generation"]
                and not integration.get("current") and integration["state"] != "pending"):
            previous = self.receipt("integration", {**state, "pending": {}})
            if previous.get("current") and previous["state"] in ("fail", "error"):
                integration = {**previous, "skip_reason": None, "why": "completed failure retained; a later incomplete attempt cannot hide it"}
            else:
                integration.update(state="skipped", skip_reason=skip["reason"])
        if integration["state"] == "error":
            integration["state"] = "fail"
        performance.update(recommended=bool(state.get("changed_extensions")),
                           why="setup changed; rendering/input/stream/startup impact is uncertain" if state.get("changed_extensions") else "no observed hot-path change; performance evidence is optional")
        if value.get("setup_id") is None:
            unit.update(state="unverified", why="setup identity is unknown")
        guard = snapshot.get("extensions", {}).get("test-unit")
        unavailable = guard is None or guard.get("enabled") is False or guard.get("display_status") != "running"
        if unavailable:
            reasons.append("testing extension is off, so changes are not tracked: turn it on, or ask the owner for an exception")
        for name, assessment in (("Unit", unit), ("Integration", integration), ("Performance", performance)):
            if assessment["state"] not in ("pass", "skipped"):
                reasons.append(f"{name}: {assessment['state']}" + (f"; {assessment['why']}" if assessment.get("why") else ""))
        finish = not required
        # Integration evidence is receipt-or-explain; an unavailable receipt is not an explanation.
        attached = integration.get("current") is True and integration.get("state") in ("pass", "fail", "unverified")
        if required:
            finish = ((unit["state"] == "pass" or exception is not None) and (attached or integration["state"] == "skipped")
                      and (not unavailable or exception is not None))
        finish = finish and value.get("setup_id") is not None and not invalid_policy
        return {"setup_id": value.get("setup_id"), "generation": state["generation"], "require_unit": required, "instance": value.get("instance"),
                "unit": unit, "integration": integration, "performance": performance,
                "finish_ok": bool(finish), "not_verified_reasons": list(dict.fromkeys(reasons)), "coverage_gaps": unit.get("coverage_gaps", [])}

    def skip(self, value, snapshot, suite, reason):
        if suite != "integration" or not isinstance(reason, str) or not reason.strip() or len(reason) > 300:
            raise ValueError("integration skip needs a nonempty reason of at most 300 characters")
        status = self.status(value, snapshot)
        if status["integration"].get("current") or status["integration"]["state"] == "pending":
            raise ValueError("cannot hide a completed or pending integration run with a skip reason")
        with self.locked():
            state = self.read()
            if state.get("setup_id") != status["setup_id"] or state["generation"] != status["generation"]:
                raise ValueError("setup changed before the skip was recorded; inspect status again")
            previous = self.receipt("integration", {**state, "pending": {}})
            if previous.get("current") and previous["state"] in ("fail", "error"):
                raise ValueError("cannot hide a completed integration failure behind an unfinished later attempt")
            state["skip"] = {"setup_id": state.get("setup_id"), "generation": state["generation"], "reason": reason.strip()}
            write_receipt(state, self.path)
        return self.status(value, snapshot)

    def accept(self, value, snapshot, reason, caller, owner):
        if not owner or not caller or caller.get("actor") != owner or caller.get("principal") != owner:
            raise PermissionError("test.setup.accept requires the workspace owner principal: actor == principal == owner")
        if not isinstance(reason, str) or not reason.strip() or len(reason) > 300:
            raise ValueError("an exception needs a nonempty reason of at most 300 characters")
        status = self.status(value, snapshot)
        unit = status["unit"]
        if value.get("setup_id") is None or not unit.get("receipt_sha256") or unit["state"] in ("stale", "missing", "pending"):
            raise ValueError("run Unit for this setup/generation before accepting an exception")
        with self.locked():
            state = self.read()
            if state.get("setup_id") != status["setup_id"] or state["generation"] != status["generation"]:
                raise ValueError("setup changed before acceptance; inspect status again")
            state["exception"] = {"reason": reason.strip(), "setup_id": state["setup_id"], "generation": state["generation"],
                                  "accepted_by": owner, "receipt_sha256": unit["receipt_sha256"], "scope": "unit"}
            write_receipt(state, self.path)
        return self.status(value, snapshot)


def bind_start(result, value, snapshot, state_dir, suite, destination=None):
    store = SetupStore(state_dir)
    context = store.context(store.observe(value, snapshot))
    result["started_at"] = utc_now()
    if destination is not None:
        directory = Path(state_dir) / ("test-perf" if suite == "perf" else "test-integration")
        path = Path(destination).absolute()
        if path.parent.resolve() == directory.resolve() and path.name.startswith("receipt-"):
            store.pending(suite, path, os.getpid(), context)
    return context


def bind_end(result, before, client, hello, state_dir, extensions_dir, endpoint=None):
    if before is None:
        return
    try:
        snapshot, value = scan(client, hello, state_dir, extensions_dir, endpoint)
        after = SetupStore(state_dir).context(SetupStore(state_dir).observe(value, snapshot))
        if not stamp_receipt(result, before, after):
            result["not_covered"].append("setup changed during run or identity is unknown; this receipt cannot acknowledge completion")
    except Exception as failure:
        result.update({key: before.get(key) for key in ("setup_id", "generation", "instance", "setup_observer")})
        result["closure"] = {"matched": False}
        result["not_covered"].append(text(f"setup closure unavailable: {failure}"))


def stamp_receipt(result, before, after):
    matched = (before.get("setup_id") is not None and all(before.get(key) == after.get(key) for key in
                                                        ("setup_id", "generation", "instance", "setup_observer")))
    result.update({key: before.get(key) for key in ("setup_id", "generation", "instance", "setup_observer")})
    result["closure"] = {"before": before.get("setup_id"), "after": after.get("setup_id"),
                         "before_generation": before.get("generation"), "after_generation": after.get("generation"), "matched": matched}
    return matched
