#!/usr/bin/env python3
"""Synthetic ready/exec helper. No keychain, native CLI, network, token or profile access."""
import argparse
import json
import os
import sys

parser=argparse.ArgumentParser()
parser.add_argument('action', choices=['check','launch'])
parser.add_argument('--handle', required=True)
parser.add_argument('--provider', choices=['anthropic'], required=True)
parser.add_argument('--kind', choices=['claude_subscription'], required=True)
# Split the literal runtime argv before parsing: helper options are never runtime arguments.
raw=sys.argv[1:]
if '--' in raw:
    at=raw.index('--');runtime=raw[at+1:];raw=raw[:at]
else:
    runtime=[]
args=parser.parse_args(raw)
log=os.environ.get('FAKE_RESTART_LOG')
if log:
    with open(log,'a',encoding='utf-8') as stream:
        stream.write(json.dumps({'action':args.action,'handle':args.handle,'runtime':runtime,'pid':os.getpid()})+'\n')
if args.action=='check':
    if os.environ.get('FAKE_RESTART_PREFLIGHT')=='deny':
        print(json.dumps({'status':'denied'}),file=sys.stderr);sys.exit(1)
    if os.environ.get('FAKE_RESTART_PREFLIGHT')=='oversized':
        print('x'*2048);sys.exit(0)
    print(json.dumps({'status':'ready'}));sys.exit(0)
assert runtime
resumed='--resume' in runtime
if resumed:
    assert runtime[-2]=='--resume' and runtime[-1]
else:
    assert os.environ.get('FAKE_RESTART_ALLOW_INITIAL')=='1', 'initial helper launch requires explicit fixture opt-in'
if os.environ.get('FAKE_RESTART_LAUNCH')=='refuse':
    sys.exit(1)
environment=dict(os.environ)
if resumed:
    environment['FAKE_CLAUDE_INIT_SESSION']=environment.get('FAKE_RESTART_SESSION','same')
os.execvpe(runtime[0],runtime,environment)
