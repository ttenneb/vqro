#!/usr/bin/env python3
"""Opt-in public-core conformance with scripted native runtimes, never models.

Results always list the selected/omitted normative cases. A partial green run or
an ACP/fake model is not adapter certification. See agent_conformance/README.md.
"""
import argparse
import asyncio
import json
import os
from pathlib import Path
import subprocess
import tempfile

from agent_conformance.results import select_cases
from agent_conformance.runner import run_selection, validate_manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--run", action="store_true", help="explicitly allow private backend subprocesses")
    parser.add_argument("--implementation", type=Path, required=True, help="JSON name/argv/wire/version manifest")
    parser.add_argument("--vqro", type=Path, required=True)
    parser.add_argument("--hc", type=Path, required=True)
    parser.add_argument("--cases", help="comma-separated Part A groups; omitted groups stay visible")
    parser.add_argument("--scratch-parent", type=Path, default=Path.home() / ".local/share/herdr/scratch/spike-c")
    parser.add_argument("--isolated-root", type=Path, help=argparse.SUPPRESS)
    parser.add_argument("--parent-net", help=argparse.SUPPRESS)
    args = parser.parse_args()
    if not args.run:
        parser.error("requires --run; never contacts the ambient/default server")
    if not args.vqro.is_absolute() or not args.hc.is_absolute():
        parser.error("--vqro and --hc must be explicit absolute executable paths")
    try:
        manifest = validate_manifest(json.loads(args.implementation.read_text()))
        selected = select_cases(args.cases.split(",") if args.cases else None)
    except (OSError, ValueError) as error:
        parser.error(str(error))
    if not args.isolated_root:
        args.scratch_parent.mkdir(parents=True, exist_ok=True)
        root = Path(tempfile.mkdtemp(prefix="k-", dir=args.scratch_parent))
        (root / ".private-conformance").touch()
        (root / "home").mkdir()
        copy = root / "implementation.json"
        copy.write_text(json.dumps(manifest, indent=2) + "\n")
        # A killed namespace init kills every descendant, including detached backends.
        command = ["/usr/bin/unshare", "-Urnpmf", "--kill-child=KILL", "--mount-proc", "/usr/bin/python3", str(Path(__file__).resolve()), "--run",
                   "--implementation", str(copy), "--vqro", str(args.vqro), "--hc", str(args.hc),
                   "--isolated-root", str(root), "--parent-net", os.readlink("/proc/self/ns/net")]
        if args.cases:
            command += ["--cases", args.cases]
        try:
            result = subprocess.run(command, cwd=root, timeout=30 + 70 * len(selected),
                env={"HOME": str(root / "home"), "PATH": "/usr/bin:/bin", "LANG": "C.UTF-8",
                     "PYTHONDONTWRITEBYTECODE": "1"})
            code = result.returncode
        except subprocess.TimeoutExpired:
            code = 124
            (root / "parent-timeout.json").write_text(json.dumps({"status": "fail", "component": "cleanup",
                "detail": "private PID namespace exceeded whole-run deadline; namespace killed",
                "complete_inventory": False}) + "\n")
        print(json.dumps({"code": code, "results": str(root / "results.json"),
                          "table": str(root / "results.md")}), flush=True)
        raise SystemExit(code)
    if not (args.isolated_root / ".private-conformance").is_file():
        parser.error("isolated root marker missing")
    if args.parent_net == os.readlink("/proc/self/ns/net") or args.parent_net is None:
        parser.error("private user/network namespace required")
    if any(key.startswith("HERDR_") for key in os.environ):
        parser.error("HERDR environment must not reach conformance processes")
    document = asyncio.run(run_selection(args.isolated_root, manifest, selected, args.vqro, args.hc))
    print(json.dumps({"summary": document["summary"], "complete_inventory": document["complete_inventory"],
                      "real_runtime_certification": False}), flush=True)
    raise SystemExit(0 if document["passed_selection"] else 1)


if __name__ == "__main__":
    main()
