#!/usr/bin/env python3
"""No-model launcher witness: record only non-secret profile/identity metadata, then exec fake Codex."""
import argparse
import json
import os
from pathlib import Path
import sys

parser = argparse.ArgumentParser()
parser.add_argument('--observe', type=Path, required=True)
parser.add_argument('--ledger', type=Path, required=True)
args = parser.parse_args()
args.observe.write_text(json.dumps({'pid': os.getpid(), 'home': os.environ.get('HOME'),
    'codex_home': os.environ.get('CODEX_HOME'),
    'bridge_token_present': 'VQRO_BRIDGE_TOKEN' in os.environ,
    'principal_present': 'VQRO_PRINCIPAL' in os.environ,
    'herdr_present': any(key.startswith('HERDR_') for key in os.environ)}) + '\n')
os.execv(sys.executable, [sys.executable, '-I', str(Path(__file__).with_name('fake_codex.py')), '--ledger', str(args.ledger)])
