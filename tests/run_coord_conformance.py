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

from agent_conformance.part_b.catalog import select_cases, cases_from_spec
from agent_conformance.part_b.runner import run_selection, validate_manifest, write_controller_timeout


def positive_seconds(value):
    try:
        seconds = int(value)
    except ValueError as error:
        raise argparse.ArgumentTypeError("selection timeout must be a positive integer") from error
    if seconds <= 0:
        raise argparse.ArgumentTypeError("selection timeout must be a positive integer")
    return seconds


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", action="store_true")
    parser.add_argument("--implementation", type=Path, required=True)
    repo = Path(__file__).resolve().parents[1]
    parser.add_argument("--vqro", type=Path, default=Path(os.environ.get("CARGO_TARGET_DIR") or repo / "target") / "debug/vqro")
    parser.add_argument("--cases")
    parser.add_argument("--selection-timeout", type=positive_seconds, default=900,
                        help="whole-selection controller seconds (default900); never changes per-case deadlines")
    parser.add_argument("--scratch-parent", type=Path, default=repo / ".run")
    parser.add_argument("--isolated-root", type=Path, help=argparse.SUPPRESS)
    parser.add_argument("--parent-net", help=argparse.SUPPRESS)
    args = parser.parse_args(argv)
    if not args.run: parser.error("requires --run; never uses an ambient/default service")
    if not args.vqro.is_absolute(): parser.error("--vqro must be an explicit absolute executable")
    try:
        manifest = {**validate_manifest(json.loads(args.implementation.read_text())),
                    "controller_timeout_seconds": args.selection_timeout}
        spec=manifest.get("specs",{}).get("conformance")
        if spec is None: raise ValueError("explicit pinned conformance specification is required")
        inventory=cases_from_spec(Path(spec).read_text())
        selected = select_cases(args.cases.split(",") if args.cases else None,inventory)
    except (ValueError, OSError) as error: parser.error(str(error))
    if args.isolated_root is None:
        args.scratch_parent.mkdir(parents=True, exist_ok=True)
        root = Path(tempfile.mkdtemp(prefix="b-", dir=args.scratch_parent))
        (root/".private-coord-conformance").touch(); (root/"home").mkdir()
        staged = root/"implementation.json"; staged.write_text(json.dumps(manifest, indent=2)+"\n")
        command = ["/usr/bin/unshare", "-Urnpmf", "--kill-child=KILL", "--mount-proc", "/usr/bin/python3", str(Path(__file__).resolve()), "--run",
            "--implementation", str(staged), "--vqro", str(args.vqro), "--isolated-root", str(root),
            "--parent-net", os.readlink("/proc/self/ns/net"), "--selection-timeout", str(args.selection_timeout)]
        if args.cases: command += ["--cases", args.cases]
        try:
            process = subprocess.run(command, cwd=root, timeout=args.selection_timeout, env={"HOME": str(root/"home"), "PATH": "/usr/bin:/bin",
                "LANG": "C.UTF-8", "PYTHONDONTWRITEBYTECODE": "1"})
            code=process.returncode
        except subprocess.TimeoutExpired:
            code=124
            write_controller_timeout(root, manifest, selected, inventory, args.selection_timeout)
        output = {"exit": code, "root": str(root), "controller_timeout_seconds": args.selection_timeout}
        for key, name in (("results", "results.json"), ("table", "results.md"), ("timeout", "parent-timeout.json")):
            if (root / name).is_file(): output[key] = str(root / name)
        print(json.dumps(output))
        return code
    if not (args.isolated_root/".private-coord-conformance").is_file(): parser.error("private-root marker missing")
    if args.parent_net is None or args.parent_net == os.readlink("/proc/self/ns/net"):
        parser.error("private user/network namespace required")
    if any(key.startswith("HERDR_") for key in os.environ): parser.error("HERDR environment inherited")
    document = asyncio.run(run_selection(args.isolated_root, manifest, selected, args.vqro,inventory=inventory))
    print(json.dumps({"summary": document["summary"], "qualification_complete": document["qualification_complete"],
                      "complete_inventory": document["complete_inventory"], "real_runtime_certification": False}))
    return 0 if document["passed_selection"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
