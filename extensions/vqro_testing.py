"""Shared, stdlib-only receipt and CLI mechanisms for installed-setup checks."""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import shutil
import socket
import time

STATUSES = ("pass", "warn", "fail", "skip", "unverified")
EXIT_CODES = {"pass": 0, "fail": 1, "unverified": 77, "error": 2}
EVIDENCE_CHARS = 300
RECEIPT_BYTES = 750 * 1024


def text(value):
    if not isinstance(value, str):
        value = json.dumps(value, sort_keys=True, ensure_ascii=False)
    return value[:EVIDENCE_CHARS]


def row(check, subject, status, evidence, advice=None):
    if status not in STATUSES:
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
    return {"vqro": hello.get("software", hello.get("version")), "extensions_dir": digest(parts),
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
