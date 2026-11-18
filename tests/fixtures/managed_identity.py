#!/usr/bin/python3
"""Private test-only holder of a real supervisor lifetime; exports its disposable token to its fixture."""
import os
from pathlib import Path
import sys
Path(sys.argv[1]).write_text(os.environ['PRIM_EXTENSION_TOKEN'])
# This process belongs to its test core; no core sockets or external services are opened here.
sys.stdin.buffer.read()
