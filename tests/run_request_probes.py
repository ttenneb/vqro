#!/usr/bin/env python3
"""Diagnostic driver for the independent R3/R4 request probes (agent_conformance/request_probes.py).

Runs each probe in its own private public-core fixture, exactly as run_agent_conformance.py isolates a case
(user/network/PID/mount namespace, empty HOME, kill-child), and writes the probe witnesses. It never emits a Part A
qualification table. Its diagnostic labels cover these probes only: R4 here is the additional timeout vector,
not the existing unknown/oversized request vectors. Use the main runner for the integrated case.
"""
from python_interpreter import PYTHON
import argparse
import asyncio
import json
import os
from pathlib import Path
import subprocess
import shutil
import tempfile

from agent_conformance.results import BY_ID
from agent_conformance.request_probes import answer_acknowledgement, positive_relative_timeout
from agent_conformance.runner import run_case, validate_manifest

PROBES = {"R3": answer_acknowledgement, "R4": positive_relative_timeout}


async def diagnose(root, manifest, vqro, hc):
    rows = []
    for ident, probe in PROBES.items():
        result = await run_case(BY_ID[ident], root / ident, manifest, vqro, hc, handlers={ident: probe})
        row = result.__dict__ if hasattr(result, "__dict__") else dict(result)
        rows.append({"case": ident, "probe": probe.__name__, "status": row.get("status"), "component": row.get("component"),
                     "reason": row.get("reason"), "detail": row.get("detail"), "evidence": row.get("evidence"),
                     "declarations": row.get("declarations")})
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", action="store_true")
    parser.add_argument("--implementation", type=Path, required=True)
    repo = Path(__file__).resolve().parents[1]
    parser.add_argument("--vqro", type=Path, default=Path(os.environ.get("CARGO_TARGET_DIR") or repo / "target") / "debug/vqro")
    parser.add_argument("--hc", type=Path, default=Path(shutil.which("herdr-core") or "herdr-core").resolve())
    parser.add_argument("--scratch-parent", type=Path, default=repo / ".run")
    parser.add_argument("--isolated-root", type=Path, help=argparse.SUPPRESS)
    parser.add_argument("--parent-net", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if not args.run or not args.vqro.is_absolute() or not args.hc.is_absolute():
        parser.error("requires --run and absolute --vqro/--hc")
    manifest = validate_manifest(json.loads(args.implementation.read_text()))
    if not args.isolated_root:
        args.scratch_parent.mkdir(parents=True, exist_ok=True)
        root = Path(tempfile.mkdtemp(prefix="rq-", dir=args.scratch_parent))
        (root / ".private-conformance").touch()
        (root / "home").mkdir()
        copy = root / "implementation.json"
        copy.write_text(json.dumps(manifest, indent=2) + "\n")
        command = ["/usr/bin/unshare", "-Urnpmf", "--kill-child=KILL", "--mount-proc", PYTHON,
                   str(Path(__file__).resolve()), "--run", "--implementation", str(copy), "--vqro", str(args.vqro),
                   "--hc", str(args.hc), "--isolated-root", str(root), "--parent-net", os.readlink("/proc/self/ns/net")]
        try:
            code = subprocess.run(command, cwd=root, timeout=200, env={"HOME": str(root / "home"), "PATH": "/usr/bin:/bin",
                                  "LANG": "C.UTF-8", "PYTHONDONTWRITEBYTECODE": "1"}).returncode
        except subprocess.TimeoutExpired:
            code = 124
        print(json.dumps({"code": code, "probes": str(root / "request-probes.json")}), flush=True)
        raise SystemExit(code)
    if not (args.isolated_root / ".private-conformance").is_file() or args.parent_net in (None, os.readlink("/proc/self/ns/net")):
        parser.error("private root and namespace required")
    if any(key.startswith("HERDR_") for key in os.environ):
        parser.error("HERDR environment must not reach conformance processes")
    rows = asyncio.run(diagnose(args.isolated_root, manifest, args.vqro, args.hc))
    (args.isolated_root / "request-probes.json").write_text(json.dumps(
        {"diagnostic_only": True, "part_a_table": False, "real_runtime_certification": False, "probes": rows},
        indent=1, default=str) + "\n")
    for row in rows:
        print(json.dumps({key: row[key] for key in ("case", "probe", "status", "component", "reason", "detail")}), flush=True)


if __name__ == "__main__":
    main()
