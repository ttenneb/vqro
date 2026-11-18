"""Installed bridge entry point: run with python -I launch.py, never from a project's import path."""
from pathlib import Path
import sys

# Isolated interpreter startup excludes cwd, PYTHONPATH and user site packages before any
# bridge imports. Add only this trusted installation; do not alter the process environment.
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from pi_bridge.__main__ import main

if __name__ == "__main__":
    main()
