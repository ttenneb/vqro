#!/usr/bin/env python3
"""Run an explicit Linux test command and require zero new fixed-registry records.

This checker never deletes records. New files are listed with their contents for
attribution; a test command's exit failure is preserved independently.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys


def snapshot(root):
    if not root.exists():
        return {}
    return {path.name: path.read_text() for path in root.glob('*.json')}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--receipt', type=Path, required=True)
    parser.add_argument('command', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ['--'] else args.command
    if not command:
        parser.error('an explicit test command is required')
    root = Path(f'/run/user/{os.geteuid()}/vqro/pty-owners-v1')
    before = snapshot(root)
    result = subprocess.run(command)
    after = snapshot(root)
    added = sorted(set(after)-set(before))
    receipt = {'registry': str(root), 'command': command, 'command_exit': result.returncode,
               'before_records': sorted(before), 'after_records': sorted(after),
               'new_records': {name: json.loads(after[name]) for name in added},
               'zero_new_records': not added}
    args.receipt.write_text(json.dumps(receipt, indent=2)+'\n')
    print(json.dumps({'command_exit': result.returncode, 'new_record_count': len(added), 'receipt': str(args.receipt)}))
    return result.returncode or (1 if added else 0)


if __name__ == '__main__':
    sys.exit(main())
