#!/usr/bin/env python3
"""Read-only, best-effort stale-layout warnings before a developer install."""
import datetime
import os
from pathlib import Path
import re
import stat
import subprocess
import sys


def warnings(binary):
    home = Path.home()
    state = Path(os.environ.get("VQRO_STATE_DIR", Path(os.environ.get("XDG_STATE_HOME", home / ".local/state")) / "vqro"))
    found = []
    for name in ("client-shell", "plugins"):
        path = state / name
        if path.is_dir():
            found.append(f"old-layout state directory: {path}")
    legacy = home / ".config/vqro"
    if legacy.is_dir():
        for path in legacy.rglob("*"):
            try:
                if stat.S_ISSOCK(path.lstat().st_mode):
                    found.append(f"old-layout socket (possibly stale): {path}")
            except OSError:
                pass
    try:
        # BSD/macOS and Linux ps both support these fields; no /proc dependency.
        processes = subprocess.run(["ps", "-axo", "pid=,lstart=,args="], capture_output=True,
                                   text=True, timeout=3, check=True).stdout.splitlines()
    except (OSError, subprocess.SubprocessError):
        found.append("could not inspect process command lines with ps; check for old cores manually")
        processes = []
    try:
        current_pid = int((state / "serve.pid").read_text().strip())
    except (OSError, ValueError):
        current_pid = None
    for line in processes:
        fields = line.split(None, 6)
        if len(fields) != 7:
            continue
        pid, *_, command = fields
        if re.match(r"(?:\S*/)?prim\s+serve(?:\s|$)", command) or re.match(r"(?:\S*/)?vqro\s+server(?:\s|$)", command):
            found.append(f"legacy core process {pid}: {command}")
        elif re.match(r"(?:\S*/)?(?:prim|vqro)\s+serve(?:\s|$)", command) and str(legacy) in command:
            found.append(f"old-layout core process {pid}: {command}")
        if current_pid is not None and pid == str(current_pid) and re.match(r"(?:\S*/)?vqro\s+serve(?:\s|$)", command):
            try:
                started = datetime.datetime.strptime(" ".join(fields[1:6]), "%a %b %d %H:%M:%S %Y").timestamp()
                if started < Path(binary).stat().st_mtime:
                    found.append(f"current-endpoint core {pid} predates the new binary; it may be an older build")
            except (OSError, ValueError):
                found.append(f"current-endpoint core {pid}: build age could not be checked")
    return found


def main():
    try:
        found = warnings(sys.argv[1])
    except OSError as error:
        found = [f"stale-install inspection incomplete: {error}"]
    if found:
        print("dev-install: WARNING (read-only preflight; nothing stopped or moved):", file=sys.stderr)
        for notice in found:
            print(f"  - {notice}", file=sys.stderr)
        print("Review scripts/migrate-from-prim.py --help before a pre-rename migration. Close old UIs/launchers and stop the old core yourself first. Migration refuses an existing destination (including old-layout vqro state), an active/unavailable state lock, symlinks or unsupported source files. Back up and resolve conflicts manually; this installer does not migrate them.", file=sys.stderr)


if __name__ == "__main__":
    main()
