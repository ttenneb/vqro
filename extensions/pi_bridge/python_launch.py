"""Explicit legacy Python bridge; run with python -I python_launch.py, never a project import path."""
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
from pi_bridge.__main__ import main

if __name__ == "__main__":
    main()
