#!/usr/bin/env python3
"""Copy declared bundled unit suites; no build, core, or live-install assumptions."""
import argparse
import json
from pathlib import Path
import shutil


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


def install_tests(root, destination):
    mapping = test_files(root)
    for name, files in mapping.items():
        directory = destination / "tests" / name
        if directory.exists():
            shutil.rmtree(directory)
        directory.mkdir(parents=True)
        for file in files:
            shutil.copyfile(root / file, directory / Path(file).name)
    shutil.copyfile(root / "extensions/tests.json", destination / "tests.json")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", type=Path, help="installed extensions directory")
    args = parser.parse_args()
    install_tests(ROOT, args.destination)


if __name__ == "__main__":
    main()
