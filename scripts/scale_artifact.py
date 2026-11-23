#!/usr/bin/env python3
"""Select a scale test from explicit prebuild Cargo JSON, or validate a caller's path.

This helper NEVER invokes Cargo. `select` reads the JSON stream of an explicit
`cargo test --release --test scale --no-run --message-format=json` prebuild.
`validate` is safe inside timing admission: it only reads the nominated file.
Both print the chosen canonical executable path and optionally record its hash/mtime.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import sys

PREBUILD = "prebuild the release scale test and pass its path (VQRO_SCALE_TEST_EXE)"


def validate_path(executable, target):
    if any(char in str(executable) for char in "\r\n"):
        raise ValueError("scale executable path contains a line break")
    root = Path(target).resolve(strict=True)
    if not root.is_dir():
        raise ValueError("CARGO_TARGET_DIR is not a directory")
    path = Path(executable).resolve(strict=True)
    if not path.is_relative_to(root):
        raise ValueError("scale executable must be inside CARGO_TARGET_DIR")
    if not path.is_file() or not os.access(path, os.X_OK):
        raise ValueError("scale test path is not an executable file")
    return path


def select_artifact(messages, manifest, target):
    """Use package/target identity, never an inventory of old hashed filenames.

    Fresh=false is allowed HERE: the caller explicitly prebuilt outside timing.
    A stable Cargo --no-run command is not a dry run, so timing never issues it.
    """
    manifest = Path(manifest).resolve(strict=True)
    source = (manifest.parent / "tests/scale.rs").resolve(strict=True)
    selected, finished = [], False
    for row in messages:
        if not isinstance(row, dict):
            raise ValueError("invalid Cargo JSON message")
        if finished:
            raise ValueError("Cargo prebuild has records after terminal build-finished")
        if row.get("reason") == "build-finished":
            if row.get("success") is not True:
                raise ValueError("scale prebuild failed")
            finished = True
        if row.get("reason") != "compiler-artifact":
            continue
        spec, profile = row.get("target", {}), row.get("profile", {})
        if not isinstance(spec, dict) or not isinstance(profile, dict):
            raise ValueError("invalid Cargo artifact target/profile")
        if (row.get("manifest_path") and Path(row["manifest_path"]).resolve() == manifest
            and spec.get("name") == "scale" and spec.get("kind") == ["test"]
            and spec.get("src_path") and Path(spec["src_path"]).resolve() == source
            and profile.get("test") is True and row.get("executable")):
            selected.append(row)
    if not finished:
        raise ValueError("Cargo prebuild stream has no successful build-finished message")
    if len(selected) != 1:
        raise ValueError("Cargo prebuild must identify exactly one scale test for this package/checkout")
    row = selected[0]
    path = validate_path(row["executable"], target)
    # This helper's documented prebuild is --release, not a debug test invocation.
    if path.parent != (Path(target).resolve() / "release/deps"):
        raise ValueError("Cargo scale test is not a release test artifact")
    return path, row


def describe(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
        stat = os.fstat(source.fileno())
    return dict(path=str(path), sha256=digest.hexdigest(), mtime_unix_ns=stat.st_mtime_ns)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("select", "validate"))
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--target-dir", type=Path, required=True)
    parser.add_argument("--executable", type=Path)
    parser.add_argument("--record", type=Path)
    args = parser.parse_args()
    try:
        if args.mode == "select":
            rows = (json.loads(line) for line in sys.stdin if line.strip())
            path, row = select_artifact(rows, args.manifest, args.target_dir)
            selection = dict(method="cargo compiler-artifact", package_id=row.get("package_id"),
                             cargo_reported_fresh=row.get("fresh"))
        else:
            if args.executable is None:
                raise ValueError("no explicit scale test path")
            path = validate_path(args.executable, args.target_dir)
            selection = dict(method="explicit caller path")
        record = {"kind": "vqro.scale-test-artifact", "version": 1,
                  "assurance": "caller-attested, not revision-bound",
                  "manifest": str(args.manifest.resolve()), "selection": selection, **describe(path)}
        if args.record:
            args.record.parent.mkdir(parents=True, exist_ok=True)
            args.record.write_text(json.dumps(record, indent=1) + "\n")
        print(path)
        return 0
    except (OSError, ValueError, TypeError, KeyError) as error:
        print(f"FAIL scale artifact: {error}; {PREBUILD}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    sys.exit(main())
