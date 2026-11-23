#!/usr/bin/env python3
"""Copy declared bundled unit suites; no build, core, or live-install assumptions."""
import argparse
import json
from pathlib import Path
import shutil
import tempfile


ROOT = Path(__file__).resolve().parents[1]


def test_files(root):
    """Validate the entire mapping before replacing any installed suite."""
    mapping = json.loads((root / "extensions/tests.json").read_text())["suites"]
    names = {entry["name"] for entry in json.loads((root / "extensions/defaults.json").read_text())["extensions"]}
    for name, files in mapping.items():
        if name not in names or Path(name).name != name:
            raise ValueError(f"not a bundled extension name: {name}")
        if not files or not any(Path(file).name.startswith("test_") for file in files):
            raise ValueError(f"no tests for {name}")
        if len({Path(file).name for file in files}) != len(files):
            raise ValueError(f"duplicate destination filename for {name}")
        for file in files:
            path = Path(file)
            if path.parent != Path("tests") or path.suffix != ".py" or not (root / path).is_file():
                raise ValueError(f"not a test source: {file}")
    return mapping


def check_destination(destination, names):
    """Check existing parents/leaves without following suite symlinks or writing."""
    parent = destination / "tests"
    paths = [parent, *(parent / name for name in names)]
    if parent.is_dir() and not parent.is_symlink():
        paths.extend(path for path in parent.iterdir() if path.is_symlink())
    for path in paths:
        if path.is_symlink() or not path.resolve().is_relative_to(destination):
            raise ValueError(f"unsafe test destination: {path}")
        if path.exists() and not path.is_dir():
            raise ValueError(f"not a suite directory: {path}")


def preflight(root, destination):
    destination = destination.resolve()
    mapping = test_files(root)
    check_destination(destination, mapping)
    return destination, mapping


def install_tests(root, destination, *, check=False):
    destination, mapping = preflight(root, destination)
    if check:
        return
    parent = destination / "tests"
    parent.mkdir(parents=True, exist_ok=True)
    for name, files in mapping.items():
        directory = parent / name
        # Prepare the entire replacement before moving the old suite. The temporary
        # sibling lives on the same filesystem, so publication uses rename, not copy.
        temporary = Path(tempfile.mkdtemp(prefix=f".{name}-", dir=parent))
        previous = temporary / "previous"
        try:
            staging = temporary / "suite"
            staging.mkdir()
            for file in files:
                shutil.copyfile(root / file, staging / Path(file).name)
            check_destination(destination, mapping)
            if directory.exists():
                directory.rename(previous)
            try:
                staging.rename(directory)
            except OSError:
                if previous.exists():
                    previous.rename(directory)
                raise
        finally:
            # If even rollback fails, retain the previous suite for manual recovery.
            if directory.exists() or not previous.exists():
                check_destination(destination, mapping)
                shutil.rmtree(temporary)
    # Replace the mapping itself instead of following an existing tests.json symlink.
    with tempfile.TemporaryDirectory(prefix=".tests-map-", dir=destination) as temporary:
        staged_mapping = Path(temporary) / "tests.json"
        shutil.copyfile(root / "extensions/tests.json", staged_mapping)
        staged_mapping.replace(destination / "tests.json")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", type=Path, help="installed extensions directory")
    parser.add_argument("--check", action="store_true", help="validate sources and destination; write nothing")
    args = parser.parse_args()
    install_tests(ROOT, args.destination, check=args.check)


if __name__ == "__main__":
    main()
