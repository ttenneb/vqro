#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Bennett Garcia
"""Opt-in, network-isolated public-core Part B coordination conformance."""
import argparse
import asyncio
import json
import os
from pathlib import Path
import subprocess
import tempfile

from agent_conformance.part_b.catalog import select_cases
from agent_conformance.part_b.runner import run_selection, validate_manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", action="store_true")
    parser.add_argument("--implementation", type=Path, required=True)
    parser.add_argument("--vqro", type=Path, required=True)
    parser.add_argument("--cases")
    parser.add_argument("--scratch-parent", type=Path, default=Path.home()/".local/share/herdr/scratch/prim-pi1-check/pb")
    parser.add_argument("--isolated-root", type=Path, help=argparse.SUPPRESS)
    parser.add_argument("--parent-net", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if not args.run: parser.error("requires --run; never uses an ambient/default service")
    if not args.vqro.is_absolute(): parser.error("--vqro must be an explicit absolute executable")
    try:
        manifest = validate_manifest(json.loads(args.implementation.read_text()))
        selected = select_cases(args.cases.split(",") if args.cases else None)
    except (ValueError, OSError) as error: parser.error(str(error))
    if args.isolated_root is None:
        args.scratch_parent.mkdir(parents=True, exist_ok=True)
        root = Path(tempfile.mkdtemp(prefix="b-", dir=args.scratch_parent))
        (root/".private-coord-conformance").touch(); (root/"home").mkdir()
        staged = root/"implementation.json"; staged.write_text(json.dumps(manifest, indent=2)+"\n")
        command = ["/usr/bin/unshare", "-Urn", "/usr/bin/python3", str(Path(__file__).resolve()), "--run",
            "--implementation", str(staged), "--vqro", str(args.vqro), "--isolated-root", str(root),
            "--parent-net", os.readlink("/proc/self/ns/net")]
        if args.cases: command += ["--cases", args.cases]
        process = subprocess.run(command, cwd=root, env={"HOME": str(root/"home"), "PATH": "/usr/bin:/bin",
            "LANG": "C.UTF-8", "PYTHONDONTWRITEBYTECODE": "1"})
        print(json.dumps({"exit": process.returncode, "results": str(root/"results.json"), "table": str(root/"results.md")}))
        return process.returncode
    if not (args.isolated_root/".private-coord-conformance").is_file(): parser.error("private-root marker missing")
    if args.parent_net is None or args.parent_net == os.readlink("/proc/self/ns/net"):
        parser.error("private user/network namespace required")
    if any(key.startswith("HERDR_") for key in os.environ): parser.error("HERDR environment inherited")
    document = asyncio.run(run_selection(args.isolated_root, manifest, selected, args.vqro))
    print(json.dumps({"summary": document["summary"], "qualification_complete": document["qualification_complete"],
                      "complete_inventory": document["complete_inventory"], "real_runtime_certification": False}))
    return 0 if document["passed_selection"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
