"""Portable test-only Python selection. Safe to import, including under Python 3.9."""
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time

PROBE_SECONDS = 3
RESOLVE_SECONDS = 10
WINDOWS = os.name == "nt"


def candidates():
    if "VQRO_PYTHON" in os.environ:
        value = os.environ["VQRO_PYTHON"]
        if os.path.dirname(value):
            yield value  # A literal relative or absolute path is intentional.
        else:
            selected = shutil.which(value) if value else None
            if selected is None:
                raise RuntimeError("VQRO_PYTHON name was not found on PATH")
            yield selected
        return
    names = ("python3.exe", "python.exe") if WINDOWS else ("python3",)
    for directory in os.environ.get("PATH", "").split(os.pathsep):
        if WINDOWS and Path(directory).name.lower() == "windowsapps":
            continue  # Store aliases must not even be inspected.
        for name in names:
            yield str(Path(directory or os.curdir) / name)
    # Native Windows Python installations need not expose python3 on PATH.
    if WINDOWS and Path(sys.executable).parent.name.lower() != "windowsapps":
        yield sys.executable


def resolve():
    deadline = time.monotonic() + RESOLVE_SECONDS
    for candidate in candidates():
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            break
        if not candidate or not os.path.isfile(candidate) or not os.access(candidate, os.X_OK):
            continue
        try:
            result = subprocess.run(
                [candidate, "-I", "-c", "import sys; sys.exit(sys.version_info < (3, 11))"],
                stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                timeout=min(PROBE_SECONDS, remaining),
            )
        except (OSError, subprocess.TimeoutExpired):
            continue  # subprocess.run kills and reaps its owned child on timeout.
        if result.returncode == 0:
            return os.path.abspath(candidate)
    if "VQRO_PYTHON" in os.environ:
        raise RuntimeError("VQRO_PYTHON must name an executable Python >= 3.11 (probe timeout is 3s)")
    raise RuntimeError("need Python >= 3.11 on PATH, or set VQRO_PYTHON (probe timeout is 3s)")


if __name__ == "__main__":
    try:
        print(resolve())
    except RuntimeError as error:
        print("tests: " + str(error), file=sys.stderr)
        raise SystemExit(2)
