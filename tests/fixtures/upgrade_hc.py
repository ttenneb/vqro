#!/usr/bin/python3
"""Persistent virtual hc, only for the in-process -> installed-provider upgrade test."""
import json
from pathlib import Path
import struct
import sys
root=Path(sys.argv[0]).parent
state=root/'backend.json'
records=json.loads(state.read_text()) if state.exists() else [{'address':'t1','created_ms':42,'state':'live','argv':['sh'],'cols':80,'rows':24}]
def exact(n):
    out=b''
    while len(out)<n:
        part=sys.stdin.buffer.read(n-len(out))
        if not part: raise EOFError
        out+=part
    return out
json.loads(sys.stdin.buffer.readline())
sys.stdout.buffer.write(b'{"jsonrpc":"2.0","id":0,"result":{"framed":true,"core":{"id":"upgrade-fixture"},"version":"fixture"}}\n');sys.stdout.buffer.flush()
try:
    while True:
        channel,kind,size=struct.unpack('<IBI',exact(9));message=json.loads(exact(size))
        if message['method']=='events.subscribe': result={'channel':1,'terminals':records}
        elif message['method']=='terminal.run':
            result={'address':f't{len(records)+1}','created_ms':42+len(records),'state':'live','argv':message['params'].get('argv',['sh']),'cols':80,'rows':24}
            records.append(result);state.write_text(json.dumps(records))
        else: raise AssertionError(message)
        wire=json.dumps({'jsonrpc':'2.0','id':message['id'],'result':result}).encode()
        sys.stdout.buffer.write(struct.pack('<IBI',0,0,len(wire))+wire);sys.stdout.buffer.flush()
except EOFError: pass
