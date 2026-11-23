"""Shared, stdlib-only receipt and CLI mechanisms for installed-setup checks."""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import select
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
from collections import deque
from fnmatch import fnmatchcase

sys.dont_write_bytecode = True

STATUSES = ("pass", "warn", "fail", "skip", "unverified")
EXIT_CODES = {"pass": 0, "fail": 1, "unverified": 77, "error": 2}
EVIDENCE_CHARS = 300
RECEIPT_BYTES = 750 * 1024


def text(value):
    if not isinstance(value, str):
        value = json.dumps(value, sort_keys=True, ensure_ascii=False)
    return value[:EVIDENCE_CHARS]


def row(check, subject, status, evidence, advice=None):
    if status not in STATUSES + ("weak",):
        raise ValueError(f"unknown check status: {status}")
    return {"id": f"{check}/{subject}", "subject": subject, "status": status,
            "evidence": text(evidence), "advice": text(advice) if advice else None}


def canonical(value):
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode()


def digest(parts):
    result = hashlib.sha256()
    for part in parts:
        # Length framing prevents ambiguity between adjacent paths and file contents.
        result.update(len(part).to_bytes(8, "big"))
        result.update(part)
    return result.hexdigest()


def resolve_program(program, cwd, *, binary=None, path=None):
    path = os.environ.get("PATH", os.defpath) if path is None else path
    if program.startswith("vqro-") and "/" not in program and "\\" not in program:
        binary = binary or shutil.which("vqro", path=path)
        if binary:
            sibling = Path(binary).resolve().parent / program
            if sibling.is_file() and os.access(sibling, os.X_OK):
                return sibling
    if "/" in program or "\\" in program:
        candidate = Path(program)
        candidate = candidate if candidate.is_absolute() else Path(cwd) / candidate
        return candidate if candidate.is_file() and os.access(candidate, os.X_OK) else None
    found = shutil.which(program, path=path)
    return Path(found).resolve() if found else None


def script_path(declaration, extensions_dir):
    argv = declaration.get("argv") or []
    if len(argv) > 1 and Path(argv[0]).name.startswith("python"):
        # -m and -c do not name script files; skip interpreter flags such as -u / -B.
        for arg in argv[1:]:
            if arg in ("-m", "-c"):
                return None
            if not arg.startswith("-"):
                path = Path(arg)
                return path if path.is_absolute() else Path(declaration.get("cwd") or extensions_dir) / path
    return None


def fingerprint(extensions_dir, declarations, hello):
    root = Path(extensions_dir)
    files = sorted(set(root.glob("*.py")) | set(root.glob("manifests/*.json")) |
                   ({root / "defaults.json"} if (root / "defaults.json").is_file() else set()))
    parts = []
    for path in files:
        parts.extend((path.relative_to(root).as_posix().encode(), path.read_bytes()))
    individual = {}
    for key, declaration in sorted(declarations.items()):
        name = key.removeprefix("extension/")
        chunks = [canonical(declaration)]
        script = script_path(declaration, root)
        manifest = Path(declaration.get("manifest") or f"manifests/{name}.json")
        if not manifest.is_absolute():
            manifest = root / manifest
        for path in (script, manifest):
            if path is not None:
                chunks.extend((str(path).encode(), path.read_bytes() if path.is_file() else b"<missing>"))
        individual[name] = digest(chunks)
    return {"vqro": {"server": hello.get("server"), "version": hello.get("version")}, "extensions_dir": digest(parts),
            "declarations": digest([canonical(declarations)]), "extensions": individual}


def receipt(rows, *, source, started_at, started, fingerprint_value, inputs, not_covered,
            extension_count, strict=False, error=False):
    counts = {status: sum(r["status"] == status for r in rows) for status in STATUSES}
    if error or not extension_count:
        verdict = "error"
    elif counts["fail"] or (strict and counts["warn"]):
        verdict = "fail"
    elif not any(r["status"] == "pass" and r["id"].startswith("status/") for r in rows):
        verdict = "unverified"
    else:
        verdict = "pass"
    result = {"kind": "vqro.test-receipt", "version": 1, "suite": "unit", "verdict": verdict,
              "source": source, "started_at": started_at, "elapsed_ms": round((time.monotonic() - started) * 1000),
              "fingerprint": fingerprint_value, "inputs": inputs, "rows": rows, "counts": counts,
              "not_covered": not_covered,
              "skipped": [{"id": r["id"], "why": r["evidence"]} for r in rows if r["status"] == "skip" ]}
    if len(json.dumps(result, ensure_ascii=False, indent=2).encode()) > RECEIPT_BYTES:
        # Never publish a partial report as a successful complete run.
        return receipt([row("receipt", "harness", "fail", "receipt exceeds size bound")], source=source,
                       started_at=started_at, started=started, fingerprint_value={}, inputs=[],
                       not_covered=["report exceeded receipt bound; no complete result"], extension_count=0, error=True)
    return result


def utc_now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def write_receipt(result, destination):
    destination = Path(destination)
    fd = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as stream:
        os.fchmod(stream.fileno(), 0o600)
        json.dump(result, stream, ensure_ascii=False, indent=2)
        stream.write("\n")
    return str(destination)


def save_receipt(result, state_dir):
    directory = Path(state_dir) / "test-unit"
    directory.mkdir(parents=True, exist_ok=True, mode=0o700)
    stamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%S%fZ")
    destination = write_receipt(result, directory / f"receipt-{stamp}.json")
    for path in sorted(directory.glob("receipt-*.json"), reverse=True)[20:]:
        path.unlink()
    return destination


class PlainClient:
    """One-shot read client; unlike the extension SDK, transport EOF never exits the process."""
    MAX_RECORD_BYTES = 64 * 1024 * 1024
    ALLOWED = {"hello", "extension.list", "workspace.snapshot"}

    def __init__(self, path, timeout=15):
        self.socket = socket.socket(socket.AF_UNIX)
        self.timeout = timeout
        self.socket.settimeout(timeout)
        try:
            self.socket.connect(path)
            self.stream = self.socket.makefile("rwb", buffering=0)
        except Exception:
            self.socket.close()
            raise
        self.next_id = 0

    def call(self, method, params=None):
        if method not in self.ALLOWED:
            raise ValueError("health client only permits read methods")
        self.next_id += 1
        request = {"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params or {}}
        self.socket.settimeout(self.timeout)
        self.socket.sendall(canonical(request) + b"\n")
        deadline = time.monotonic() + self.timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("core response deadline")
            self.socket.settimeout(remaining)
            line = self.stream.readline(self.MAX_RECORD_BYTES + 1)
            if not line or len(line) > self.MAX_RECORD_BYTES:
                raise ConnectionError("core disconnected or reply exceeded size bound")
            message = json.loads(line)
            if "method" in message or message.get("id") != self.next_id:
                continue
            if "error" in message:
                raise RuntimeError(text(message["error"]))
            return message["result"]

    def close(self):
        self.stream.close()
        self.socket.close()


def cli_parser():
    parser = argparse.ArgumentParser(description="Read-only health report for installed vqro extensions")
    for flag in ("json", "offline", "no-tests", "strict"):
        parser.add_argument("--" + flag, action="store_true")
    for flag in ("socket", "state-dir", "extensions-dir", "out"):
        parser.add_argument("--" + flag)
    return parser


def endpoints(args, env=None):
    env = os.environ if env is None else env
    home = Path(env.get("HOME", str(Path.home())))
    state = Path(args.state_dir or env.get("VQRO_STATE_DIR") or
                 str(Path(env.get("XDG_STATE_HOME", str(home / ".local/state"))) / "vqro"))
    inherited = env.get("VQRO_SOCKET")
    if env.get("VQRO_SOCKET_SOURCE") == "terminal" and env.get("VQRO_STATE_DIR"):
        inherited = None
    socket = args.socket or inherited or str(state / "vqro.sock")
    extensions = Path(args.extensions_dir or env.get("VQRO_EXTENSIONS_DIR") or home / ".local/share/vqro/extensions")
    return str(socket), state, extensions


def print_result(result, as_json=False):
    if as_json:
        print(json.dumps(result, ensure_ascii=False))
        return
    print(f"{result['verdict'].upper()} ({result['source']}) — {result['counts']}")
    for item in result["rows"]:
        print(f"{item['status']:10} {item['id']}: {item['evidence']}")
    print("Not covered: " + "; ".join(result["not_covered"]))


CREDENTIAL = re.compile(r"auth|token|credential|secret|password", re.I)


def contains_credentials(value):
    if isinstance(value, dict):
        return any(CREDENTIAL.search(str(k)) or contains_credentials(v) for k, v in value.items())
    if isinstance(value, list):
        return any(contains_credentials(v) for v in value)
    return isinstance(value, str) and bool(CREDENTIAL.search(value))


class SandboxClient:
    """Bounded synchronous RPC and event reader, restricted to one disposable root."""
    def __init__(self, path, root, timeout=15):
        if not Path(path).resolve().is_relative_to(Path(root).resolve()):
            raise ValueError("sandbox endpoint outside root")
        self.socket = socket.socket(socket.AF_UNIX)
        self.socket.settimeout(timeout)
        try:
            self.socket.connect(str(path))
        except Exception:
            self.socket.close()
            raise
        self.timeout, self.next_id = timeout, 0
        self.buffer = bytearray()
        self.events = deque()
        self.event_bytes = 0

    def close(self):
        self.socket.close()

    def read_message(self, deadline):
        while b"\n" not in self.buffer:
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not select.select([self.socket], [], [], remaining)[0]:
                raise TimeoutError("sandbox response/event deadline")
            data = self.socket.recv(65536)
            if not data:
                raise ConnectionError("sandbox core disconnected")
            self.buffer.extend(data)
            if len(self.buffer) > PlainClient.MAX_RECORD_BYTES:
                raise ValueError("sandbox record exceeds size bound")
        line, _, rest = self.buffer.partition(b"\n")
        self.buffer = bytearray(rest)
        return json.loads(line)

    def call(self, method, params=None, timeout=None):
        self.next_id += 1
        ident = self.next_id
        deadline = min(time.monotonic() + (self.timeout if timeout is None else timeout),
                       getattr(self, "step_deadline", float("inf")))
        self.socket.sendall(canonical({"jsonrpc": "2.0", "id": ident, "method": method, "params": params or {}}) + b"\n")
        while True:
            message = self.read_message(deadline)
            if message.get("method"):
                size = len(canonical(message))
                self.event_bytes += size
                if self.event_bytes > 8 * 1024 * 1024:
                    raise ValueError("sandbox event backlog exceeded bound")
                self.events.append((message, size))
            elif message.get("id") == ident:
                if "error" in message:
                    raise RuntimeError(text(message["error"]))
                return message["result"]

    def next_event(self, deadline):
        if self.events:
            message, size = self.events.popleft()
            self.event_bytes -= size
            return message
        return self.read_message(deadline)

    def subscribe(self):
        snapshot = self.call("workspace.snapshot")
        self.call("events.subscribe", {"from": snapshot["seq"], "epoch": snapshot["epoch"]})
        return snapshot

    def wait(self, observe, predicate, timeout):
        deadline, last = time.monotonic() + timeout, None
        previous_deadline = getattr(self, "step_deadline", float("inf"))
        self.step_deadline = min(previous_deadline, deadline)
        try:
            while True:
                last = observe()
                if predicate(last):
                    return last
                self.next_event(deadline)
                if time.monotonic() >= deadline:
                    raise TimeoutError("event deadline")
        except TimeoutError as error:
            # An observation RPC can expire too, not only the event read between observations.
            raise TimeoutError("deadline; last observed: " + text(last) + "; " + str(error)) from None
        finally:
            self.step_deadline = previous_deadline

    def apply(self, operations, note="test-integration: sandbox change"):
        entries = self.call("workspace.snapshot")["entries"]
        ops = []
        for operation in operations:
            op = dict(operation)
            op["expect"] = entries.get(op["key"], {}).get("version", 0)
            ops.append(op)
        return self.call("workspace.apply", {"ops": ops, "note": note})

    def invoke(self, command, args=None, timeout=15):
        return self.call("command.invoke", {"name": command, "args": args or {}}, timeout=timeout)


def process_identity(pid):
    try:
        fields = Path(f"/proc/{pid}/stat").read_text().rpartition(")")[2].split()
        return int(fields[19]), fields[0]
    except (OSError, ValueError, IndexError):
        return None


def private_processes(root, started_ticks):
    if not Path("/proc/self/environ").exists():
        return None
    found = []
    prefix = str(root).encode()
    for path in Path("/proc").iterdir():
        if not path.name.isdigit():
            continue
        identity = process_identity(path.name)
        if not identity or identity[0] < started_ticks or identity[1] == "Z":
            continue
        try:
            environ = (path / "environ").read_bytes().split(b"\0")
            owned = any(field.partition(b"=")[2].startswith(prefix + b"/") for field in environ)
            if owned:
                found.append({"pid": int(path.name), "start_ticks": identity[0],
                              "command": (path / "cmdline").read_bytes().replace(b"\0", b" ").decode("utf-8", "replace")[:300]})
        except (OSError, PermissionError):
            continue
    return found


def process_group_members(group, env):
    """Only process facts, never ambient server commands; zombies have already exited."""
    if os.name != "posix":
        return None
    result = subprocess.run(["ps", "-eo", "pid=,pgid=,stat=,args="], env=env,
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=5, check=True, text=True)
    found = []
    for line in result.stdout.splitlines():
        fields = line.split(None, 3)
        if len(fields) == 4 and int(fields[1]) == group and not fields[2].startswith("Z"):
            found.append({"pid": int(fields[0]), "command": fields[3][:300]})
    return found


class Sandbox:
    """Disposable copy, audited before launch. Cooperative isolation, not a security boundary."""
    def __init__(self, source_dir, snapshot, *, binary=None, root=None, backend="fake",
                 allow_external=False, keep=False, candidate_ext=None, candidate_ops=None):
        self.source_dir = Path(source_dir).resolve()
        self.snapshot = snapshot
        self.binary = os.path.abspath(binary or shutil.which("vqro") or Path.home() / ".local/bin/vqro")
        self.root = Path(root).resolve() if root else Path(tempfile.mkdtemp(prefix="vqro-it-"))
        self.backend, self.allow_external, self.keep = backend, allow_external, keep
        self.candidate_ext, self.candidate_ops = candidate_ext, candidate_ops or []
        self.ext, self.state, self.socket = self.root / "ext", self.root / "state", self.root / "p.sock"
        self.env = {"HOME": str(self.root / "home"), "PATH": os.environ.get("PATH", os.defpath), "LANG": "C.UTF-8",
                    "VQRO_STATE_DIR": str(self.state), "VQRO_SOCKET": str(self.socket), "VQRO_EXTENSIONS_DIR": str(self.ext),
                    "XDG_RUNTIME_DIR": str(self.root / "xrun"), "XDG_STATE_HOME": str(self.root / "xstate"),
                    "XDG_CONFIG_HOME": str(self.root / "config"), "HC_SOCKET": str(self.root / "hc.sock"),
                    "HC_SESSION": "sandbox"}
        if backend == "fake":
            self.env["VQRO_HERDR"] = '["/bin/false"]'
        self.process = self.client = self.log = None
        self.groups, self.declarations, self.audit_rows = [], {}, []
        self.info = {"root": str(self.root), "backend": backend, "started": False, "not_copied": [],
                     "canary": {"status": "not run", "checked": {"env": [], "paths": {}}, "violations": []},
                     "excluded": [], "rebased": [], "vqro": None}
        self.leftovers, self.lifetime_rows = [], []
        self.started_ticks = int(float(Path("/proc/uptime").read_text().split()[0]) * os.sysconf("SC_CLK_TCK")) if Path("/proc/uptime").exists() else 0
        self._created = root is None

    def copy_tree(self, source, destination):
        source = Path(source).resolve()

        # Preserve source links without dereferencing; an overlay must not follow a destination link.
        destination = Path(destination)
        if destination.is_symlink():
            destination.unlink()
        destination.mkdir(parents=True, exist_ok=True)
        for path in source.iterdir():
            target = destination / path.name
            if target.is_symlink():
                target.unlink()
            if path.is_symlink():
                if target.is_dir():
                    shutil.rmtree(target)
                elif target.exists():
                    target.unlink()
                target.symlink_to(os.readlink(path))
            elif path.is_dir():
                self.copy_tree(path, target)
            else:
                shutil.copy2(path, target)

    def prepare(self):
        if self.root.exists() and not self._created and any(self.root.iterdir()):
            raise ValueError("sandbox root must be absent or empty")
        self.root.mkdir(parents=True, exist_ok=True, mode=0o700)
        self._created = True
        for name in ("home", "state", "xrun", "xstate", "config"):
            (self.root / name).mkdir(mode=0o700)
        self.copy_tree(self.source_dir, self.ext)
        if self.candidate_ext:
            self.copy_tree(self.candidate_ext, self.ext)
        values = {}
        for key, entry in self.snapshot.get("entries", {}).items():
            value = entry.get("value")
            if not key.startswith(("extension/", "doc/")):
                continue
            if (isinstance(value, dict) and "owner" in value) or contains_credentials({key: value}):
                self.info["excluded"].append(key)
            else:
                values[key] = json.loads(json.dumps(value))
        for op in self.candidate_ops:
            key = op["key"]
            if not key.startswith(("extension/", "doc/")) or contains_credentials({key: op.get("value")}):
                raise ValueError("candidate ops may only change credential-free declarations/documents")
            if op["op"] == "set":
                values[key] = op["value"]
            elif op["op"] == "delete":
                values.pop(key, None)
            else:
                raise ValueError("unsupported candidate operation")
        for key, value in list(values.items()):
            if key.startswith("extension/"):
                values[key] = self.audit(key.removeprefix("extension/"), value)
        self.declarations = {k: v for k, v in values.items() if k.startswith("extension/")}
        self.values = values
        self.canary()
        return self

    def audit(self, name, declaration):
        value = json.loads(json.dumps(declaration))
        external = []

        def rebase(item):
            path = Path(item)
            if not path.is_absolute():
                if ".." in path.parts:
                    external.append(item)
                return item
            resolved = path.resolve()
            if resolved.is_relative_to(self.source_dir):
                target = str(self.ext / resolved.relative_to(self.source_dir))
                self.info["rebased"].append({"from": item, "to": target})
                return target
            if resolved.is_relative_to(self.ext):
                return str(resolved)
            external.append(item)
            return item
        argv = value.get("argv") or []
        value["argv"] = [rebase(arg) for arg in argv]
        for field in ("cwd", "manifest"):
            if isinstance(value.get(field), str):
                value[field] = rebase(value[field])
        value.setdefault("cwd", str(self.ext))
        script = script_path(value, self.ext)
        if script is not None and not script.resolve().is_relative_to(self.ext):
            external.append(str(script))
        provider = value.get("kind") == "provider" or bool(argv and Path(argv[0]).name in ("vqro-herdr", "vqro-tmux"))
        # Explicit provider endpoint flags could bypass the private environment.
        if provider and any(flag in argv for flag in ("--tmux-socket", "--hc", "--hc-arg", "--session")):
            external.append("provider endpoint override")
        if external and not self.allow_external:
            value["enabled"] = False
        if provider and self.backend == "fake":
            value["enabled"] = False
        status = "unverified" if external and not self.allow_external else "pass"
        why = "external launch paths kept off: " + ", ".join(external) if status == "unverified" else "launch paths rebased/contained"
        if provider and self.backend == "fake":
            why += "; provider disabled for fake backend"
        self.audit_rows.append(row("audit", name, status, why))
        return value

    def canary(self):
        if len(os.fsencode(self.socket)) >= 100:
            self.info["canary"].update(status="fail", violations=[{"what": "socket path too long", "value": str(self.socket)}])
            raise ValueError("sandbox socket path must be under 100 bytes")
        self.info["canary"]["checked"] = {"env": sorted(self.env), "paths": {
            key: value for key, value in self.env.items() if key.startswith(("VQRO_", "HC_", "XDG_"))
            and key not in ("VQRO_HERDR", "HC_SESSION")}}
        code = (
            "import os,pathlib; r=pathlib.Path(os.environ['HOME']).parent.resolve(); "
            "assert not any(k.startswith(('HERDR_', 'TMUX')) for k in os.environ); "
            "names=[k for k in os.environ if k.startswith(('VQRO_', 'HC_', 'XDG_')) "
            "and k not in ('VQRO_HERDR','HC_SESSION')]; "
            "assert all(pathlib.Path(os.environ[k]).resolve().is_relative_to(r) for k in names); "
            "assert pathlib.Path(os.environ['VQRO_SOCKET']).parent==r; "
            "assert pathlib.Path(os.environ['VQRO_STATE_DIR']).parent==r"
        )
        probe = subprocess.run([sys.executable, "-B", "-c", code], env=self.env,
                               stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10, check=False)
        self.info["canary"].update(status="pass" if probe.returncode == 0 else "fail")
        if probe.returncode:
            evidence = probe.stderr.decode("utf-8", "replace")[-300:]
            self.info["canary"]["violations"].append({"what": "environment probe failed", "value": evidence})
            raise ValueError("sandbox environment canary failed: " + evidence)

    def start(self, seed=True):
        if self.info["canary"]["status"] != "pass":
            raise ValueError("sandbox canary must pass before launch")
        self.log = (self.root / "core.log").open("ab")
        argv = [self.binary, "serve", "--safe"] + (["--fake"] if self.backend == "fake" else [])
        self.process = subprocess.Popen(argv, cwd=self.root, env=self.env, stdout=self.log,
                                        stderr=subprocess.STDOUT, start_new_session=True)
        self.groups.append(self.process.pid)
        self.info["started"] = True
        deadline, error = time.monotonic() + 60, None
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise RuntimeError("sandbox core exited: " + str(self.process.returncode))
            try:
                self.client = SandboxClient(self.socket, self.root)
                hello = self.client.call("hello", {"client": "test-integration", "identity": {"kind": "owner"}})
                self.info["vqro"] = {"server": hello.get("server"), "version": hello.get("version")}
                self.client.subscribe()
                break
            except (OSError, ConnectionError) as failure:
                error = failure
                if self.client:
                    self.client.close()
                    self.client = None
                time.sleep(0.05)
        else:
            raise TimeoutError("sandbox readiness: " + str(error))
        if seed:
            current = self.client.call("workspace.snapshot")["entries"]
            operations = [{"op": "delete", "key": key} for key in current
                          if key.startswith(("extension/", "doc/")) and key not in self.values]
            operations += [{"op": "set", "key": key, "value": value} for key, value in self.values.items()]
            try:
                self.client.apply(operations, "test-integration: seed private copy")
            except RuntimeError:
                ordinary = [op for op in operations if op["key"].startswith("extension/")
                            or op["key"].split("/")[:2] in (["doc", "settings"], ["doc", "vqro"])]
                if ordinary:
                    self.client.apply(ordinary, "test-integration: seed ordinary copy")
                for op in operations:
                    if op in ordinary:
                        continue
                    try:
                        self.client.apply([op], "test-integration: seed optional document")
                    except RuntimeError as failure:
                        self.info["not_copied"].append({"key": op["key"], "error": text(str(failure))})
        self.client.invoke("vqro.safe_mode", {"on": False})
        return self

    def enabled(self):
        return [key.removeprefix("extension/") for key, value in self.declarations.items() if value.get("enabled", True)]

    def wait_running(self, timeout=60):
        names = self.enabled()

        def observe():
            return {"extensions": self.client.call("extension.list")["extensions"],
                    "commands": [c["name"] for c in self.client.call("command.list")["commands"] if c.get("available", True)]}

        def ready(observed):
            facts, commands = observed["extensions"], observed["commands"]
            if any(facts.get(name, {}).get("display_status") == "failed" for name in names):
                raise TimeoutError("extension failed before readiness")
            for name in names:
                fact = facts.get(name, {})
                if fact.get("display_status") != "running":
                    return False
                for capability in (fact.get("manifest") or {}).get("provides", []):
                    if capability.startswith("command/") and not any(
                            fnmatchcase(command, capability[8:].split("@")[0]) for command in commands):
                        return False
            return True

        return self.client.wait(observe, ready, timeout)["extensions"]

    def stop(self):
        if self.client:
            try:
                self.client.call("core.shutdown")
            except Exception as error:
                self.lifetime_rows.append(row("lifetime", "core", "fail", str(error)))
            finally:
                self.client.close()
                self.client = None
        deadline = time.monotonic() + 10
        if self.process:
            try:
                self.process.wait(timeout=max(0, deadline - time.monotonic()))
            except subprocess.TimeoutExpired:
                pass
        for group in self.groups:
            members = process_group_members(group, self.env)
            while members and time.monotonic() < deadline:
                time.sleep(0.05)
                members = process_group_members(group, self.env)
            if members is None:
                self.lifetime_rows.append(row("lifetime", "unavailable", "unverified", "process-group facts unavailable"))
            elif members:
                self.leftovers.extend(members)
                try:
                    os.killpg(group, signal.SIGTERM)
                    time.sleep(0.1)
                    if process_group_members(group, self.env):
                        os.killpg(group, signal.SIGKILL)
                except ProcessLookupError:
                    pass
        self.groups.clear()
        if self.info["started"] and self.backend == "private":
            for argv, endpoint in [(["hc", "server", "stop"], Path(self.env["HC_SOCKET"])),
                                   (["tmux", "-N", "-S", str(self.state / "tmux.sock"), "kill-server"], self.state / "tmux.sock")]:
                enabled_backend = any(Path((v.get("argv") or [""])[0]).name == ("vqro-herdr" if argv[0] == "hc" else "vqro-tmux")
                                      and v.get("enabled", True) for v in self.declarations.values())
                if endpoint.exists():
                    try:
                        subprocess.run(argv, env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10, check=False)
                    except (OSError, subprocess.TimeoutExpired) as error:
                        self.lifetime_rows.append(row("lifetime", "backend", "fail", str(error)))
                elif enabled_backend:
                    self.lifetime_rows.append(row("lifetime", "backend", "unverified", f"enabled private backend endpoint never appeared: {endpoint}"))
        if self.info["started"]:
            detached = private_processes(self.root, self.started_ticks)
            if detached is None:
                self.lifetime_rows.append(row("lifetime", "unavailable", "unverified", "private process facts unavailable"))
            else:
                self.leftovers.extend(detached)
                for process in detached:
                    for sig in (signal.SIGTERM, signal.SIGKILL):
                        identity = process_identity(process["pid"])
                        if identity is None or identity[0] != process["start_ticks"] or identity[1] == "Z":
                            break
                        try:
                            os.kill(process["pid"], sig)
                        except ProcessLookupError:
                            break
                        time.sleep(0.1)
            attributed = {name: [] for name in self.enabled()}
            unattributed = []
            for process in self.leftovers:
                owners = []
                for name in attributed:
                    declaration = self.declarations["extension/" + name]
                    script = script_path(declaration, self.ext)
                    if script is not None and (str(script) in process["command"] or script.name in process["command"].split()):
                        owners.append(name)
                if owners:
                    for name in owners:
                        attributed[name].append(process)
                else:
                    unattributed.append(process)
            for name, processes in attributed.items():
                self.lifetime_rows.append(row("lifetime", name, "fail" if processes else "pass",
                                              processes or "no surviving processes attributed to this extension"))
            if unattributed:
                self.lifetime_rows.append(row("lifetime", "unattributed", "fail", unattributed))
        if self.process:
            self.process.wait(timeout=10)
            self.process = None
        if self.log:
            self.log.close()
            self.log = None

    def cleanup(self):
        try:
            self.stop()
        finally:
            if self._created and not self.keep and self.root.exists():
                shutil.rmtree(self.root)


def smoke(sandbox, *, readiness=60, window=10):
    rows, client = [], sandbox.client
    names = sandbox.enabled()
    if not names:
        return [row("start", "harness", "fail", "zero enabled extensions; no smoke coverage")]
    try:
        sandbox.wait_running(readiness)
    except TimeoutError as error:
        rows.append(row("start", "deadline", "fail", str(error)))
    # A real survival window, not a guessed readiness sleep; facts are read on both sides.
    before = client.call("extension.list")["extensions"]
    deadline = time.monotonic() + window
    while time.monotonic() < deadline:
        try:
            client.next_event(deadline)
        except TimeoutError:
            break
    facts = client.call("extension.list")["extensions"]
    snapshot = client.call("workspace.snapshot")
    commands = {c["name"] for c in client.call("command.list")["commands"] if c.get("available", True)}
    live = snapshot.get("live", {})
    total = sum(len(canonical(content)) for content in live.values())
    for name in names:
        fact, previous = facts.get(name, {}), before.get(name, {})
        healthy = (fact.get("display_status") == "running" and not fact.get("crashes", 0)
                   and previous.get("display_status") == "running"
                   and fact.get("generation") == previous.get("generation"))
        rows.append(row("start", name, "pass" if healthy else "fail", fact if not healthy else "running, 0 crashes across survival window"))
        deps = fact.get("dependencies")
        rows.append(row("deps", name, {"ok": "pass", "missing": "fail", "degraded": "warn"}.get((deps or {}).get("status"), "skip"), deps or "no dependency facts"))
        provides = (fact.get("manifest") or {}).get("provides", [])
        missing_commands = [cap[8:].split("@")[0] for cap in provides if cap.startswith("command/")
                            and not any(fnmatchcase(c, cap[8:].split("@")[0]) for c in commands)]
        rows.append(row("commands", name, "fail" if missing_commands else "pass", missing_commands or "all provided commands registered; none invoked"))
        view_names = [cap[5:].split("@")[0] for cap in provides if cap.startswith("view/")]
        families = [name for name in view_names if name.endswith("*")]
        missing_views = [name for name in view_names if not name.endswith("*") and "view/" + name not in snapshot["entries"]]
        oversized = [view for view, content in live.items() if len(canonical(content)) > 1024 * 1024]
        log = sandbox.state / f"ext-{name}.log"
        rejection = False
        if log.is_file():
            with log.open("rb") as stream:
                stream.seek(max(0, log.stat().st_size - 65536))
                tail = stream.read(65536)
            rejection = b"too_large" in tail or b"live view" in tail and b"limit" in tail
        bad = missing_views or oversized or total > 8 * 1024 * 1024 or rejection
        evidence = {"missing": missing_views, "oversized": oversized, "total_bytes": total, "rejection_in_log": rejection}
        if families:
            evidence.update(families=families, coverage="view family needs live resources; not instantiated in the sandbox")
        rows.append(row("views", name, "fail" if bad else "unverified" if families else "pass", evidence))
    try:
        client.invoke("vqro.safe_mode", {"on": True})
        client.wait(lambda: client.call("extension.list")["extensions"], lambda f: all(not v.get("running") for v in f.values()), readiness)
        client.invoke("vqro.safe_mode", {"on": False})
        restored = sandbox.wait_running(readiness)
        ok = all(restored.get(n, {}).get("display_status") == "running" for n in names)
        rows.append(row("recovery", "safe_mode", "pass" if ok else "fail", "same enabled set restored" if ok else restored))
    except Exception as error:
        rows.append(row("recovery", "safe_mode", "fail", str(error)))
    try:
        applied = client.apply([{"op": "set", "key": "doc/test-integration/probe", "value": {"probe": True}}])
        client.invoke("vqro.revert", {"txn": applied["txn"]})
        absent = "doc/test-integration/probe" not in client.call("workspace.snapshot")["entries"]
        rows.append(row("recovery", "revert", "pass" if absent else "fail", "probe removed" if absent else "probe remains"))
    except Exception as error:
        rows.append(row("recovery", "revert", "fail", str(error)))
    try:
        declarations = {k: v["value"] for k, v in client.call("workspace.snapshot")["entries"].items() if k.startswith("extension/")}
        sandbox.stop()
        sandbox.start(seed=False)
        restored = sandbox.wait_running(readiness)
        after = {k: v["value"] for k, v in sandbox.client.call("workspace.snapshot")["entries"].items() if k.startswith("extension/")}
        ok = declarations == after and all(restored.get(n, {}).get("display_status") == "running" for n in names)
        rows.append(row("recovery", "restart", "pass" if ok else "fail", "declarations preserved and enabled set running" if ok else "declarations or running set changed"))
    except Exception as error:
        rows.append(row("recovery", "restart", "fail", str(error)))
    return rows


def subset_matches(actual, expected):
    if isinstance(expected, dict):
        return isinstance(actual, dict) and all(k in actual and subset_matches(actual[k], value) for k, value in expected.items())
    if isinstance(expected, list):
        return isinstance(actual, list) and len(actual) == len(expected) and all(subset_matches(a, b) for a, b in zip(actual, expected))
    return actual == expected


def scenario_observation(client, predicate):
    snapshot = client.call("workspace.snapshot")
    if "content" in predicate:
        key = predicate["content"]
        present = key in snapshot.get("live", {})
        value = snapshot.get("live", {}).get(key)
    elif "entry" in predicate:
        key = predicate["entry"]
        present = key in snapshot.get("entries", {})
        value = snapshot.get("entries", {}).get(key, {}).get("value")
    else:
        raise ValueError("predicate requires entry or content")
    path = predicate.get("path", [])
    if isinstance(path, str):
        path = path.split(".") if path else []
    for field in path:
        try:
            value = value[int(field)] if isinstance(value, list) else value[field]
        except (KeyError, IndexError, TypeError, ValueError):
            present, value = False, None
            break
    return {"present": present, "value": value}


def scenario_matches(observed, predicate):
    if "exists" in predicate and observed["present"] != predicate["exists"]:
        return False
    if "equals" in predicate and (not observed["present"] or observed["value"] != predicate["equals"]):
        return False
    if "contains" in predicate and (not observed["present"] or predicate["contains"] not in json.dumps(observed["value"], ensure_ascii=False)):
        return False
    return True


def discover_scenarios(extensions_dir):
    result = {}
    for path in sorted(Path(extensions_dir).glob("tests/*/scenarios/*.json")):
        key = f"{path.parent.parent.name}/{path.stem}"
        with path.open() as stream:
            value = json.load(stream)
        if not isinstance(value, dict) or not isinstance(value.get("steps"), list):
            raise ValueError(f"invalid scenario: {path}")
        result[key] = value
    return result


def run_scenarios(client, scenarios):
    rows = []
    for name, scenario in sorted(scenarios.items()):
        step_number = 0
        try:
            if not scenario["steps"]:
                raise ValueError("scenario has zero steps")
            for step_number, step in enumerate(scenario["steps"], 1):
                seconds = step.get("within_ms", 10000) / 1000
                if not 0 < seconds <= 120:
                    raise ValueError("step deadline must be between 1 and 120000 ms")
                client.step_deadline = time.monotonic() + seconds
                if "apply" in step:
                    client.apply(step["apply"], "test-integration: scenario " + name)
                elif "invoke" in step:
                    command = step["invoke"]
                    if command.startswith(("pi.", "codex.", "tier1.", "agents.")):
                        raise ValueError("runtime commands require the offline agent-profile follow-up; not invoked")
                    result = client.invoke(command, step.get("args"), timeout=seconds)
                    if "result" in step and not subset_matches(result, step["result"]):
                        raise AssertionError("command result mismatch; last observed: " + text(result))
                elif "wait" in step:
                    predicate = step["wait"]
                    client.wait(lambda: scenario_observation(client, predicate),
                                lambda observed: scenario_matches(observed, predicate), seconds)
                elif "expect" in step:
                    predicate = step["expect"]
                    observed = scenario_observation(client, predicate)
                    if not scenario_matches(observed, predicate):
                        raise AssertionError("expect mismatch; last observed: " + text(observed))
                else:
                    raise ValueError("unknown scenario step")
                if time.monotonic() > client.step_deadline:
                    raise TimeoutError("step deadline exceeded")
            rows.append(row("scenario", name, "pass", f"{len(scenario['steps'])} steps observed"))
        except Exception as error:
            rows.append(row("scenario", name, "fail", f"step {step_number}: {error}"))
        finally:
            client.step_deadline = float("inf")
    return rows


def compare_runs(baseline, candidate, scenarios):
    rows = []
    before = {r["id"]: r for r in baseline["rows"]}
    after = {r["id"]: r for r in candidate["rows"]}
    for name, scenario in sorted(scenarios.items()):
        ident = "scenario/" + name
        b, c = before.get(ident, {}).get("status"), after.get(ident, {}).get("status")
        if scenario.get("new"):
            status = "weak" if b == c == "pass" else "pass" if b == "fail" and c == "pass" else "fail"
            evidence = "new scenario passed both: no evidence" if status == "weak" else f"baseline {b}; candidate {c}"
        else:
            status = "pass" if b == c == "pass" else "warn" if b == c == "fail" else "fail"
            evidence = "already failing before the change" if status == "warn" else f"baseline {b}; candidate {c}"
        rows.append(row("compare/scenario", name, status, evidence))
    for ident, current in after.items():
        if not ident.startswith("scenario/") and current["status"] != "pass":
            regression = before.get(ident, {}).get("status") == "pass"
            rows.append(row("compare/smoke", ident, "fail" if regression else "warn",
                            "regression" if regression else "already failing before the change"))
    return rows