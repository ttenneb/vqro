"""Test-only interpreter selection, shared with Rust fixtures and check.sh."""
from pathlib import Path
import subprocess

PYTHON = subprocess.check_output(
    ["/bin/sh", str(Path(__file__).resolve().parents[1] / "scripts/test-python.sh")],
    text=True,
).strip()
