"""Private finite-window event CREDIT fixture. Never invokes hc."""
import json, struct, sys
from pathlib import Path
inp,out=sys.stdin.buffer,sys.stdout.buffer
root=Path(sys.argv[1])
def exact(n):
    data=b''
    while len(data)<n:
        part=inp.read(n-len(data))
        if not part: raise EOFError
        data+=part
    return data
def send(channel,kind,payload):
    out.write(struct.pack('<IBI',channel,kind,len(payload))+payload);out.flush()
def event(kind,data):
    return json.dumps({'method':'event','params':{'terminal':'t1','type':kind,'data':data}}).encode()+b'\n'
json.loads(inp.readline())
out.write(b'{"jsonrpc":"2.0","id":0,"result":{"framed":true,"core":{"id":"event-credit-fixture"},"version":"fixture"}}\n');out.flush()
channel,kind,size=struct.unpack('<IBI',exact(9));request=json.loads(exact(size))
assert channel==0 and request['method']=='events.subscribe'
send(0,0,json.dumps({'jsonrpc':'2.0','id':request['id'],'result':{'channel':1,'terminals':[{'address':'t1','created_ms':7,'state':'live','argv':['sh']}]}}).encode())
records=[event('title',{'title':'one'}),event('title',{'title':'two'}),event('title',{'title':'tri'}),event('finished',{'reason':'exited','exit_code':0})]
window=max(map(len,records))
try:
    for i,record in enumerate(records):
        while window<len(record):
            channel,kind,size=struct.unpack('<IBI',exact(9));payload=exact(size)
            if channel==1 and kind==1:
                credit,=struct.unpack('<I',payload);window+=credit
                with (root/'credits').open('a') as log:log.write(str(credit)+'\n')
        window-=len(record);send(1,0,record)
        if i==0:(root/'first-sent').write_text('yes')
    (root/'finished-sent').write_text('yes')
    while inp.read(1):pass
except EOFError:pass
